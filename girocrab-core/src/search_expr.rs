//! Search expressions for rules, in MoneyMoney's syntax: `AND`/`OR`/`NOT`
//! (uppercase), parentheses, `"phrases"`, `*`/`?` wildcards, `/regex/`,
//! field prefixes (`name:`, `purpose:`, …) and `amount` comparisons.
//! Matching ignores case and folds umlauts (ä → ae, ß → ss). A word matches a
//! whole whitespace-separated word of the text; punctuation doesn't split.

use std::cell::OnceCell;
use std::fmt;

use regex::{Regex, RegexBuilder};

use crate::error::CoreError;
use crate::i18n::{tr, trf};

/// The booking fields an expression can look at.
#[derive(Clone, Copy)]
pub struct ExprInput<'a> {
    pub amount_cents: i64,
    pub name: &'a str,
    pub purpose: &'a str,
    pub booking_text: &'a str,
    pub remote_account: &'a str,
    pub local_account: &'a str,
    pub comment: &'a str,
    pub currency: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Field {
    Name,
    Purpose,
    BookingText,
    RemoteAccount,
    LocalAccount,
    Comment,
    Currency,
    /// Known to MoneyMoney, not stored by GiroCrab: never matches.
    Unsupported(&'static str),
}

impl Field {
    fn parse(s: &str) -> Option<Field> {
        Some(match s {
            "name" => Field::Name,
            "purpose" => Field::Purpose,
            "booking_text" => Field::BookingText,
            "remote_account" => Field::RemoteAccount,
            "local_account" => Field::LocalAccount,
            "comment" => Field::Comment,
            "currency" => Field::Currency,
            "reference" => Field::Unsupported("reference"),
            "mandate" => Field::Unsupported("mandate"),
            "creditor_id" => Field::Unsupported("creditor_id"),
            _ => return None,
        })
    }

    fn text<'a>(self, t: &ExprInput<'a>) -> &'a str {
        match self {
            Field::Name => t.name,
            Field::Purpose => t.purpose,
            Field::BookingText => t.booking_text,
            Field::RemoteAccount => t.remote_account,
            Field::LocalAccount => t.local_account,
            Field::Comment => t.comment,
            Field::Currency => t.currency,
            Field::Unsupported(_) => "",
        }
    }

    /// Slot in [`Prepared`]'s word cache; `None` for unsupported fields.
    fn index(self) -> Option<usize> {
        Some(match self {
            Field::Name => 0,
            Field::Purpose => 1,
            Field::BookingText => 2,
            Field::RemoteAccount => 3,
            Field::LocalAccount => 4,
            Field::Comment => 5,
            Field::Currency => 6,
            Field::Unsupported(_) => return None,
        })
    }

    /// Account numbers are compared without their spaces, as one word.
    fn is_account(self) -> bool {
        matches!(self, Field::RemoteAccount | Field::LocalAccount)
    }
}

/// Fields searched by a term without a field prefix.
const DEFAULT_FIELDS: [Field; 4] = [
    Field::Name,
    Field::Purpose,
    Field::BookingText,
    Field::Comment,
];

#[derive(Debug, Clone, Copy, PartialEq)]
enum Cmp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

enum Pattern {
    /// One word or a phrase: consecutive words, each an anchored wildcard pattern.
    Words(Vec<Regex>),
    Regex(Regex),
}

enum Node {
    Text(Option<Field>, Pattern),
    Amount(Cmp, i64),
    Not(Box<Node>),
    And(Box<Node>, Box<Node>),
    Or(Box<Node>, Box<Node>),
}

enum Tok {
    Open,
    Close,
    And,
    Or,
    Not,
    Atom(Node),
}

pub struct SearchExpr {
    root: Node,
    /// First field the expression uses that GiroCrab doesn't store.
    unsupported: Option<&'static str>,
}

/// Limits that keep the recursive parser/evaluator (and dropping the tree)
/// far from the stack limit, whatever a synced or imported rule contains.
const MAX_EXPR_CHARS: usize = 2000;
const MAX_NESTING: usize = 32;

/// Why an expression doesn't parse. `Display` is the localised message shown
/// in the rule editor; it reaches the app as `CoreError::InvalidInput`.
#[derive(Debug, PartialEq)]
pub enum SearchExprError {
    TooLong,
    Empty,
    TooDeep,
    Incomplete,
    UnexpectedClose,
    UnclosedParen,
    UnclosedQuote,
    UnclosedRegex,
    AndWithoutTerm,
    OrWithoutTerm,
    EndsAfter(&'static str),
    MissingValue(String),
    UnknownField(String),
    InvalidAmount(String),
    InvalidRegex(regex::Error),
    /// Rejected by [`SearchExpr::parse_supported`].
    UnsupportedField(&'static str),
}

impl fmt::Display for SearchExprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use SearchExprError::*;
        let text = match self {
            TooLong => trf(
                "Der Suchausdruck ist zu lang (höchstens {0} Zeichen).",
                &[&MAX_EXPR_CHARS],
            ),
            Empty => tr("Der Suchausdruck ist leer.").into(),
            TooDeep => tr("Der Suchausdruck ist zu tief verschachtelt.").into(),
            Incomplete => tr("Der Suchausdruck ist unvollständig.").into(),
            UnexpectedClose => tr("Unerwartete schließende Klammer.").into(),
            UnclosedParen => tr("Klammer nicht geschlossen.").into(),
            UnclosedQuote => tr("Anführungszeichen nicht geschlossen.").into(),
            UnclosedRegex => tr("Regulärer Ausdruck nicht mit / geschlossen.").into(),
            AndWithoutTerm => tr("„AND“ steht ohne Begriff davor.").into(),
            OrWithoutTerm => tr("„OR“ steht ohne Begriff davor.").into(),
            EndsAfter(op) => trf("Der Ausdruck endet nach „{0}“.", &[op]),
            MissingValue(field) => trf("Nach „{0}:“ fehlt ein Suchbegriff.", &[field]),
            UnknownField(field) => trf("Unbekanntes Feld „{0}“", &[field]),
            InvalidAmount(value) => trf("Ungültiger Betrag „{0}“", &[value]),
            InvalidRegex(e) => trf("Ungültiger regulärer Ausdruck: {0}", &[e]),
            UnsupportedField(field) => trf("Das Feld „{0}“ wird nicht unterstützt.", &[field]),
        };
        f.write_str(&text)
    }
}

impl From<SearchExprError> for CoreError {
    fn from(e: SearchExprError) -> Self {
        CoreError::invalid(e.to_string())
    }
}

impl SearchExpr {
    pub fn parse(src: &str) -> Result<SearchExpr, SearchExprError> {
        if src.chars().count() > MAX_EXPR_CHARS {
            return Err(SearchExprError::TooLong);
        }
        let (toks, unsupported) = tokenize(src)?;
        if toks.is_empty() {
            return Err(SearchExprError::Empty);
        }
        let mut p = Parser {
            toks,
            pos: 0,
            depth: 0,
        };
        let root = p.or()?;
        if p.pos < p.toks.len() {
            return Err(SearchExprError::UnexpectedClose);
        }
        Ok(SearchExpr { root, unsupported })
    }

    /// Like [`parse`](Self::parse), but rejects fields GiroCrab doesn't store:
    /// they parse (imported rules keep them) but never match.
    pub fn parse_supported(src: &str) -> Result<SearchExpr, SearchExprError> {
        let expr = Self::parse(src)?;
        match expr.unsupported {
            Some(field) => Err(SearchExprError::UnsupportedField(field)),
            None => Ok(expr),
        }
    }

    pub fn matches(&self, t: &ExprInput) -> bool {
        self.matches_prepared(&Prepared::new(*t))
    }

    /// Like [`matches`](Self::matches), reusing the booking's folded words
    /// across all expressions evaluated against the same `Prepared`.
    pub fn matches_prepared(&self, t: &Prepared) -> bool {
        eval(&self.root, t)
    }
}

/// A booking's text fields, folded and split into words on first use, so
/// every term of every rule evaluated against it shares that work.
pub struct Prepared<'a> {
    input: ExprInput<'a>,
    words: [OnceCell<Vec<String>>; 7],
}

impl<'a> Prepared<'a> {
    pub fn new(input: ExprInput<'a>) -> Self {
        Prepared {
            input,
            words: Default::default(),
        }
    }

    fn words(&self, f: Field) -> &[String] {
        let Some(i) = f.index() else {
            return &[];
        };
        self.words[i].get_or_init(|| {
            let text = f.text(&self.input);
            let folded = if f.is_account() {
                fold(&text.replace(' ', ""))
            } else {
                fold(text)
            };
            #[cfg(test)]
            FOLDS.with(|c| c.set(c.get() + 1));
            folded.split_whitespace().map(String::from).collect()
        })
    }
}

#[cfg(test)]
thread_local! {
    static FOLDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How often a booking field has been folded on this thread (tests only).
#[cfg(test)]
fn fold_count() -> usize {
    FOLDS.with(|c| c.get())
}
/// Lowercase plus ä → ae, ö → oe, ü → ue, ß → ss.
fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        match c {
            'ä' => out.push_str("ae"),
            'ö' => out.push_str("oe"),
            'ü' => out.push_str("ue"),
            'ß' => out.push_str("ss"),
            c => out.push(c),
        }
    }
    out
}

fn wildcard(word: &str) -> Result<Regex, SearchExprError> {
    let mut src = String::from("^");
    for c in fold(word).chars() {
        match c {
            '*' => src.push_str(".*"),
            '?' => src.push('.'),
            c => src.push_str(&regex::escape(&c.to_string())),
        }
    }
    src.push('$');
    Regex::new(&src).map_err(SearchExprError::InvalidRegex)
}

fn words(text: &str) -> Result<Pattern, SearchExprError> {
    let parts = text
        .split_whitespace()
        .map(wildcard)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Pattern::Words(parts))
}

fn regex(src: &str) -> Result<Pattern, SearchExprError> {
    RegexBuilder::new(src)
        .case_insensitive(true)
        .size_limit(1 << 20)
        .build()
        .map(Pattern::Regex)
        .map_err(SearchExprError::InvalidRegex)
}

fn parse_cents(v: &str) -> Option<i64> {
    let (neg, digits) = match v.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, v.strip_prefix('+').unwrap_or(v)),
    };
    // German thousands dots before a decimal comma: 1.000,50 -> 1000,50.
    // Only when a comma is present and every group has exactly three digits;
    // a lone "1.000" is left alone (still rejected).
    let normalized;
    let digits = match digits.rsplit_once(',') {
        Some((int, frac)) if int.contains('.') => {
            let mut groups = int.split('.');
            let first = groups.next().unwrap_or("");
            if first.is_empty() || first.len() > 3 || groups.any(|g| g.len() != 3) {
                return None;
            }
            normalized = format!("{},{frac}", int.replace('.', ""));
            normalized.as_str()
        }
        _ => digits,
    };
    let (int, frac) = match digits.split_once([',', '.']) {
        Some((i, f)) => (i, f),
        None => (digits, ""),
    };
    if int.is_empty() && frac.is_empty()
        || !int.chars().all(|c| c.is_ascii_digit())
        || !frac.chars().all(|c| c.is_ascii_digit())
        || frac.len() > 2
    {
        return None;
    }
    let int: i64 = if int.is_empty() { 0 } else { int.parse().ok()? };
    let frac: i64 = format!("{frac:0<2}").parse().ok()?;
    let cents = int.checked_mul(100)?.checked_add(frac)?;
    Some(if neg { -cents } else { cents })
}

fn amount(rest: &str) -> Result<Node, SearchExprError> {
    let (cmp, value) = [
        ("==", Cmp::Eq),
        ("!=", Cmp::Ne),
        ("<=", Cmp::Le),
        (">=", Cmp::Ge),
        ("=", Cmp::Eq),
        ("<", Cmp::Lt),
        (">", Cmp::Gt),
    ]
    .iter()
    .find_map(|(op, cmp)| rest.strip_prefix(op).map(|v| (*cmp, v)))
    .ok_or_else(|| SearchExprError::InvalidAmount(rest.into()))?;
    let cents = parse_cents(value).ok_or_else(|| SearchExprError::InvalidAmount(value.into()))?;
    Ok(Node::Amount(cmp, cents))
}

/// Reads up to the closing `end` (a `\` escapes it) and returns the text in between.
fn delimited(chars: &[char], i: &mut usize, end: char) -> Option<String> {
    let mut out = String::new();
    while *i < chars.len() {
        let c = chars[*i];
        *i += 1;
        if c == '\\' && end == '/' && chars.get(*i) == Some(&'/') {
            out.push('/');
            *i += 1;
        } else if c == end {
            return Some(out);
        } else {
            out.push(c);
        }
    }
    None
}

/// A quoted phrase or a `/regex/` starting at `chars[*i]`.
fn quoted(chars: &[char], i: &mut usize) -> Result<Pattern, SearchExprError> {
    let open = chars[*i];
    *i += 1;
    match open {
        '"' => {
            let s = delimited(chars, i, '"').ok_or(SearchExprError::UnclosedQuote)?;
            words(&s)
        }
        _ => {
            let s = delimited(chars, i, '/').ok_or(SearchExprError::UnclosedRegex)?;
            regex(&s)
        }
    }
}

/// The tokens and the first field GiroCrab doesn't store, if any.
fn tokenize(src: &str) -> Result<(Vec<Tok>, Option<&'static str>), SearchExprError> {
    let mut unsupported = None;
    let chars: Vec<char> = src.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '(' || c == ')' {
            toks.push(if c == '(' { Tok::Open } else { Tok::Close });
            i += 1;
            continue;
        }
        if c == '"' || c == '/' {
            toks.push(Tok::Atom(Node::Text(None, quoted(&chars, &mut i)?)));
            continue;
        }
        // A bare word, possibly `field:` followed by a word, phrase or regex.
        let start = i;
        while i < chars.len() && !chars[i].is_whitespace() && !matches!(chars[i], '(' | ')') {
            if chars[i] == ':' {
                break;
            }
            i += 1;
        }
        let head: String = chars[start..i].iter().collect();
        if i < chars.len() && chars[i] == ':' && is_field_name(&head) {
            let field =
                Field::parse(&head).ok_or_else(|| SearchExprError::UnknownField(head.clone()))?;
            if let Field::Unsupported(name) = field {
                unsupported.get_or_insert(name);
            }
            i += 1;
            let pattern = if field.is_account() && chars.get(i) == Some(&'"') {
                // "DE89 3704 …" is one account number, not a phrase.
                i += 1;
                let s = delimited(&chars, &mut i, '"').ok_or(SearchExprError::UnclosedQuote)?;
                words(&s.replace(' ', ""))?
            } else if matches!(chars.get(i), Some('"') | Some('/')) {
                quoted(&chars, &mut i)?
            } else {
                let s = i;
                while i < chars.len() && !chars[i].is_whitespace() && !matches!(chars[i], '(' | ')')
                {
                    i += 1;
                }
                let value: String = chars[s..i].iter().collect();
                if value.is_empty() {
                    return Err(SearchExprError::MissingValue(head));
                }
                if field.is_account() {
                    words(&value.replace(' ', ""))?
                } else {
                    words(&value)?
                }
            };
            toks.push(Tok::Atom(Node::Text(Some(field), pattern)));
            continue;
        }
        // Not a field: the colon belongs to the word.
        while i < chars.len() && !chars[i].is_whitespace() && !matches!(chars[i], '(' | ')') {
            i += 1;
        }
        let word: String = chars[start..i].iter().collect();
        toks.push(match word.as_str() {
            "AND" => Tok::And,
            "OR" => Tok::Or,
            "NOT" => Tok::Not,
            w if w.starts_with("amount") && w[6..].starts_with(['=', '!', '<', '>']) => {
                Tok::Atom(amount(&w[6..])?)
            }
            w => Tok::Atom(Node::Text(None, words(w)?)),
        });
    }
    Ok((toks, unsupported))
}

/// `name` in `name:` – lowercase letters and underscores only, so words like
/// "Ref:123" or "https://…" stay plain words.
fn is_field_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c == '_')
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    /// Current nesting of parentheses and NOTs.
    depth: usize,
}

impl Parser {
    fn enter(&mut self) -> Result<(), SearchExprError> {
        self.depth += 1;
        if self.depth > MAX_NESTING {
            return Err(SearchExprError::TooDeep);
        }
        Ok(())
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn or(&mut self) -> Result<Node, SearchExprError> {
        let mut left = self.and()?;
        while matches!(self.peek(), Some(Tok::Or)) {
            self.pos += 1;
            let right = self.operand("OR", Parser::and)?;
            left = Node::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Node, SearchExprError> {
        let mut left = self.unary()?;
        loop {
            match self.peek() {
                Some(Tok::And) => {
                    self.pos += 1;
                    let right = self.operand("AND", Parser::unary)?;
                    left = Node::And(Box::new(left), Box::new(right));
                }
                // Adjacent terms: implicit AND.
                Some(Tok::Atom(_)) | Some(Tok::Not) | Some(Tok::Open) => {
                    let right = self.unary()?;
                    left = Node::And(Box::new(left), Box::new(right));
                }
                _ => return Ok(left),
            }
        }
    }

    fn unary(&mut self) -> Result<Node, SearchExprError> {
        if matches!(self.peek(), Some(Tok::Not)) {
            self.pos += 1;
            self.enter()?;
            let inner = self.operand("NOT", Parser::unary);
            self.depth -= 1;
            return Ok(Node::Not(Box::new(inner?)));
        }
        self.primary()
    }

    /// The operand after `op`; an error if the expression ends there.
    fn operand(
        &mut self,
        op: &'static str,
        next: fn(&mut Parser) -> Result<Node, SearchExprError>,
    ) -> Result<Node, SearchExprError> {
        if matches!(self.peek(), None | Some(Tok::Close)) {
            return Err(SearchExprError::EndsAfter(op));
        }
        next(self)
    }

    fn primary(&mut self) -> Result<Node, SearchExprError> {
        let tok = self.toks.get_mut(self.pos);
        match tok {
            Some(Tok::Open) => {
                self.pos += 1;
                self.enter()?;
                let inner = self.or();
                self.depth -= 1;
                let inner = inner?;
                if !matches!(self.peek(), Some(Tok::Close)) {
                    return Err(SearchExprError::UnclosedParen);
                }
                self.pos += 1;
                Ok(inner)
            }
            Some(Tok::Atom(_)) => {
                let Tok::Atom(node) = std::mem::replace(&mut self.toks[self.pos], Tok::Close)
                else {
                    unreachable!()
                };
                self.pos += 1;
                Ok(node)
            }
            Some(Tok::Close) => Err(SearchExprError::UnexpectedClose),
            Some(Tok::And) => Err(SearchExprError::AndWithoutTerm),
            Some(Tok::Or) => Err(SearchExprError::OrWithoutTerm),
            Some(Tok::Not) | None => Err(SearchExprError::Incomplete),
        }
    }
}

fn eval(node: &Node, t: &Prepared) -> bool {
    match node {
        Node::Not(n) => !eval(n, t),
        Node::And(a, b) => eval(a, t) && eval(b, t),
        Node::Or(a, b) => eval(a, t) || eval(b, t),
        Node::Amount(cmp, v) => {
            let a = t.input.amount_cents;
            match cmp {
                Cmp::Eq => a == *v,
                Cmp::Ne => a != *v,
                Cmp::Lt => a < *v,
                Cmp::Gt => a > *v,
                Cmp::Le => a <= *v,
                Cmp::Ge => a >= *v,
            }
        }
        Node::Text(Some(Field::Unsupported(_)), _) => false,
        Node::Text(Some(f), p) => text_matches(p, t, *f),
        Node::Text(None, p) => DEFAULT_FIELDS.iter().any(|f| text_matches(p, t, *f)),
    }
}

fn text_matches(p: &Pattern, t: &Prepared, f: Field) -> bool {
    match p {
        Pattern::Regex(re) => re.is_match(f.text(&t.input)),
        Pattern::Words(pat) => {
            !pat.is_empty()
                && t.words(f)
                    .windows(pat.len())
                    .any(|w| w.iter().zip(pat).all(|(tok, re)| re.is_match(tok)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inp<'a>(name: &'a str, purpose: &'a str, amount_cents: i64) -> ExprInput<'a> {
        ExprInput {
            amount_cents,
            name,
            purpose,
            booking_text: "",
            remote_account: "",
            local_account: "",
            comment: "",
            currency: "EUR",
        }
    }

    fn m(expr: &str, t: &ExprInput) -> bool {
        SearchExpr::parse(expr).unwrap().matches(t)
    }

    #[test]
    fn bare_word_matches_whole_token_in_any_text_field() {
        assert!(m("REWE", &inp("REWE Markt GmbH", "", -100)));
        assert!(m("rewe", &inp("", "Einkauf REWE 123", -100)));
        assert!(!m("REWE", &inp("REWEMARKT", "", -100)));
    }

    #[test]
    fn wildcards() {
        assert!(m("Blumen*", &inp("Blumenhaus Kunz", "", -1)));
        assert!(m("*Miete*", &inp("", "Wohnungsmiete.Mai", -1)));
        assert!(m("M?ller", &inp("Miller", "", -1)));
    }

    #[test]
    fn phrase_matches_consecutive_words() {
        assert!(m("\"Bahn AG\"", &inp("DB Bahn AG", "", -1)));
        assert!(!m("\"Bahn AG\"", &inp("AG der Bahn", "", -1)));
    }

    #[test]
    fn boolean_operators_and_precedence() {
        let t = inp("EC-Karte", "Barauszahlung", -1);
        assert!(!m("\"EC-Karte\" AND (NOT \"Barauszahlung\")", &t));
        assert!(m(
            "\"EC-Karte\" AND (NOT \"Barauszahlung\")",
            &inp("EC-Karte", "", -1)
        ));
        assert!(m(
            "Buecher OR eBooks OR (Waren AND Versand)",
            &inp("", "Waren Versand", -1)
        ));
        assert!(m("REWE EDEKA", &inp("REWE EDEKA", "", -1)), "implicit AND");
        assert!(!m("REWE EDEKA", &inp("REWE", "", -1)));
        assert!(
            m("Lidl and Aldi", &inp("Lidl and Aldi", "", -1)),
            "lowercase and is a word"
        );
        assert!(!m("Lidl and Aldi", &inp("Lidl Aldi", "", -1)));
    }

    #[test]
    fn fields_restrict_the_search() {
        assert!(m("name:REWE", &inp("REWE", "", -1)));
        assert!(!m("name:REWE", &inp("", "REWE", -1)));
        assert!(m("purpose:\"Miete Mai\"", &inp("", "Miete Mai 2026", -1)));
        let mut t = inp("", "", -1);
        t.comment = "Urlaub Kroatien";
        t.remote_account = "DE89 3704 0044 0532 0130 00";
        assert!(m("comment:Urlaub", &t));
        assert!(m("remote_account:DE89370400440532013000", &t));
        assert!(m("currency:eur", &t));
    }

    #[test]
    fn german_thousands_separators_in_amounts() {
        assert_eq!(parse_cents("1.000,00"), Some(100_000));
        assert_eq!(parse_cents("-12.345.678,9"), Some(-1_234_567_890));
        // Without a comma "1.000" stays ambiguous and is rejected, as before.
        assert_eq!(parse_cents("1.000"), None);
        assert_eq!(parse_cents("1,5"), Some(150));
        for bad in ["1.00,00", ".000,00", "1.0000,00", "1.000,000", "1..000,00"] {
            assert_eq!(parse_cents(bad), None, "{bad}");
        }
        let t = inp("x", "y", 150_000);
        assert!(m("amount>1.000,00", &t));
        assert!(!m("amount>2.000,00", &t));
    }

    #[test]
    fn deep_or_huge_expressions_are_rejected_not_crashed() {
        let n = 100_000;
        assert!(SearchExpr::parse(&format!("{}a{}", "(".repeat(n), ")".repeat(n))).is_err());
        assert!(SearchExpr::parse(&"a ".repeat(n)).is_err());
        assert!(SearchExpr::parse(&format!("{}a", "NOT ".repeat(n))).is_err());
        let deep = |d: usize| format!("{}a{}", "(".repeat(d), ")".repeat(d));
        assert!(SearchExpr::parse(&deep(MAX_NESTING)).is_ok());
        assert!(SearchExpr::parse(&deep(MAX_NESTING + 1)).is_err());
        // Wildcard storms stay linear (regex crate, no backtracking).
        let e = SearchExpr::parse(&format!("{}b", "a*".repeat(200))).unwrap();
        let long = "a".repeat(5000);
        let t = inp(&long, "", 0);
        assert!(!e.matches(&t));
        // Multi-byte text next to wildcards and operators does not panic.
        let t = inp("Müller 🦀 Straße", "Café ñ", 0);
        assert!(m("m*ller 🦀 stra*e OR amount>1", &t));
        assert!(m("/Caf./ AND *ñ", &t));
    }

    #[test]
    fn amount_comparisons_in_cents() {
        assert!(m("amount<0", &inp("", "", -1)));
        assert!(!m("amount<0", &inp("", "", 0)));
        assert!(m("amount>=12,50", &inp("", "", 1250)));
        assert!(!m("amount>12,50", &inp("", "", 1250)));
        assert!(m("amount!=5.00", &inp("", "", 499)));
        assert!(m("amount=-3,5", &inp("", "", -350)));
        assert!(m("amount==-3,5", &inp("", "", -350)));
        assert!(m("amount<=0,00", &inp("", "", 0)));
    }

    #[test]
    fn umlauts_fold_both_ways() {
        assert!(m("name:Müller", &inp("MUELLER GmbH", "", -1)));
        assert!(m("name:Mueller", &inp("Müller", "", -1)));
        assert!(
            !m("Strasse", &inp("", "Hauptstraße", -1)),
            "whole token only"
        );
        assert!(m("*strasse", &inp("", "Hauptstraße", -1)));
    }

    #[test]
    fn regex_atoms() {
        assert!(m("/GA NR[0-9]{8}/", &inp("", "BahnCard GA NR12345678", -1)));
        assert!(m("purpose:/^miete/", &inp("", "Miete Mai", -1)));
        assert!(!m("name:/^miete/", &inp("", "Miete Mai", -1)));
    }

    #[test]
    fn unsupported_fields_parse_but_never_match() {
        let src = "mandate:X OR reference:ABC";
        assert!(
            !SearchExpr::parse(src)
                .unwrap()
                .matches(&inp("ABC", "X", -1))
        );
        assert!(matches!(
            SearchExpr::parse_supported(src),
            Err(SearchExprError::UnsupportedField("mandate"))
        ));
        assert!(SearchExpr::parse_supported("name:ABC").is_ok());
    }

    #[test]
    fn syntax_errors_are_reported() {
        use SearchExprError::*;
        let err = |src: &str| {
            SearchExpr::parse(src)
                .err()
                .unwrap_or_else(|| panic!("{src:?} parsed"))
        };
        assert_eq!(err(""), Empty);
        assert_eq!(err("("), Incomplete);
        assert_eq!(err("(REWE"), UnclosedParen);
        assert_eq!(err("REWE)"), UnexpectedClose);
        assert_eq!(err("\"REWE"), UnclosedQuote);
        assert_eq!(err("/REWE"), UnclosedRegex);
        assert_eq!(err("AND REWE"), AndWithoutTerm);
        assert_eq!(err("OR REWE"), OrWithoutTerm);
        assert_eq!(err("REWE OR"), EndsAfter("OR"));
        assert_eq!(err("NOT"), EndsAfter("NOT"));
        assert_eq!(err("foo:bar"), UnknownField("foo".into()));
        assert_eq!(err("name:"), MissingValue("name".into()));
        assert_eq!(err("amount<abc"), InvalidAmount("abc".into()));
        assert!(matches!(err("/(/"), InvalidRegex(_)));
    }

    #[test]
    fn errors_reach_the_app_as_localised_invalid_input() {
        let Err(e) = SearchExpr::parse("foo:bar") else {
            panic!("parsed");
        };
        assert!(matches!(
            CoreError::from(e),
            CoreError::InvalidInput { message } if message == "Unbekanntes Feld „foo“"
        ));
    }

    #[test]
    fn quoted_account_number_ignores_spaces() {
        let mut t = inp("", "", -1);
        t.remote_account = "DE89 3704 0044 0532 0130 00";
        assert!(m("remote_account:\"DE89 3704 0044 0532 0130 00\"", &t));
        assert!(m("remote_account:\"DE89370400440532013000\"", &t));
    }

    #[test]
    fn a_booking_is_folded_once_per_field_for_all_rules() {
        let rules: Vec<SearchExpr> = (0..20)
            .map(|r| {
                SearchExpr::parse(&format!(
                    "Shop{r}a* OR Shop{r}b OR purpose:x{r} OR comment:y{r}"
                ))
                .unwrap()
            })
            .collect();
        let t = inp("Kaufhaus Nord", "Einkauf vom Montag", -100);
        let prepared = Prepared::new(t);
        let before = fold_count();
        assert!(!rules.iter().any(|e| e.matches_prepared(&prepared)));
        // name, purpose, booking_text, comment – each folded once
        assert_eq!(fold_count() - before, 4);
    }
}
