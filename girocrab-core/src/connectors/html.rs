//! Helpers for connectors that read web pages instead of an API: regex-based
//! access to tags, attributes, tables and forms, JSF partial responses, and
//! the German amounts and dates those pages show. Good enough for the known,
//! simple pages; there is no HTML parser. Attribute values and [`text`] come
//! back with HTML entities decoded.

use std::sync::LazyLock;

use regex::Regex;

use crate::dates::{find_de_date, iso};

pub(super) static FORM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<form\b([^>]*)>(.*?)</form\s*>").unwrap());
pub(super) static INPUT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<input\b([^>]*)>").unwrap());
/// A link: attributes, inner HTML.
pub(super) static LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<a\b([^>]*)>(.*?)</a\s*>").unwrap());

// ------------------------------------------------------------------ tags and text

static TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<([a-zA-Z][a-zA-Z0-9]*)\b([^>]*)>").unwrap());
static ATTR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)([A-Za-z_:][-A-Za-z0-9_:.]*)\s*(?:=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+)))?"#)
        .unwrap()
});
static ANY_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").unwrap());

/// Attributes of a tag body (`name="x" type=hidden checked`), names lower-case.
pub(super) fn attrs(body: &str) -> Vec<(String, String)> {
    ATTR.captures_iter(body)
        .map(|c| {
            let v = c
                .get(2)
                .or(c.get(3))
                .or(c.get(4))
                .map(|m| crate::text::decode_html_entities(m.as_str()))
                .unwrap_or_default();
            (c[1].to_ascii_lowercase(), v)
        })
        .collect()
}

pub(super) fn attr<'a>(a: &'a [(String, String)], name: &str) -> Option<&'a str> {
    a.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// Visible text of an HTML fragment, whitespace collapsed.
pub(super) fn text(fragment: &str) -> String {
    let plain = ANY_TAG.replace_all(fragment, " ");
    crate::text::decode_html_entities(&plain)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Inner HTML of the element with `id` (nested elements of the same tag are
/// counted, so the matching end tag is found).
pub(super) fn element_by_id<'a>(html: &'a str, id: &str) -> Option<&'a str> {
    let open = TAG
        .captures_iter(html)
        .find(|c| attr(&attrs(&c[2]), "id") == Some(id))?;
    let name = open[1].to_ascii_lowercase();
    let start = open.get(0)?.end();
    if open[2].trim_end().ends_with('/') {
        return Some("");
    }
    let lower = html[start..].to_ascii_lowercase();
    let (o, c) = (format!("<{name}"), format!("</{name}"));
    // Next `<name` that is the whole tag name (`<b` must not match `<br`).
    let find_open = |from: usize| {
        let mut from = from;
        while let Some(i) = lower[from..].find(&o).map(|i| from + i) {
            let end = i + o.len();
            if lower[end..]
                .chars()
                .next()
                .is_none_or(|ch| !ch.is_ascii_alphanumeric())
            {
                return Some(i);
            }
            from = end;
        }
        None
    };
    let mut depth = 1usize;
    let mut pos = 0usize;
    loop {
        let next_close = lower[pos..].find(&c).map(|i| pos + i)?;
        match find_open(pos) {
            Some(i) if i < next_close => {
                depth += 1;
                pos = i + o.len();
            }
            _ => {
                depth -= 1;
                if depth == 0 {
                    return Some(&html[start..start + next_close]);
                }
                pos = next_close + c.len();
            }
        }
    }
}

// ------------------------------------------------------------------ tables

static ROW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<tr\b[^>]*>(.*?)</tr\s*>").unwrap());
static CELL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<(td|th)\b[^>]*>(.*?)</(?:td|th)\s*>").unwrap());

/// Inner HTML of the `<td>` cells of each `<tr>`; rows of only `<th>` cells
/// come back empty.
pub(super) fn table_cells(table: &str) -> Vec<Vec<&str>> {
    ROW.captures_iter(table)
        .map(|r| {
            let row = r.get(1).map_or("", |m| m.as_str());
            CELL.captures_iter(row)
                .filter(|c| c[1].eq_ignore_ascii_case("td"))
                .filter_map(|c| c.get(2).map(|m| m.as_str()))
                .collect()
        })
        .collect()
}

/// `<td>` texts of each `<tr>`; rows of only `<th>` cells come back empty.
pub(super) fn table_texts(table: &str) -> Vec<Vec<String>> {
    table_cells(table)
        .into_iter()
        .map(|cells| cells.into_iter().map(text).collect())
        .collect()
}

// ------------------------------------------------------------------ forms

static FORM_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<form\b([^>]*)>").unwrap());
static FORM_END: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)</form").unwrap());
static SELECT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<select\b([^>]*)>(.*?)</select\s*>").unwrap());
static OPTION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<option\b([^>]*)>").unwrap());

/// A page form as the browser would submit it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Form {
    /// Absolute path (with query) the form posts to.
    pub(super) action: String,
    /// Hidden and text fields and selects with their current values.
    pub(super) fields: Vec<(String, String)>,
}

impl Form {
    pub(super) fn set(&mut self, name: &str, value: &str) {
        match self.fields.iter_mut().find(|(n, _)| n == name) {
            Some((_, v)) => *v = value.to_string(),
            None => self.fields.push((name.to_string(), value.to_string())),
        }
    }

    pub(super) fn get(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// Name of the field ending in `suffix` (JSF generates the prefixes).
    pub(super) fn field_ending(&self, suffix: &str) -> Option<String> {
        self.fields
            .iter()
            .find(|(n, _)| n.ends_with(suffix))
            .map(|(n, _)| n.clone())
    }

    /// The fields plus `name=name` (a clicked button or command link).
    pub(super) fn clicked(&self, name: &str) -> Vec<(String, String)> {
        let mut out = self.fields.clone();
        out.push((name.to_string(), name.to_string()));
        out
    }
}

/// The form with `id`, up to its first `</form>` (nested forms are not
/// supported). Only same-site actions (`/…`, not `//…`) are accepted.
pub(super) fn form_by_id(html: &str, id: &str) -> Option<Form> {
    let open = FORM_TAG
        .captures_iter(html)
        .find(|c| attr(&attrs(&c[1]), "id") == Some(id))?;
    let a = attrs(&open[1]);
    let action = attr(a.as_slice(), "action")?.trim().to_string();
    if !action.starts_with('/') || action.starts_with("//") {
        return None;
    }
    let start = open.get(0)?.end();
    let rest = &html[start..];
    let end = FORM_END.find(rest).map_or(rest.len(), |m| m.start());
    let body = &rest[..end];
    let mut fields = Vec::new();
    for input in INPUT.captures_iter(body) {
        let a = attrs(&input[1]);
        let Some(name) = attr(&a, "name").filter(|n| !n.is_empty()) else {
            continue;
        };
        if attr(&a, "disabled").is_some() {
            continue;
        }
        let kind = attr(&a, "type").unwrap_or("text").to_ascii_lowercase();
        if matches!(kind.as_str(), "hidden" | "text") {
            fields.push((
                name.to_string(),
                attr(&a, "value").unwrap_or("").to_string(),
            ));
        }
    }
    for select in SELECT.captures_iter(body) {
        let a = attrs(&select[1]);
        let Some(name) = attr(&a, "name") else {
            continue;
        };
        let options = enabled_options(&select[2]);
        let chosen = options
            .iter()
            .find(|o| attr(o, "selected").is_some())
            .or(options.first())
            .and_then(|o| attr(o, "value"))
            .unwrap_or("");
        fields.push((name.to_string(), chosen.to_string()));
    }
    Some(Form { action, fields })
}

/// Attributes of the options of a select body that are not `disabled`.
fn enabled_options(select_body: &str) -> Vec<Vec<(String, String)>> {
    OPTION
        .captures_iter(select_body)
        .map(|o| attrs(&o[1]))
        .filter(|o| attr(o, "disabled").is_none())
        .collect()
}

/// Values of the enabled options of the select whose name ends in `suffix`.
pub(super) fn select_options(html: &str, suffix: &str) -> Vec<String> {
    SELECT
        .captures_iter(html)
        .find(|s| attr(&attrs(&s[1]), "name").is_some_and(|n| n.ends_with(suffix)))
        .map(|s| {
            enabled_options(&s[2])
                .iter()
                .filter_map(|o| attr(o, "value").map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

// ------------------------------------------------------------------ JSF partial responses

static CDATA_UPDATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<update id="([^"]+)"><!\[CDATA\[(.*?)\]\]></update>"#).unwrap()
});

/// `(update id, content)` of a JSF partial response.
pub(super) fn partial_updates(xml: &str) -> Vec<(String, String)> {
    CDATA_UPDATE
        .captures_iter(xml)
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .collect()
}

// ------------------------------------------------------------------ page values

/// Amount in an HTML fragment ("<em>- 1,17 €</em>", "50.000,00&nbsp;€") → cents.
pub(super) fn parse_amount(fragment: &str) -> Option<i64> {
    let s = text(fragment);
    let cleaned: String = s
        .replace(['\u{2212}', '\u{2013}'], "-")
        .replace("EUR", "")
        .chars()
        .filter(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | ',' | '.'))
        .collect();
    if !cleaned.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    crate::money::parse_cents(&cleaned).ok()
}

/// `DD.MM.YYYY` anywhere in the cell → `YYYY-MM-DD`.
pub(super) fn parse_date(s: &str) -> Option<String> {
    find_de_date(s).map(iso)
}

/// `YYYY-MM-DD` → `DD.MM.YYYY`, the date format these pages' forms take.
pub(super) fn de_date(iso: &str) -> Option<String> {
    let d = jiff::civil::Date::strptime("%Y-%m-%d", iso).ok()?;
    Some(d.strftime("%d.%m.%Y").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn element_by_id_handles_nesting() {
        let html = r#"<div id="a"><div>x</div><div>y</div></div><div>z</div>"#;
        assert_eq!(element_by_id(html, "a"), Some("<div>x</div><div>y</div>"));
        assert_eq!(element_by_id(html, "b"), None);
        let bold = r#"<b id="t">1<br>2<b>3</b>4</b>rest"#;
        assert_eq!(element_by_id(bold, "t"), Some("1<br>2<b>3</b>4"));
    }

    #[test]
    fn amounts_and_dates() {
        assert_eq!(parse_amount("−12,30 €"), Some(-1230));
        assert_eq!(parse_amount("1.234,56 EUR"), Some(123_456));
        assert_eq!(parse_amount("+0,07"), Some(7));
        assert_eq!(parse_amount("—"), None);
        assert_eq!(parse_date("1.2.2026").as_deref(), Some("2026-02-01"));
        assert_eq!(parse_date("31.02.2026"), None);
        assert_eq!(de_date("2026-01-05").as_deref(), Some("05.01.2026"));
        assert_eq!(de_date("05.01.2026"), None);
    }

    #[test]
    fn text_strips_tags_and_decodes_entities() {
        assert_eq!(text("<b>A&amp;B</b><br>\n  &nbsp;C&#39;s"), "A&B C's");
        // Decoded after the tags are gone, so escaped markup stays text.
        assert_eq!(text("&lt;b&gt;x&lt;/b&gt;"), "<b>x</b>");
    }

    #[test]
    fn attrs_cover_quoting_styles_and_flags() {
        let a = attrs(r#"NAME="n" value='a&amp;b' type=hidden checked"#);
        assert_eq!(attr(&a, "name"), Some("n"));
        assert_eq!(attr(&a, "value"), Some("a&b"));
        assert_eq!(attr(&a, "type"), Some("hidden"));
        assert_eq!(attr(&a, "checked"), Some(""));
        assert_eq!(attr(&a, "id"), None);
    }

    #[test]
    fn tables_skip_header_cells() {
        let table = "<tr><th>Datum</th></tr><tr><td><b>1</b></td><td> x </td></tr>";
        assert_eq!(table_cells(table), [vec![], vec!["<b>1</b>", " x "]]);
        assert_eq!(table_texts(table), [vec![], vec!["1", "x"]]);
    }

    #[test]
    fn forms_keep_submittable_fields_and_same_site_actions() {
        let page = r#"<form id="f" action="/post?x=1">
            <input type="hidden" name="h" value="1">
            <input name="t" value="2">
            <input type="password" name="p" value="secret">
            <input type="hidden" name="off" value="3" disabled>
            <select name="s"><option value="a" disabled selected>A</option>
              <option value="b">B</option></select>
            </form>"#;
        let mut form = form_by_id(page, "f").unwrap();
        assert_eq!(form.action, "/post?x=1");
        assert_eq!(form.get("h"), Some("1"));
        assert_eq!(form.get("t"), Some("2"));
        assert_eq!((form.get("p"), form.get("off")), (None, None));
        assert_eq!(form.get("s"), Some("b"));
        form.set("t", "9");
        assert_eq!(
            form.clicked("go").last().unwrap(),
            &("go".into(), "go".into())
        );
        assert_eq!(form.get("t"), Some("9"));
        assert_eq!(form.field_ending("t").as_deref(), Some("t"));
        for action in ["https://evil.example/x", "//evil.example/x"] {
            let foreign = page.replace("/post?x=1", action);
            assert!(form_by_id(&foreign, "f").is_none(), "{action}");
        }
        assert!(form_by_id(page, "g").is_none());
    }

    #[test]
    fn select_options_skip_disabled_ones() {
        let page = r#"<select name="f:account_input"><option value="1">A</option>
            <option value="2" disabled>B</option><option value="3">C</option></select>"#;
        assert_eq!(select_options(page, ":account_input"), ["1", "3"]);
        assert!(select_options(page, ":other").is_empty());
    }

    #[test]
    fn partial_updates_read_cdata() {
        let xml = r#"<partial-response><changes><update id="a"><![CDATA[<b>1</b>]]></update><update id="b"><![CDATA[]]></update></changes></partial-response>"#;
        assert_eq!(
            partial_updates(xml),
            [
                ("a".to_string(), "<b>1</b>".to_string()),
                ("b".into(), String::new())
            ]
        );
    }
}
