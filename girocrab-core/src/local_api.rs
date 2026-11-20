//! Read-only local interface for external analysis tools (GitHub #136).
//!
//! A versioned JSON request/response layer over the open vault: accounts,
//! categories, bookings (keyset-paged) and period summaries. The app serves it
//! on a local socket while the vault is unlocked ([`server`]); every socket
//! request carries the access token the user created in the app.
//!
//! - Opt-in: off until the user stores an enabled [`ShareScope`]; the scope is
//!   read on every request, so revoking takes effect at once.
//! - Strictly read-only: the handler only knows the read methods below, nothing
//!   writes user data, no connection logins, credentials, keys or documents
//!   (`accounts` uses `list_accounts`, which may refresh stale derived asset
//!   values, as on every app reload).
//! - Only the accounts in the scope are visible, also through search, loan views
//!   and sums: a payment linked to an unshared loan reads as `transfer`. A
//!   booking to an unshared *own* account stays a `transfer` too (else savings
//!   would count as spending); that tells "to an own account", never which.
//! - IBANs / account numbers and notes are off by default; counterparty and
//!   purpose can be switched off. Categories (names, tree, a booking's category)
//!   and the coarse `flow` are always shared: without them the data says
//!   little, and both can reflect hidden fields (a rule on a counterparty, a
//!   salary IBAN).
//!
//! The wire types (`Api*`) are their own, versioned structs: internal models
//! change between releases, `v1` stays stable.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::LazyLock;

use regex::{Captures, Regex};
use rusqlite::params_from_iter;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::CoreError;
use crate::merchants::FlowKind;
use crate::models::{Booking, BookingDirection, BookingFilter, KpiSide, kpi_side};
use crate::vault::Vault;

/// Version of the request/response format; a breaking change bumps it.
pub const API_VERSION: u32 = 1;

/// Settings key of the stored scope (JSON, inside the encrypted vault).
const SCOPE_SETTING: &str = "local_api.scope";

const DEFAULT_PAGE: u32 = 100;
const MAX_PAGE: u32 = 500;
/// Longest `summary` range (three years).
const MAX_SUMMARY_DAYS: i64 = 3 * 366;

/// What the user shares. Accounts are an explicit allow list: an account added
/// later is not shared until the user selects it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
#[serde(default)]
pub struct ShareScope {
    pub enabled: bool,
    pub account_ids: Vec<i64>,
    /// Own and counterparty IBANs and account numbers; off = also masked inside texts.
    pub share_account_numbers: bool,
    pub share_counterparty: bool,
    /// Purpose and booking text.
    pub share_purpose: bool,
    pub share_notes: bool,
    /// SHA-256 (hex) of the access token; "" = none, socket requests are refused.
    /// Only `create_local_api_token` / `revoke_local_api_token` change it.
    pub token_sha256: String,
}

impl Default for ShareScope {
    fn default() -> Self {
        Self {
            enabled: false,
            account_ids: Vec::new(),
            share_account_numbers: false,
            share_counterparty: true,
            share_purpose: true,
            share_notes: false,
            token_sha256: String::new(),
        }
    }
}

// MARK: - Wire types (v1)

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiInfo {
    pub api_version: u32,
    pub core_version: String,
    pub account_count: u32,
    pub shares_account_numbers: bool,
    pub shares_counterparty: bool,
    pub shares_purpose: bool,
    pub shares_notes: bool,
    /// Newest successful bank sync of a shared account (Unix seconds).
    pub last_sync_unix: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiAccount {
    pub id: i64,
    pub name: String,
    pub bank_name: String,
    /// `checking`, `savings`, `credit_card`, … (`AccountKind::as_str`).
    pub kind: String,
    pub currency: String,
    pub balance_cents: i64,
    pub balance_date: String,
    pub archived: bool,
    /// Last successful sync of the account's bank connection (Unix seconds).
    pub last_sync_unix: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iban: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_number: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiCategory {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    /// Full path, segments joined with ` › `.
    pub path: String,
    pub depth: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiTransaction {
    /// Stable for the life of the booking.
    pub id: i64,
    pub account_id: i64,
    pub booking_date: String,
    pub value_date: String,
    /// Negative = outflow.
    pub amount_cents: i64,
    pub currency: String,
    pub category_id: Option<i64>,
    pub category_path: String,
    /// Recognised merchant ("" = none).
    pub merchant: String,
    /// `consumption`, `income`, `investment`, `perk`, `transfer`, `loan`.
    pub flow: String,
    pub payment_method: String,
    /// Not booked yet: the bank can still change or drop it.
    pub pending: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counterparty: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counterparty_iban: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub booking_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiTransactionPage {
    pub transactions: Vec<ApiTransaction>,
    /// Pass as `cursor` for the next (older) page; `None` = last page.
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiCurrencyTotals {
    pub currency: String,
    /// Same definition as the app's income / expense tiles (transfers between
    /// own accounts are neutral, refunds reduce the expenses), except that a
    /// payment to an unshared loan is neutral too.
    pub income_cents: i64,
    /// Positive amount of money spent.
    pub expense_cents: i64,
    pub booking_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiCategoryTotal {
    pub category_id: Option<i64>,
    pub category_path: String,
    pub currency: String,
    /// Net sum of every booking in the category, like the app's category list:
    /// transfers and loan payments included (unlike `totals`), split bookings
    /// with their parts.
    pub net_cents: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiSummary {
    pub date_from: String,
    pub date_to: String,
    pub totals: Vec<ApiCurrencyTotals>,
    pub by_category: Vec<ApiCategoryTotal>,
}

// MARK: - Requests

#[derive(Debug, Deserialize)]
struct Request {
    #[serde(default)]
    v: Option<u32>,
    /// Required on the socket, ignored in process.
    #[serde(default)]
    token: Option<String>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TransactionParams {
    account_ids: Vec<i64>,
    date_from: Option<String>,
    date_to: Option<String>,
    category_id: Option<i64>,
    include_subcategories: Option<bool>,
    uncategorized_only: bool,
    /// `income`, `expense` or `all` (default).
    direction: Option<String>,
    search: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SummaryParams {
    account_ids: Vec<i64>,
    date_from: String,
    date_to: String,
}

/// Error codes of the response (`error.code`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiErrorCode {
    Disabled,
    /// Missing or wrong access token.
    Unauthorized,
    /// The vault is locked (socket only).
    Locked,
    /// All connections in use; try again later (socket only).
    Busy,
    UnsupportedVersion,
    UnknownMethod,
    InvalidParams,
    NotShared,
    Internal,
}

impl ApiErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ApiErrorCode::Disabled => "disabled",
            ApiErrorCode::Unauthorized => "unauthorized",
            ApiErrorCode::Locked => "locked",
            ApiErrorCode::Busy => "busy",
            ApiErrorCode::UnsupportedVersion => "unsupported_version",
            ApiErrorCode::UnknownMethod => "unknown_method",
            ApiErrorCode::InvalidParams => "invalid_params",
            ApiErrorCode::NotShared => "not_shared",
            ApiErrorCode::Internal => "internal",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub code: ApiErrorCode,
    pub message: String,
}

impl ApiError {
    fn new(code: ApiErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new(ApiErrorCode::InvalidParams, message)
    }
}

impl From<CoreError> for ApiError {
    /// Vault errors stay inside the app: their text can name stored data.
    fn from(_: CoreError) -> Self {
        Self::new(ApiErrorCode::Internal, "internal error")
    }
}

type ApiResult<T> = Result<T, ApiError>;

/// The stored scope; default (disabled, nothing shared) when none or an
/// unreadable one is stored. Its accounts are the live ones only: a deleted
/// account is neither served nor reported as shared.
fn stored_scope(conn: &rusqlite::Connection) -> Result<ShareScope, CoreError> {
    let mut scope: ShareScope = Vault::setting_inner(conn, SCOPE_SETTING)?
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    scope.account_ids = live_account_ids(conn, &scope.account_ids)?;
    Ok(scope)
}

/// `ids` without deleted or unknown accounts, in the accounts' sort order.
fn live_account_ids(conn: &rusqlite::Connection, ids: &[i64]) -> Result<Vec<i64>, CoreError> {
    let wanted: HashSet<i64> = ids.iter().copied().collect();
    let mut stmt =
        conn.prepare("SELECT id FROM accounts WHERE deleted_ms IS NULL ORDER BY sort_order, id")?;
    let live = stmt
        .query_map([], |r| r.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(live.into_iter().filter(|id| wanted.contains(id)).collect())
}

// MARK: - Vault

#[uniffi::export]
impl Vault {
    /// The stored scope; default (disabled, nothing shared) when none is stored.
    pub fn local_api_scope(&self) -> Result<ShareScope, CoreError> {
        let db = self.db()?;
        stored_scope(&db)
    }

    /// Stores what is shared. The token stays as stored (`token_sha256` of
    /// `scope` is ignored), so saving a toggle never restores access; but
    /// switching external access **off** revokes the token. Otherwise an old key
    /// (long forgotten, or passed on) would work again the moment the switch is
    /// turned back on. Turning it on again needs a new key.
    pub fn set_local_api_scope(&self, scope: ShareScope) -> Result<(), CoreError> {
        self.update_local_api_scope(|stored| {
            let switched_off = stored.enabled && !scope.enabled;
            let token = if switched_off {
                String::new()
            } else {
                std::mem::take(&mut stored.token_sha256)
            };
            *stored = ShareScope {
                token_sha256: token,
                ..scope
            }
        })
    }

    /// A new access token for external tools, shown once: only its hash is
    /// stored, and it replaces (revokes) the previous one.
    pub fn create_local_api_token(&self) -> Result<String, CoreError> {
        use rand::RngCore;
        let mut raw = [0u8; 32];
        rand::rng().fill_bytes(&mut raw);
        let token = format!("gcapi_{}", hex::encode(raw));
        self.update_local_api_scope(|s| s.token_sha256 = token_hash(&token))?;
        Ok(token)
    }

    /// Revokes access: socket requests are refused until a new token exists.
    pub fn revoke_local_api_token(&self) -> Result<(), CoreError> {
        self.update_local_api_scope(|s| s.token_sha256.clear())
    }
}

impl Vault {
    /// Read, change and write the stored scope under one vault lock, so a
    /// settings save and a new token at the same moment cannot undo each other.
    fn update_local_api_scope(
        &self,
        change: impl FnOnce(&mut ShareScope),
    ) -> Result<(), CoreError> {
        let db = self.db()?;
        let mut scope = stored_scope(&db)?;
        change(&mut scope);
        scope.account_ids = live_account_ids(&db, &scope.account_ids)?;
        let json = serde_json::to_string(&scope).map_err(|e| CoreError::invalid(e.to_string()))?;
        Vault::set_setting_inner(&db, SCOPE_SETTING, &json)
    }

    /// Answers one JSON request (`{"v":1,"method":"…","params":{…}}`) with
    /// `{"v":1,"result":…}` or `{"v":1,"error":{"code":"…","message":"…"}}`.
    /// In process, without a token (tests, the app itself).
    pub fn local_api_request(&self, request: &str) -> String {
        response(self.local_api_dispatch(request, false))
    }

    /// The same for a request from the socket: it must carry the current token.
    /// Only the Unix-socket server calls it (`server` is `cfg(unix)`).
    #[cfg(unix)]
    pub(crate) fn local_api_socket_request(&self, request: &str) -> String {
        response(self.local_api_dispatch(request, true))
    }

    fn local_api_dispatch(&self, request: &str, require_token: bool) -> ApiResult<Value> {
        let req: Request = serde_json::from_str(request)
            .map_err(|e| ApiError::invalid(format!("malformed request: {e}")))?;
        if req.v.is_some_and(|v| v != API_VERSION) {
            return Err(ApiError::new(
                ApiErrorCode::UnsupportedVersion,
                format!("supported version: {API_VERSION}"),
            ));
        }
        let scope = self.local_api_scope()?;
        // Token first: without it, a socket client learns nothing, not even
        // whether access is switched on.
        if require_token {
            let given = req.token.as_deref().map(token_hash).unwrap_or_default();
            if scope.token_sha256.is_empty() || !same_bytes(&given, &scope.token_sha256) {
                return Err(ApiError::new(
                    ApiErrorCode::Unauthorized,
                    "missing or wrong access token",
                ));
            }
        }
        if !scope.enabled {
            return Err(ApiError::new(
                ApiErrorCode::Disabled,
                "external access is turned off in GiroCrab",
            ));
        }
        fn params<T: for<'de> Deserialize<'de> + Default>(v: Value) -> ApiResult<T> {
            if v.is_null() {
                return Ok(T::default());
            }
            serde_json::from_value(v).map_err(|e| ApiError::invalid(e.to_string()))
        }
        let value = match req.method.as_str() {
            "info" => to_value(self.api_info(&scope)?),
            "accounts" => to_value(self.shared_accounts(&scope)?),
            "categories" => to_value(self.api_categories()?),
            "transactions" => to_value(self.api_transactions(&scope, params(req.params)?)?),
            "summary" => to_value(self.api_summary(&scope, params(req.params)?)?),
            other => {
                return Err(ApiError::new(
                    ApiErrorCode::UnknownMethod,
                    format!("unknown method: {other}"),
                ));
            }
        };
        Ok(value)
    }

    /// Shared accounts that still exist, in sidebar order, with the balances the
    /// sidebar shows (`list_accounts` derives open card amounts and asset values).
    fn shared_accounts(&self, scope: &ShareScope) -> ApiResult<Vec<ApiAccount>> {
        let wanted: HashSet<i64> = scope.account_ids.iter().copied().collect();
        let last_sync: HashMap<i64, Option<i64>> = self
            .list_bank_connections()?
            .into_iter()
            .map(|c| (c.id, c.last_sync_unix))
            .collect();
        let numbers = |s: String| Some(s).filter(|s| scope.share_account_numbers && !s.is_empty());
        Ok(self
            .list_accounts()?
            .into_iter()
            .filter(|a| wanted.contains(&a.id))
            .map(|a| ApiAccount {
                id: a.id,
                kind: a.kind.as_str().to_string(),
                currency: a.currency,
                balance_cents: a.balance_cents,
                balance_date: a.balance_date,
                archived: a.archived_unix.is_some(),
                last_sync_unix: a
                    .connection_id
                    .and_then(|c| last_sync.get(&c).copied().flatten()),
                iban: numbers(a.iban),
                account_number: numbers(a.account_number),
                // Names can carry the account number ("Girokonto 1234567").
                name: shared_text(scope, true, &a.name).unwrap_or_default(),
                bank_name: a.bank_name,
            })
            .collect())
    }

    /// Counts and flags only: one small query, no `list_accounts` (clients poll it).
    fn api_info(&self, scope: &ShareScope) -> ApiResult<ApiInfo> {
        let db = self.db()?;
        let shared: HashSet<i64> = shared_account_ids(&db, scope)?.into_iter().collect();
        let mut stmt = db
            .prepare(
                "SELECT a.id, c.last_sync_unix FROM accounts a
                   JOIN bank_connections c ON c.id = a.connection_id AND c.deleted_ms IS NULL
                  WHERE a.deleted_ms IS NULL",
            )
            .map_err(CoreError::from)?;
        let mut last_sync: Option<i64> = None;
        for row in stmt
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
            })
            .map_err(CoreError::from)?
        {
            let (id, ts) = row.map_err(CoreError::from)?;
            if shared.contains(&id) {
                last_sync = last_sync.max(ts);
            }
        }
        Ok(ApiInfo {
            api_version: API_VERSION,
            core_version: crate::core_version(),
            account_count: shared.len() as u32,
            shares_account_numbers: scope.share_account_numbers,
            shares_counterparty: scope.share_counterparty,
            shares_purpose: scope.share_purpose,
            shares_notes: scope.share_notes,
            last_sync_unix: last_sync,
        })
    }

    /// The category tree. Category names are no account data and are shared whole.
    fn api_categories(&self) -> ApiResult<Vec<ApiCategory>> {
        let db = self.db()?;
        let paths = Vault::category_paths(&db)?;
        let mut stmt = db
            .prepare("SELECT id, parent_id FROM categories WHERE deleted_ms IS NULL")
            .map_err(CoreError::from)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
            })
            .map_err(CoreError::from)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(CoreError::from)?;
        // A deleted parent is not listed: the child reads as top level.
        let parents: HashMap<i64, Option<i64>> = rows
            .iter()
            .map(|&(id, p)| (id, p.filter(|p| paths.contains_key(p))))
            .collect();
        // Steps up the parent chain (a name may contain " › " itself); bounded
        // against a broken, cyclic tree.
        let depth = |mut id: i64| {
            let mut d = 0u32;
            while let Some(Some(p)) = parents.get(&id).filter(|_| d < 64) {
                d += 1;
                id = *p;
            }
            d
        };
        let mut out: Vec<ApiCategory> = rows
            .iter()
            .filter_map(|&(id, _)| {
                let (name, path, _, _) = paths.get(&id)?;
                Some(ApiCategory {
                    id,
                    parent_id: parents.get(&id).copied().flatten(),
                    name: name.clone(),
                    depth: depth(id),
                    path: path.clone(),
                })
            })
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path).then(a.id.cmp(&b.id)));
        Ok(out)
    }

    fn api_transactions(
        &self,
        scope: &ShareScope,
        p: TransactionParams,
    ) -> ApiResult<ApiTransactionPage> {
        let db = self.db()?;
        let shared = shared_account_ids(&db, scope)?;
        let accounts = requested_accounts(&shared, &p.account_ids)?;
        let search = check_search(scope, p.search)?;
        let direction = match p.direction.as_deref() {
            None | Some("all") => BookingDirection::All,
            Some("income") => BookingDirection::Income,
            Some("expense") => BookingDirection::Expense,
            Some(other) => return Err(ApiError::invalid(format!("unknown direction: {other}"))),
        };
        // Accounts only as the hard limit: v1 returns the bookings *on* an account,
        // not the app's loan view (bookings of other accounts that pay the loan).
        let filter = BookingFilter {
            category_id: p.category_id,
            include_subcategories: p.include_subcategories.unwrap_or(true),
            uncategorized_only: p.uncategorized_only,
            search,
            date_from: check_date(p.date_from, "date_from")?,
            date_to: check_date(p.date_to, "date_to")?,
            direction,
            ..Default::default()
        };
        let after = p.cursor.as_deref().map(parse_cursor).transpose()?;
        let limit = p.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
        // One extra row tells whether another page follows.
        let mut rows = self.booking_page_after(
            &db,
            &filter,
            &accounts,
            &shared,
            after.as_ref().map(|(d, id)| (d.as_str(), *id)),
            limit + 1,
        )?;
        let next_cursor = if rows.len() > limit as usize {
            rows.truncate(limit as usize);
            rows.last().map(|t| format!("{}:{}", t.booking_date, t.id))
        } else {
            None
        };
        Ok(ApiTransactionPage {
            transactions: rows.iter().map(|t| api_transaction(scope, t)).collect(),
            next_cursor,
        })
    }

    fn api_summary(&self, scope: &ShareScope, p: SummaryParams) -> ApiResult<ApiSummary> {
        let from = check_date(Some(p.date_from), "date_from")?
            .ok_or_else(|| ApiError::invalid("date_from is required"))?;
        let to = check_date(Some(p.date_to), "date_to")?
            .ok_or_else(|| ApiError::invalid("date_to is required"))?;
        if from > to {
            return Err(ApiError::invalid("date_from is after date_to"));
        }
        // The totals enrich every booking of the range while holding the vault:
        // a bounded range keeps one request short (~2 ms per year on 31k bookings).
        let day = |d: &str| jiff::civil::Date::strptime("%Y-%m-%d", d).ok();
        if let (Some(f), Some(t)) = (day(&from), day(&to))
            && crate::dates::days_between(f, t) > MAX_SUMMARY_DAYS
        {
            return Err(ApiError::invalid(format!(
                "at most {MAX_SUMMARY_DAYS} days per summary"
            )));
        }
        // One lock for both reads, so totals and categories see the same vault.
        let db = self.db()?;
        let shared = shared_account_ids(&db, scope)?;
        let accounts = requested_accounts(&shared, &p.account_ids)?;
        // Totals: the enriched bookings (flow decides the KPI side), like the
        // app's tiles and `cashflow_by_period`.
        let filter = BookingFilter {
            date_from: Some(from.clone()),
            date_to: Some(to.clone()),
            ..Default::default()
        };
        let mut totals: BTreeMap<String, ApiCurrencyTotals> = BTreeMap::new();
        for t in self.booking_page_after(&db, &filter, &accounts, &shared, None, 0)? {
            let e = totals
                .entry(t.currency.clone())
                .or_insert_with(|| ApiCurrencyTotals {
                    currency: t.currency.clone(),
                    income_cents: 0,
                    expense_cents: 0,
                    booking_count: 0,
                });
            e.booking_count += 1;
            match kpi_side(t.flow, t.amount_cents) {
                KpiSide::Income => e.income_cents += t.amount_cents,
                KpiSide::Expense => e.expense_cents -= t.amount_cents,
                KpiSide::Neutral => {}
            }
        }
        // Per category: one split-aware SQL sum over the same bookings; a deleted
        // category reads as none, like in `transactions`.
        let paths = Vault::category_paths(&db)?;
        let ph: Vec<String> = (0..accounts.len()).map(|i| format!("?{}", i + 3)).collect();
        let sql = format!(
            "SELECT c.id, e.currency, SUM(e.amount_cents)
               FROM {} e
               LEFT JOIN categories c ON c.id = e.category_id AND c.deleted_ms IS NULL
              GROUP BY c.id, e.currency",
            crate::planning::effective_sql(
                ", t.currency AS currency",
                &format!(
                    " AND t.account_id IN ({}) AND t.booking_date BETWEEN ?1 AND ?2",
                    ph.join(",")
                ),
            )
        );
        let mut args: Vec<rusqlite::types::Value> = vec![from.clone().into(), to.clone().into()];
        args.extend(accounts.iter().map(|id| (*id).into()));
        let by_category: Vec<ApiCategoryTotal> = if accounts.is_empty() {
            Vec::new()
        } else {
            let mut stmt = db.prepare(&sql).map_err(CoreError::from)?;
            let rows = stmt
                .query_map(params_from_iter(args.iter()), |r| {
                    Ok((
                        r.get::<_, Option<i64>>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })
                .map_err(CoreError::from)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(CoreError::from)?;
            let mut out: Vec<ApiCategoryTotal> = rows
                .into_iter()
                .map(|(category_id, currency, net_cents)| ApiCategoryTotal {
                    category_path: category_id
                        .and_then(|c| paths.get(&c))
                        .map(|(_, path, _, _)| path.clone())
                        .unwrap_or_default(),
                    category_id,
                    currency,
                    net_cents,
                })
                .collect();
            out.sort_by(|a, b| {
                b.net_cents
                    .abs()
                    .cmp(&a.net_cents.abs())
                    .then_with(|| a.category_path.cmp(&b.category_path))
            });
            out
        };
        Ok(ApiSummary {
            date_from: from,
            date_to: to,
            totals: totals.into_values().collect(),
            by_category,
        })
    }
}

/// Ids of the shared accounts that still exist: what bookings and sums need,
/// without the balances and counts of `list_accounts`.
fn shared_account_ids(db: &rusqlite::Connection, scope: &ShareScope) -> ApiResult<Vec<i64>> {
    Ok(live_account_ids(db, &scope.account_ids)?)
}

/// Requested accounts, all of them shared (an unknown id reads as not shared,
/// so the answer never tells which accounts exist); empty = all shared.
fn requested_accounts(shared: &[i64], requested: &[i64]) -> ApiResult<Vec<i64>> {
    if requested.is_empty() {
        return Ok(shared.to_vec());
    }
    if let Some(id) = requested.iter().find(|id| !shared.contains(id)) {
        return Err(ApiError::new(
            ApiErrorCode::NotShared,
            format!("account {id} is not shared"),
        ));
    }
    Ok(requested.to_vec())
}

/// The response envelope of a dispatch result.
pub(crate) fn response(result: ApiResult<Value>) -> String {
    match result {
        Ok(result) => json!({ "v": API_VERSION, "result": result }),
        Err(e) => json!({
            "v": API_VERSION,
            "error": { "code": e.code.as_str(), "message": e.message },
        }),
    }
    .to_string()
}

fn token_hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Comparison in constant time for equal lengths (token hashes).
fn same_bytes(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn to_value<T: Serialize>(v: T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn flow_str(flow: FlowKind) -> &'static str {
    match flow {
        FlowKind::Consumption => "consumption",
        FlowKind::Income => "income",
        FlowKind::Investment => "investment",
        FlowKind::Perk => "perk",
        FlowKind::Transfer => "transfer",
        FlowKind::Loan => "loan",
    }
}

/// A date as stored (`YYYY-MM-DD`, zero-padded: SQL compares the text) or
/// nothing ("" = nothing). `2025-1-5` reads as `2025-01-05`.
fn check_date(date: Option<String>, name: &str) -> ApiResult<Option<String>> {
    match date.filter(|d| !d.is_empty()) {
        None => Ok(None),
        Some(d) => jiff::civil::Date::strptime("%Y-%m-%d", &d)
            .ok()
            .filter(|day| (1000..=9999).contains(&day.year()))
            .map(|day| Some(crate::dates::iso(day)))
            .ok_or_else(|| ApiError::invalid(format!("{name} must be YYYY-MM-DD"))),
    }
}

/// `<booking_date>:<id>` as handed out in `next_cursor`. The date is the stored
/// one, whatever its form (it is only compared, as a bound parameter).
fn parse_cursor(cursor: &str) -> ApiResult<(String, i64)> {
    let bad = || ApiError::invalid("invalid cursor");
    let (date, id) = cursor.rsplit_once(':').ok_or_else(bad)?;
    if date.is_empty() {
        return Err(bad());
    }
    Ok((date.to_string(), id.parse().map_err(|_| bad())?))
}

/// The booking search also matches the fields the scope can hide (counterparty,
/// purpose, own and counterparty IBANs and account numbers, also with
/// letters-only terms such as a bank code), so it needs all of them shared.
fn check_search(scope: &ShareScope, search: Option<String>) -> ApiResult<Option<String>> {
    let Some(q) = search.filter(|s| !s.trim().is_empty()) else {
        return Ok(None);
    };
    if !(scope.share_counterparty && scope.share_purpose && scope.share_account_numbers) {
        return Err(ApiError::new(
            ApiErrorCode::NotShared,
            "search needs counterparty, purpose and account numbers to be shared",
        ));
    }
    Ok(Some(q))
}

/// Text field as shared: `None` when switched off. Unless account numbers are
/// shared, IBANs and every run of 6+ digits are masked, like in the sync log
/// (account, card, customer and contract numbers, also grouped by single
/// spaces, `-`, `/` or `.`); a real calendar date (`15.11.2026`, `15.11.26`,
/// `2026-11-15`) and shorter numbers stay.
fn shared_text(scope: &ShareScope, on: bool, text: &str) -> Option<String> {
    static LONG_NUMBER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\d(?:[ \t\u{00A0}\u{202F}\-/.]?\d){5,}").unwrap());
    if !on {
        return None;
    }
    if scope.share_account_numbers {
        return Some(text.to_string());
    }
    let text = crate::sync_log::replace_ibans(text, |_| "[IBAN]".into());
    Some(
        LONG_NUMBER
            .replace_all(&text, |c: &Captures<'_>| {
                if is_calendar_date(&c[0]) {
                    c[0].to_string()
                } else {
                    "[number]".to_string()
                }
            })
            .into_owned(),
    )
}

/// A whole date of a plausible year, not just something shaped like one
/// (`1234-56-78` is a number).
fn is_calendar_date(s: &str) -> bool {
    ["%d.%m.%Y", "%d.%m.%y", "%Y-%m-%d", "%d/%m/%Y"]
        .iter()
        .filter_map(|f| jiff::civil::Date::strptime(f, s).ok())
        .any(|d| (1900..=2100).contains(&d.year()))
}

fn api_transaction(scope: &ShareScope, t: &Booking) -> ApiTransaction {
    ApiTransaction {
        id: t.id,
        account_id: t.account_id,
        booking_date: t.booking_date.clone(),
        value_date: t.value_date.clone(),
        amount_cents: t.amount_cents,
        currency: t.currency.clone(),
        category_id: t.category_id,
        category_path: t.category_path.clone(),
        // The merchant is read from counterparty and purpose, so it is masked
        // like them ("Darlehen Kto 4009988").
        merchant: shared_text(
            scope,
            scope.share_counterparty && scope.share_purpose,
            &t.merchant_name,
        )
        .unwrap_or_default(),
        flow: flow_str(t.flow).into(),
        // Read from booking text and purpose.
        payment_method: if scope.share_purpose {
            t.payment_method.clone()
        } else {
            String::new()
        },
        pending: t.is_pending,
        counterparty: shared_text(scope, scope.share_counterparty, &t.counterparty),
        counterparty_iban: Some(t.counterparty_iban.clone())
            .filter(|i| scope.share_account_numbers && !i.is_empty()),
        purpose: shared_text(scope, scope.share_purpose, &t.purpose),
        booking_text: shared_text(scope, scope.share_purpose, &t.booking_text),
        note: shared_text(scope, scope.share_notes, &t.note).filter(|n| !n.is_empty()),
    }
}

#[cfg(unix)]
pub mod server;

#[cfg(test)]
mod tests;
