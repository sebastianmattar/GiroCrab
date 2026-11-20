//! Which FinTS product registration goes to the bank.
//!
//! Two sources exist:
//! * **builtin** – GiroCrab's own registration with the Deutsche
//!   Kreditwirtschaft, embedded only by the release build script (see
//!   `build.rs`); absent in developer and fork builds.
//! * **own** – the user's own registration (setting `fints.product_id`). It is
//!   never deleted when the user switches to the built-in one.
//!
//! The choice is stored in `fints.product_id_mode` (`builtin`, `own` or empty =
//! not decided yet). The ID itself must never reach logs, errors or the UI.

/// Setting key of the user's choice.
pub const MODE_KEY: &str = "fints.product_id_mode";
/// Setting key of the user's own registration.
pub const OWN_KEY: &str = "fints.product_id";

include!(concat!(env!("OUT_DIR"), "/builtin_product_id.rs"));

const MASK: &[u8] = b"girocrab-fints-product-id-mask";

/// Which registration is (or would be) used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Builtin,
    Own,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Builtin => "builtin",
            Source::Own => "own",
        }
    }
}

/// The embedded ID, if this build carries one.
pub(crate) fn builtin() -> Option<String> {
    if BUILTIN_MASKED.is_empty() {
        return None;
    }
    let bytes: Vec<u8> = BUILTIN_MASKED
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ MASK[i % MASK.len()] ^ (i as u8))
        .collect();
    String::from_utf8(bytes).ok().filter(|s| !s.is_empty())
}

/// True when this build carries GiroCrab's own registration.
pub fn builtin_available() -> bool {
    !BUILTIN_MASKED.is_empty()
}

/// Normalises the stored mode; anything unknown counts as "not decided".
pub fn normalize_mode(mode: &str) -> &'static str {
    match mode.trim() {
        "builtin" => "builtin",
        "own" => "own",
        _ => "",
    }
}

fn pick(mode: &str, own: &str, builtin: Option<String>) -> Option<(Source, String)> {
    let _ = mode;
    if let Some(b) = builtin {
        // Official builds with an embedded registration always use it (#1092).
        return Some((Source::Builtin, b));
    }
    let own = own.trim();
    (!own.is_empty()).then(|| (Source::Own, own.to_string()))
}

/// Developer override: `GIROCRAB_FINTS_PRODUCT_ID_OVERRIDE` at run time acts as
/// the user's own ID and forces mode `own`. Debug builds only, so a release
/// can never be redirected by the environment.
fn dev_override() -> Option<String> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var("GIROCRAB_FINTS_PRODUCT_ID_OVERRIDE")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Resolves the registration to send: user's choice first, then what exists.
pub fn resolve(mode: &str, own: &str) -> Option<(Source, String)> {
    if let Some(v) = dev_override() {
        return Some((Source::Own, v));
    }
    pick(mode, own, builtin())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b() -> Option<String> {
        Some("BUILTIN".into())
    }

    #[test]
    fn builtin_always_wins_when_embedded() {
        for mode in ["", "builtin", "own", "garbage"] {
            for own in ["", "  ", "OWN"] {
                assert_eq!(
                    pick(mode, own, b()),
                    Some((Source::Builtin, "BUILTIN".into()))
                );
            }
        }
    }

    #[test]
    fn custom_builds_without_builtin_use_own_id() {
        for mode in ["", "builtin", "own", "garbage"] {
            assert_eq!(pick(mode, "OWN", None), Some((Source::Own, "OWN".into())));
            assert_eq!(pick(mode, "  ", None), None);
        }
    }

    #[test]
    fn vault_keeps_own_id_when_switching_mode() {
        use crate::Vault;
        let dir = crate::test_support::TempDir::new("girocrab-pid");
        let v = Vault::open(
            dir.join("v.db").to_string_lossy().to_string(),
            vec![7u8; 32],
        )
        .unwrap();
        assert_eq!(v.fints_product_id_mode().unwrap(), "");
        let none = if builtin_available() { "builtin" } else { "" };
        assert_eq!(v.fints_product_id_source().unwrap(), none);
        v.set_setting(OWN_KEY.into(), "OWNID".into()).unwrap();
        let expect = if builtin_available() {
            "builtin"
        } else {
            "own"
        };
        assert_eq!(v.fints_product_id_source().unwrap(), expect);
        v.set_fints_product_id_mode("builtin".into()).unwrap();
        assert_eq!(v.fints_product_id_mode().unwrap(), "builtin");
        assert_eq!(
            v.get_setting(OWN_KEY.into()).unwrap().as_deref(),
            Some("OWNID")
        );
        v.set_fints_product_id_mode("nonsense".into()).unwrap();
        assert_eq!(v.fints_product_id_mode().unwrap(), "");
    }
}
