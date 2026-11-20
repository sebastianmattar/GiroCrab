//! Small secret wrapper: the bytes are wiped (zeroize) when dropped and never
//! printed by `Debug`.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Serialises as the plain string, for connector state that is persisted in the
/// encrypted vault.
#[derive(Clone, Default, Zeroize, ZeroizeOnDrop, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(s: String) -> Self {
        SecretString(s)
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretString(***)")
    }
}

impl From<String> for SecretString {
    fn from(s: String) -> Self {
        SecretString(s)
    }
}

impl From<&str> for SecretString {
    fn from(s: &str) -> Self {
        SecretString(s.to_string())
    }
}

/// JSON text held as a [`SecretString`]: connector state carries session tokens
/// and keys, and a sync keeps (and clones) it for the whole session. Decode
/// into structs with `SecretString` fields; a plain `Value` is only wiped by
/// [`with_value`](Self::with_value) or [`wipe`]. Reads from and binds to SQL as text.
#[derive(Clone, Debug, Default)]
pub struct SecretJson(SecretString);

impl SecretJson {
    /// Takes stored JSON text as is; invalid text fails in [`decode`](Self::decode).
    pub fn from_text(text: String) -> Self {
        SecretJson(SecretString::new(text))
    }

    pub fn encode<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<Self> {
        // Measured first so the buffer never grows: every reallocation would
        // leave a partial copy behind unwiped.
        let mut len = ByteCount(0);
        serde_json::to_writer(&mut len, value)?;
        let mut buf = Vec::with_capacity(len.0);
        serde_json::to_writer(&mut buf, value)?;
        String::from_utf8(buf)
            .map(Self::from_text)
            .map_err(serde::ser::Error::custom)
    }

    /// No state stored yet: empty text, `{}` or `null`.
    pub fn is_empty(&self) -> bool {
        matches!(self.expose().trim(), "" | "{}" | "null")
    }

    pub fn decode<T: DeserializeOwned>(&self) -> serde_json::Result<T> {
        serde_json::from_str(self.expose())
    }

    /// Untyped access for code that handles any connector's state by key. `f`
    /// gets an empty object when the text is empty or invalid; every string
    /// still in the decoded copy is wiped afterwards (values `f` takes out need
    /// their own [`wipe`]).
    pub fn with_value<R>(&self, f: impl FnOnce(&mut Value) -> R) -> R {
        let mut value = self
            .decode()
            .unwrap_or_else(|_| Value::Object(Default::default()));
        let result = f(&mut value);
        wipe(&mut value);
        result
    }

    fn expose(&self) -> &str {
        self.0.expose()
    }
}

#[cfg(test)]
impl From<Value> for SecretJson {
    fn from(value: Value) -> Self {
        Self::encode(&value).unwrap()
    }
}

impl rusqlite::types::FromSql for SecretJson {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        String::column_result(value).map(Self::from_text)
    }
}

impl rusqlite::ToSql for SecretJson {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.expose().to_sql()
    }
}

/// Zeroizes every string in `value`; keys stay.
pub(crate) fn wipe(value: &mut Value) {
    match value {
        Value::String(s) => s.zeroize(),
        Value::Array(items) => items.iter_mut().for_each(wipe),
        Value::Object(map) => map.values_mut().for_each(wipe),
        _ => {}
    }
}

/// `io::Write` sink that only counts bytes.
struct ByteCount(usize);

impl std::io::Write for ByteCount {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Default, Zeroize, ZeroizeOnDrop)]
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    pub fn new(b: Vec<u8>) -> Self {
        SecretBytes(b)
    }
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn secret_json_round_trips_and_hides_its_text() {
        let state = SecretJson::encode(&json!({"token": "t0k3n", "n": [1]})).unwrap();
        assert_eq!(state.expose(), r#"{"n":[1],"token":"t0k3n"}"#);
        assert_eq!(state.decode::<Value>().unwrap()["token"], "t0k3n");
        assert!(!format!("{:?}", Some(state)).contains("t0k3n"));
        for empty in ["", "{}", " null "] {
            assert!(SecretJson::from_text(empty.into()).is_empty());
        }
        assert!(!SecretJson::from_text(r#"{"a":1}"#.into()).is_empty());
    }

    #[test]
    fn with_value_starts_from_an_object_and_wipes_strings() {
        for text in ["", "{oops"] {
            let state = SecretJson::from_text(text.into());
            assert!(state.with_value(|v| v.is_object()));
        }
        let mut value = json!({"a": ["t0k3n", {"b": "x"}], "n": 1});
        wipe(&mut value);
        assert_eq!(value, json!({"a": ["", {"b": ""}], "n": 1}));
    }

    #[test]
    fn secret_json_reads_and_binds_as_sql_text() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        let state = SecretJson::from_text(r#"{"k":"v"}"#.into());
        let back: SecretJson = db.query_row("SELECT ?1", [&state], |r| r.get(0)).unwrap();
        assert_eq!(back.expose(), state.expose());
    }
}
