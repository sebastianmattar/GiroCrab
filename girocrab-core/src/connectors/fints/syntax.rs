//! FinTS 3.0 "Formals" syntax: segments, data elements, groups, escaping and
//! binary data, implemented from the public DK specification.
//!
//!   segment   := header ('+' element)* '\''
//!   element   := component (':' component)*
//!   escape    := '?' before any of  + : ' ? @
//!   binary    := '@' length '@' bytes
//!
//! Text is ISO 8859-1 on the wire.

use crate::error::{CoreError, CoreResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Text(String),
    Binary(Vec<u8>),
}

impl Value {
    pub fn text(&self) -> &str {
        match self {
            Value::Text(s) => s,
            Value::Binary(_) => "",
        }
    }
}

/// One parsed segment: elements[i][j] = component j of data element i.
/// elements[0] is the header `Kennung:Nummer:Version[:Bezug]`.
#[derive(Debug, Clone)]
pub struct Segment {
    pub elements: Vec<Vec<Value>>,
}

impl Segment {
    pub fn id(&self) -> &str {
        self.get(0, 0)
    }
    pub fn number(&self) -> u32 {
        self.get(0, 1).parse().unwrap_or(0)
    }
    pub fn version(&self) -> u32 {
        self.get(0, 2).parse().unwrap_or(0)
    }
    pub fn reference(&self) -> Option<u32> {
        self.get(0, 3).parse().ok()
    }
    /// Text of component `c` of element `e` ("" if absent).
    pub fn get(&self, e: usize, c: usize) -> &str {
        self.elements
            .get(e)
            .and_then(|el| el.get(c))
            .map(|v| v.text())
            .unwrap_or("")
    }
    pub fn element(&self, e: usize) -> &[Value] {
        self.elements.get(e).map(|v| v.as_slice()).unwrap_or(&[])
    }
    pub fn binary(&self, e: usize) -> Option<&[u8]> {
        match self.elements.get(e).and_then(|el| el.first()) {
            Some(Value::Binary(b)) => Some(b),
            _ => None,
        }
    }
}

/// Escapes a text value for the wire.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '+' | ':' | '\'' | '?' | '@') {
            out.push('?');
        }
        out.push(ch);
    }
    out
}

/// UTF-8 -> ISO 8859-1. The input is already escaped, so a bare `?` would be read
/// as the escape character of the next byte and corrupt the segment framing:
/// unmappable characters become the escaped literal `??`.
pub fn to_latin1(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        if (c as u32) < 256 {
            out.push(c as u8);
        } else {
            out.extend_from_slice(b"??");
        }
    }
    out
}

/// ISO 8859-1 -> UTF-8.
pub fn from_latin1(b: &[u8]) -> String {
    crate::text::decode_bank_bytes(b)
}

/// Builder for an outgoing segment. Components are escaped automatically
/// unless added with `raw`.
pub struct SegmentBuilder {
    parts: Vec<u8>,
}

impl SegmentBuilder {
    pub fn new(id: &str, number: u32, version: u32) -> Self {
        SegmentBuilder {
            parts: format!("{id}:{number}:{version}").into_bytes(),
        }
    }
    pub fn with_ref(id: &str, number: u32, version: u32, reference: u32) -> Self {
        SegmentBuilder {
            parts: format!("{id}:{number}:{version}:{reference}").into_bytes(),
        }
    }
    /// Adds a data element made of escaped components (joined with ':').
    pub fn de(mut self, components: &[&str]) -> Self {
        self.parts.push(b'+');
        let joined = components
            .iter()
            .map(|c| escape(c))
            .collect::<Vec<_>>()
            .join(":");
        self.parts.extend(to_latin1(&joined));
        self
    }
    /// Adds a data element verbatim (already escaped / structured).
    pub fn raw(mut self, raw: &str) -> Self {
        self.parts.push(b'+');
        self.parts.extend(to_latin1(raw));
        self
    }
    /// Adds a binary data element `@len@bytes`.
    pub fn binary(mut self, data: &[u8]) -> Self {
        self.parts.push(b'+');
        self.parts.extend(format!("@{}@", data.len()).into_bytes());
        self.parts.extend_from_slice(data);
        self
    }
    pub fn build(mut self) -> Vec<u8> {
        // Trailing empty elements must be dropped.
        while self.parts.last() == Some(&b'+') {
            self.parts.pop();
        }
        self.parts.push(b'\'');
        self.parts
    }
}

/// Parses a FinTS byte stream into segments.
pub fn parse(data: &[u8]) -> CoreResult<Vec<Segment>> {
    let mut segments = Vec::new();
    let mut elements: Vec<Vec<Value>> = Vec::new();
    let mut components: Vec<Value> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut cur_binary: Option<Vec<u8>> = None;
    let mut i = 0;
    let flush_comp =
        |cur: &mut Vec<u8>, cur_binary: &mut Option<Vec<u8>>, components: &mut Vec<Value>| {
            if let Some(b) = cur_binary.take() {
                components.push(Value::Binary(b));
            } else {
                // Latin-1 envelope, but some banks send UTF-8 text: decode per component.
                components.push(Value::Text(crate::text::decode_bank_bytes(cur)));
            }
            cur.clear();
        };
    while i < data.len() {
        let c = data[i];
        match c {
            b'?' => {
                if i + 1 < data.len() {
                    cur.push(data[i + 1]);
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            b'@' if cur.is_empty() && cur_binary.is_none() => {
                // binary: @len@
                let rest = &data[i + 1..];
                let end = rest
                    .iter()
                    .position(|&b| b == b'@')
                    .ok_or_else(|| CoreError::protocol("unterminated binary length"))?;
                let len: usize = std::str::from_utf8(&rest[..end])
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| CoreError::protocol("bad binary length"))?;
                let start = i + 1 + end + 1;
                let Some(stop) = start.checked_add(len).filter(|s| *s <= data.len()) else {
                    return Err(CoreError::protocol("binary data exceeds message"));
                };
                cur_binary = Some(data[start..stop].to_vec());
                i = stop;
                continue;
            }
            b':' => {
                flush_comp(&mut cur, &mut cur_binary, &mut components);
            }
            b'+' => {
                flush_comp(&mut cur, &mut cur_binary, &mut components);
                elements.push(std::mem::take(&mut components));
            }
            b'\'' => {
                flush_comp(&mut cur, &mut cur_binary, &mut components);
                elements.push(std::mem::take(&mut components));
                segments.push(Segment {
                    elements: std::mem::take(&mut elements),
                });
            }
            b'\r' | b'\n' if cur.is_empty() && components.is_empty() && elements.is_empty() => {}
            _ => cur.push(c),
        }
        i += 1;
    }
    Ok(segments)
}

/// Parses a response and transparently unwraps the HNVSD envelope.
pub fn parse_response(data: &[u8]) -> CoreResult<Vec<Segment>> {
    let outer = parse(data)?;
    let mut out = Vec::new();
    for seg in outer {
        if seg.id() == "HNVSD" {
            if let Some(inner) = seg.binary(1) {
                out.extend(parse(inner)?);
            }
        } else {
            out.push(seg);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn unmappable_characters_keep_the_framing() {
        // "€" is not in ISO 8859-1: it must not become a bare escape character
        assert_eq!(to_latin1("a€b"), b"a??b".to_vec());
        let seg = to_latin1(&escape("Preis 5€ + Porto"));
        // the '+' is escaped, the '€' is a literal '?'
        assert_eq!(seg, b"Preis 5?? ?+ Porto".to_vec());
    }

    use super::*;

    #[test]
    fn roundtrip_escape_and_binary() {
        let seg = SegmentBuilder::new("HKTST", 3, 1)
            .de(&["a+b", "c:d"])
            .binary(b"x'y")
            .de(&["z?"])
            .build();
        assert_eq!(seg, b"HKTST:3:1+a?+b:c?:d+@3@x'y+z??'".to_vec());
        let parsed = parse(&seg).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id(), "HKTST");
        assert_eq!(parsed[0].get(1, 0), "a+b");
        assert_eq!(parsed[0].get(1, 1), "c:d");
        assert_eq!(parsed[0].binary(2), Some(&b"x'y"[..]));
        assert_eq!(parsed[0].get(3, 0), "z?");
    }

    #[test]
    fn parses_response_codes() {
        let msg = b"HNHBK:1:3+000000000120+300+abc+1+abc:1'HIRMG:2:2+0010::Nachricht entgegengenommen.'HIRMS:3:2:4+3920::Zugelassene TAN-Verfahren:942:946+0020::OK'HNHBS:4:1+1'";
        let segs = parse_response(msg).unwrap();
        assert_eq!(segs.len(), 4);
        assert_eq!(segs[2].reference(), Some(4));
        assert_eq!(segs[2].get(1, 0), "3920");
        assert_eq!(segs[2].get(1, 3), "942");
        assert_eq!(segs[2].get(2, 0), "0020");
    }
}

#[cfg(test)]
mod binary_length_tests {
    #[test]
    fn huge_binary_length_is_an_error_not_a_panic() {
        assert!(super::parse(b"HNHBK:1:3+@18446744073709551615@x'").is_err());
        assert!(super::parse(b"HNHBK:1:3+@99@x'").is_err());
    }
}
