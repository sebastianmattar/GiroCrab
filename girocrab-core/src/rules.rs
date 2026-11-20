//! Keyword / regex / search-expression auto-categorisation.
//!
//! Rules are evaluated in ascending `priority` (then id); the first enabled
//! rule that matches wins. Manually categorised transactions are never
//! touched by rules.

use regex::{Regex, RegexBuilder};

use crate::error::{CoreError, CoreResult};
use crate::i18n::tr;
use crate::models::{MatchKind, Rule, RuleField};
use crate::search_expr::{ExprInput, Prepared, SearchExpr, SearchExprError};

/// The text fields of a transaction a rule can look at.
pub struct RuleInput<'a> {
    pub account_id: i64,
    pub amount_cents: i64,
    pub counterparty: &'a str,
    pub counterparty_iban: &'a str,
    pub purpose: &'a str,
    pub booking_text: &'a str,
    /// The user's note on the booking.
    pub note: &'a str,
    pub currency: &'a str,
    /// IBAN of the booking's own account.
    pub account_iban: &'a str,
}

/// The booking fields a rule or search expression reads, plus the current
/// categorisation; one row of `RuleRow::COLUMNS`.
pub(crate) struct RuleRow {
    pub(crate) id: i64,
    pub(crate) account_id: i64,
    pub(crate) amount_cents: i64,
    pub(crate) counterparty: String,
    pub(crate) counterparty_iban: String,
    pub(crate) purpose: String,
    pub(crate) booking_text: String,
    pub(crate) note: String,
    pub(crate) currency: String,
    pub(crate) account_iban: String,
    pub(crate) category_id: Option<i64>,
    pub(crate) rule_id: Option<i64>,
}

impl RuleRow {
    /// SELECT list over `transactions t` and `accounts a`, in `read` order.
    pub(crate) const COLUMNS: &str = "t.id, t.account_id, t.amount_cents, t.counterparty,
        t.counterparty_iban, t.purpose, t.booking_text, t.note, t.currency, IFNULL(a.iban, ''),
        t.category_id, t.rule_id";

    pub(crate) fn read(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let input = Self::borrowed_input(r)?;
        Ok(Self {
            id: r.get(0)?,
            account_id: input.account_id,
            amount_cents: input.amount_cents,
            counterparty: input.counterparty.into(),
            counterparty_iban: input.counterparty_iban.into(),
            purpose: input.purpose.into(),
            booking_text: input.booking_text.into(),
            note: input.note.into(),
            currency: input.currency.into(),
            account_iban: input.account_iban.into(),
            category_id: r.get(10)?,
            rule_id: r.get(11)?,
        })
    }

    /// The `RuleInput` of a `COLUMNS` row, borrowing its texts (`read` copies
    /// them): scans that keep only a few rows skip the copies (`preview_rule`).
    pub(crate) fn borrowed_input<'r>(r: &'r rusqlite::Row<'_>) -> rusqlite::Result<RuleInput<'r>> {
        let text = |i| r.get_ref(i)?.as_str().map_err(rusqlite::Error::from);
        Ok(RuleInput {
            account_id: r.get(1)?,
            amount_cents: r.get(2)?,
            counterparty: text(3)?,
            counterparty_iban: text(4)?,
            purpose: text(5)?,
            booking_text: text(6)?,
            note: text(7)?,
            currency: text(8)?,
            account_iban: text(9)?,
        })
    }

    pub(crate) fn input(&self) -> RuleInput<'_> {
        RuleInput {
            account_id: self.account_id,
            amount_cents: self.amount_cents,
            counterparty: &self.counterparty,
            counterparty_iban: &self.counterparty_iban,
            purpose: &self.purpose,
            booking_text: &self.booking_text,
            note: &self.note,
            currency: &self.currency,
            account_iban: &self.account_iban,
        }
    }

    pub(crate) fn matches_expr(&self, expr: &SearchExpr) -> bool {
        expr.matches_prepared(&Prepared::new(expr_input(&self.input())))
    }
}

enum Matcher {
    Regex(Regex),
    Expr(SearchExpr),
}

pub struct CompiledRule {
    pub rule: Rule,
    matcher: Matcher,
}

/// Longest accepted rule pattern (bytes); bounds regex automaton allocation.
pub const MAX_PATTERN_LEN: usize = 2048;

impl CompiledRule {
    pub fn compile(rule: Rule) -> CoreResult<Self> {
        Self::compile_with(rule, SearchExpr::parse)
    }

    /// Like [`compile`](Self::compile), but an expression may not use fields
    /// GiroCrab doesn't store. Imported rules keep them (they never match);
    /// a rule saved from the editor may not.
    pub(crate) fn compile_supported(rule: Rule) -> CoreResult<Self> {
        Self::compile_with(rule, SearchExpr::parse_supported)
    }

    fn compile_with(
        rule: Rule,
        parse_expr: fn(&str) -> Result<SearchExpr, SearchExprError>,
    ) -> CoreResult<Self> {
        // Parse an expression before the byte check: the parser has its own,
        // lower limit in characters and its message names it.
        let expr = match rule.match_kind {
            MatchKind::Expression => Some(parse_expr(&rule.pattern)?),
            MatchKind::Keyword | MatchKind::Regex => None,
        };
        if rule.pattern.len() > MAX_PATTERN_LEN {
            return Err(CoreError::invalid(tr("Das Suchmuster ist zu lang.")));
        }
        if let Some(expr) = expr {
            return Ok(CompiledRule {
                rule,
                matcher: Matcher::Expr(expr),
            });
        }
        let source = match rule.match_kind {
            MatchKind::Regex | MatchKind::Expression => rule.pattern.clone(),
            MatchKind::Keyword => {
                let words: Vec<String> = rule
                    .pattern
                    // one keyword per line works too (multi-line field, #179)
                    .split(['|', ',', '\n', '\r'])
                    .map(|w| w.trim())
                    .filter(|w| !w.is_empty())
                    .map(regex::escape)
                    .collect();
                if words.is_empty() {
                    // never matches
                    "$^".to_string()
                } else {
                    format!("(?:{})", words.join("|"))
                }
            }
        };
        let matcher = RegexBuilder::new(&source)
            .case_insensitive(!rule.case_sensitive)
            .size_limit(1 << 20)
            .build()?;
        Ok(CompiledRule {
            rule,
            matcher: Matcher::Regex(matcher),
        })
    }

    pub fn matches(&self, t: &RuleInput) -> bool {
        self.matches_in(t, &Prepared::new(expr_input(t)))
    }

    /// [`matches`](Self::matches) with the booking already prepared for
    /// search expressions (shared by all rules of a [`RuleSet`]).
    fn matches_in(&self, t: &RuleInput, prepared: &Prepared) -> bool {
        let r = &self.rule;
        if !r.enabled {
            return false;
        }
        if !r.admits(t.account_id, t.amount_cents) {
            return false;
        }
        let matcher = match &self.matcher {
            Matcher::Expr(expr) => return expr.matches_prepared(prepared),
            Matcher::Regex(re) => re,
        };
        match r.field {
            RuleField::Any => {
                matcher.is_match(t.counterparty)
                    || matcher.is_match(t.purpose)
                    || matcher.is_match(t.booking_text)
            }
            RuleField::Counterparty => matcher.is_match(t.counterparty),
            RuleField::Purpose => matcher.is_match(t.purpose),
            RuleField::BookingText => matcher.is_match(t.booking_text),
            RuleField::CounterpartyIban => {
                let compact: String = t
                    .counterparty_iban
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                matcher.is_match(&compact)
            }
        }
    }
}

impl Rule {
    /// Whether a booking lies within the rule's accounts and amount range
    /// (its limits beyond the text; a search expression may hold more).
    pub(crate) fn admits(&self, account_id: i64, amount_cents: i64) -> bool {
        (self.account_ids.is_empty() || self.account_ids.contains(&account_id))
            && self.min_amount_cents.is_none_or(|min| amount_cents >= min)
            && self.max_amount_cents.is_none_or(|max| amount_cents <= max)
    }
}

fn expr_input<'a>(t: &RuleInput<'a>) -> ExprInput<'a> {
    ExprInput {
        amount_cents: t.amount_cents,
        name: t.counterparty,
        purpose: t.purpose,
        booking_text: t.booking_text,
        remote_account: t.counterparty_iban,
        local_account: t.account_iban,
        comment: t.note,
        currency: t.currency,
    }
}

pub struct RuleSet {
    rules: Vec<CompiledRule>,
}

impl RuleSet {
    pub fn new(mut rules: Vec<Rule>) -> CoreResult<Self> {
        rules.sort_by_key(|r| (r.priority, r.id));
        let compiled = rules
            .into_iter()
            .map(CompiledRule::compile)
            .collect::<CoreResult<Vec<_>>>()?;
        Ok(RuleSet { rules: compiled })
    }

    /// Returns (rule id, category id) of the first matching rule that sets a
    /// category; rules that only add tags (#580) never categorise.
    pub fn categorise(&self, t: &RuleInput) -> Option<(i64, i64)> {
        let prepared = Prepared::new(expr_input(t));
        self.rules
            .iter()
            .filter(|r| r.rule.category_id != 0)
            .find(|r| r.matches_in(t, &prepared))
            .map(|r| (r.rule.id, r.rule.category_id))
    }

    /// Whether any rule adds tags.
    pub fn sets_tags(&self) -> bool {
        self.rules.iter().any(|r| !r.rule.tag_ids.is_empty())
    }

    /// The rules that add tags (#580), in priority order.
    pub fn tag_rules(&self) -> impl Iterator<Item = &Rule> {
        self.rules
            .iter()
            .map(|r| &r.rule)
            .filter(|r| !r.tag_ids.is_empty())
    }

    /// The rules that add tags and match `t`, in priority order.
    pub fn tag_rules_matching(&self, t: &RuleInput) -> Vec<&Rule> {
        let prepared = Prepared::new(expr_input(t));
        self.rules
            .iter()
            .filter(|r| !r.rule.tag_ids.is_empty() && r.matches_in(t, &prepared))
            .map(|r| &r.rule)
            .collect()
    }

    /// The tags of every matching rule (#580), sorted and deduplicated.
    pub fn tags_for(&self, t: &RuleInput) -> Vec<i64> {
        let mut tags: Vec<i64> = self
            .tag_rules_matching(t)
            .into_iter()
            .flat_map(|r| r.tag_ids.iter().copied())
            .collect();
        tags.sort_unstable();
        tags.dedup();
        tags
    }
}

/// Reads an account set: NULL = no restriction, integer = one account,
/// text = comma-separated ids (`group_concat`). Sorted and deduplicated.
pub(crate) fn parse_account_ids(v: &rusqlite::types::Value) -> Vec<i64> {
    use rusqlite::types::Value;
    let mut ids: Vec<i64> = match v {
        Value::Integer(i) => vec![*i],
        Value::Text(t) => t.split(',').filter_map(|p| p.trim().parse().ok()).collect(),
        _ => Vec::new(),
    };
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Writes a rule's account set (empty = all accounts). `rules.account_id`
/// mirrors the lowest id so the foreign key and older readers keep working.
pub(crate) fn store_rule_accounts(
    conn: &rusqlite::Connection,
    rule_id: i64,
    ids: &[i64],
) -> CoreResult<()> {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    conn.execute(
        "UPDATE rules SET account_id = ?2 WHERE id = ?1",
        rusqlite::params![rule_id, ids.first()],
    )?;
    conn.execute("DELETE FROM rule_accounts WHERE rule_id = ?1", [rule_id])?;
    for id in ids {
        conn.execute(
            "INSERT INTO rule_accounts(rule_id, account_id) VALUES (?1, ?2)",
            rusqlite::params![rule_id, id],
        )?;
    }
    Ok(())
}

/// SQL expression for a rule's account set (`rules` aliased `r`), readable by
/// [`parse_account_ids`].
pub(crate) const ACCOUNT_SET_SQL: &str =
    "(SELECT group_concat(account_id) FROM rule_accounts WHERE rule_id = r.id)";

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(pattern: &str, kind: MatchKind) -> Rule {
        Rule {
            id: 1,
            name: "t".into(),
            category_id: 7,
            field: RuleField::Any,
            match_kind: kind,
            pattern: pattern.into(),
            case_sensitive: false,
            min_amount_cents: None,
            max_amount_cents: Some(0),
            account_ids: Vec::new(),
            priority: 0,
            enabled: true,
            match_count: 0,
            tag_ids: Vec::new(),
        }
    }

    #[test]
    fn account_restriction_accepts_any_listed_account() {
        let mut r = rule("x", MatchKind::Keyword);
        r.max_amount_cents = None;
        let mut t = input("x", -1);
        r.account_ids = vec![2, 3];
        assert!(!CompiledRule::compile(r.clone()).unwrap().matches(&t));
        t.account_id = 3;
        assert!(CompiledRule::compile(r.clone()).unwrap().matches(&t));
        r.account_ids.clear();
        assert!(CompiledRule::compile(r.clone()).unwrap().matches(&t));
    }

    #[test]
    fn account_id_sets_parse() {
        use rusqlite::types::Value;
        assert_eq!(parse_account_ids(&Value::Null), Vec::<i64>::new());
        assert_eq!(parse_account_ids(&Value::Integer(4)), vec![4]);
        assert_eq!(parse_account_ids(&Value::Text("5, 2,5".into())), vec![2, 5]);
    }

    fn input<'a>(cp: &'a str, amount: i64) -> RuleInput<'a> {
        RuleInput {
            account_id: 1,
            amount_cents: amount,
            counterparty: cp,
            counterparty_iban: "",
            purpose: "",
            booking_text: "",
            note: "",
            currency: "EUR",
            account_iban: "",
        }
    }

    #[test]
    fn keyword_rules() {
        let set = RuleSet::new(vec![rule("REWE|edeka", MatchKind::Keyword)]).unwrap();
        assert_eq!(
            set.categorise(&input("Edeka Center Kassel", -2350)),
            Some((1, 7))
        );
        assert_eq!(
            set.categorise(&input("REWE Markt", 500)),
            None,
            "max amount 0 excludes income"
        );
        assert_eq!(set.categorise(&input("Lidl", -100)), None);
    }

    #[test]
    fn keywords_one_per_line() {
        // the multi-line pattern field (#179): a line break separates keywords
        let set =
            RuleSet::new(vec![rule("REWE\nEdeka\r\n  Lidl  \n", MatchKind::Keyword)]).unwrap();
        assert_eq!(
            set.categorise(&input("Edeka Center Kassel", -2350)),
            Some((1, 7))
        );
        assert_eq!(
            set.categorise(&input("LIDL sagt danke", -100)),
            Some((1, 7))
        );
        assert_eq!(set.categorise(&input("Aldi Süd", -100)), None);
    }

    #[test]
    fn regex_rules() {
        let set = RuleSet::new(vec![rule(
            r"^DB\s+(Fernverkehr|Vertrieb)",
            MatchKind::Regex,
        )])
        .unwrap();
        assert!(set.categorise(&input("DB Vertrieb GmbH", -4990)).is_some());
        assert!(set.categorise(&input("ADB Vertrieb", -4990)).is_none());
    }

    #[test]
    fn expression_rule_uses_note_and_amount_range() {
        let set = RuleSet::new(vec![rule("comment:Urlaub", MatchKind::Expression)]).unwrap();
        let mut t = input("", -500);
        t.note = "Urlaub Kroatien";
        assert_eq!(set.categorise(&t), Some((1, 7)));
        t.amount_cents = 500;
        assert_eq!(set.categorise(&t), None, "max amount 0 still applies");
        assert!(RuleSet::new(vec![rule("(REWE", MatchKind::Expression)]).is_err());
    }

    #[test]
    fn overlong_pattern_is_rejected_in_the_ui_language() {
        use crate::i18n::{Lang, with_language};
        let long = "a".repeat(MAX_PATTERN_LEN + 1);
        for (kind, expected) in [
            (MatchKind::Keyword, "The pattern is too long."),
            (MatchKind::Regex, "The pattern is too long."),
            (
                MatchKind::Expression,
                "The search expression is too long (at most 2000 characters).",
            ),
        ] {
            let result = with_language(Lang::En, || CompiledRule::compile(rule(&long, kind)));
            let Err(CoreError::InvalidInput { message }) = result else {
                panic!("{kind:?}: expected InvalidInput");
            };
            assert_eq!(message, expected, "{kind:?}");
        }
        assert!(
            CompiledRule::compile(rule(&"a".repeat(MAX_PATTERN_LEN), MatchKind::Keyword)).is_ok()
        );
    }
}
