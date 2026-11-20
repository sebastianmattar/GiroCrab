//! Advanzia browser adapter, ported from the private 9a3d7a6 implementation (#177).
//! Authentication stays in WebKit. Only transaction JSON and a checked scalar balance
//! cross the FFI boundary. No cookie, password or OTP enters this module.
use super::{
    BankConnector, ChallengeResponse, ConnectContext, FetchResult, Phase, RemoteAccount,
    SyncSummary,
};
use crate::{
    error::{CoreError, CoreResult},
    i18n::tr,
    models::{AccountKind, ConnectorKind},
    secret::SecretJson,
    vault::{Vault, connections::store_state},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
mod compatibility;
pub(crate) use compatibility::is_hold as is_advanzia_hold;
mod parser;
mod reconcile;
mod settings;
use parser::{parse_balance, parse_captures};
pub(crate) use reconcile::settle_holds;
#[cfg(test)]
mod compatibility_tests;
#[cfg(test)]
mod tests;

pub(crate) const HOLD_PREFIX: &str = "pending-hold-portal-advanzia-";

fn invalid(message: &'static str) -> CoreError {
    CoreError::invalid(format!("Advanzia: {}", tr(message)))
}
fn account_ref(id: &str) -> String {
    format!("advanzia:{id}")
}
fn uuid(raw: &str) -> CoreResult<String> {
    uuid::Uuid::parse_str(raw)
        .map(|u| u.to_string())
        .map_err(|_| invalid("Ungültige Konto-Zuordnung."))
}

pub(crate) fn bank_descriptor(kind: ConnectorKind) -> super::BankDescriptor {
    super::BankDescriptor {
        kind, name: "Advanzia Bank".into(), protocol: tr("Web-Login mit SMS- oder App-Freigabe").into(),
        bank_code: String::new(), sf_symbol: "creditcard.fill".into(),
        login_label: tr("Advanzia-Benutzername").into(),
        secret_label: tr("(Anmeldung im Browser-Fenster)").into(), required_settings: vec![],
        note: tr("Inoffizielle Web-Schnittstelle: Kontostand und Kartenumsätze. SMS-Code im Bankfenster eingeben oder in der Advanzia-App bestätigen. Eine Karte je Zugang; getrennte Sitzungen für verschiedene Logins. Die Nutzung kann gegen Nutzungsbedingungen verstoßen.").into(),
    }
}

pub struct WebOnlyConnector;
#[async_trait::async_trait]
impl BankConnector for WebOnlyConnector {
    async fn begin(&mut self, _: &ConnectContext) -> CoreResult<Phase> {
        Err(CoreError::BrowserLoginRequired {
            message: tr(
                "Bitte bei Advanzia im Bankfenster anmelden und die gewählte Freigabe bestätigen.",
            )
            .into(),
        })
    }
    async fn respond(&mut self, ctx: &ConnectContext, _: ChallengeResponse) -> CoreResult<Phase> {
        self.begin(ctx).await
    }
    async fn fetch(&mut self, _: &ConnectContext) -> CoreResult<FetchResult> {
        Err(CoreError::BrowserLoginRequired {
            message: tr("Bitte Advanzia im Bankfenster abrufen.").into(),
        })
    }
}

#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AdvanziaProfile {
    /// Random per-vault connection namespace, never a sequential connection id.
    pub profile_id: String,
    pub bank_account_id: Option<String>,
    pub has_account: bool,
    pub auth_method: String,
    /// Existing legacy account requiring one explicit confirmation, never a guessed card.
    pub legacy_account_id: Option<i64>,
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    profile_id: String,
    #[serde(default = "settings::default_method")]
    auth_method: String,
    #[serde(default)]
    login_scope: String,
    bank_account_id: Option<String>,
    #[serde(default)]
    settled_posts: HashSet<String>,
    // Import-only command, removed before state is stored; binds atomically with bookings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_account_id: Option<i64>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    import_aliases: HashMap<String, String>,
}
fn connection(db: &Connection, id: i64) -> CoreResult<(String, State)> {
    let row: Option<(String, String, SecretJson)> = db.query_row(
        "SELECT connector,display_name,state_json FROM bank_connections WHERE id=?1 AND deleted_ms IS NULL",
        [id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    let Some((kind, name, raw)) = row else {
        return Err(invalid("Bankzugang nicht gefunden."));
    };
    if kind != "advanzia" {
        return Err(invalid("Falscher Bankzugang."));
    }
    let state = if raw.is_empty() {
        State::default()
    } else {
        raw.decode()
            .map_err(|_| invalid("Ungültiger Sitzungszustand."))?
    };
    Ok((name, state))
}
fn login_scope(db: &Connection, id: i64) -> CoreResult<String> {
    use sha2::{Digest, Sha256};
    let login: String = db.query_row(
        "SELECT login_name FROM bank_connections WHERE id=?1",
        [id],
        |r| r.get(0),
    )?;
    Ok(hex::encode(Sha256::digest(login.as_bytes())))
}
fn encode_state(state: &State) -> CoreResult<SecretJson> {
    SecretJson::encode(state).map_err(|_| invalid("Ungültiger Sitzungszustand."))
}

fn decode_state(state: Option<&SecretJson>) -> CoreResult<State> {
    state
        .and_then(|s| s.decode().ok())
        .ok_or_else(|| invalid("Ungültiger Sitzungszustand."))
}

/// Runs inside store_fetch's transaction, before any adoption or account selection.
pub(crate) fn prepare_import(db: &Connection, id: i64, data: &mut FetchResult) -> CoreResult<()> {
    let (_, stored) = connection(db, id)?;
    let next = decode_state(data.state.as_ref())?;
    if stored.login_scope != login_scope(db, id)?
        || stored.profile_id.is_empty()
        || stored.profile_id != next.profile_id
        || stored
            .bank_account_id
            .as_ref()
            .is_some_and(|a| Some(a) != next.bank_account_id.as_ref())
    {
        return Err(invalid(
            "Die Konto-Zuordnung hat sich geändert. Bitte erneut anmelden.",
        ));
    }
    let bank = next
        .bank_account_id
        .as_deref()
        .ok_or_else(|| invalid("Konto-Zuordnung fehlt."))?;
    let (ref_id, legacy) = compatibility::reference(db, id, &stored, bank, next.target_account_id)?;
    if data.accounts.len() != 1 || data.accounts[0].external_ref != ref_id {
        return Err(invalid("Ungültige Konto-Zuordnung."));
    }
    if let Some(target) = next
        .target_account_id
        .filter(|target| Some(*target) != legacy)
    {
        let eligible: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM accounts WHERE id=?1 AND connection_id IS NULL AND kind='credit_card' AND currency='EUR' AND deleted_ms IS NULL AND archived_unix IS NULL)", [target], |r| r.get(0))?;
        let existing: i64 = db.query_row(
            "SELECT count(*) FROM accounts
              WHERE connection_id=?1 AND (deleted_ms IS NULL OR external_ref=?2)",
            params![id, ref_id],
            |r| r.get(0),
        )?;
        if !eligible || existing != 0 {
            return Err(invalid(
                "Nur ein unverbundenes aktives EUR-Kreditkartenkonto darf zugeordnet werden.",
            ));
        }
        db.execute(
            "UPDATE accounts SET connection_id=?1,external_ref=?2 WHERE id=?3",
            params![id, ref_id, target],
        )?;
    }
    // Re-adding a login can reuse a connection with deleted accounts. A
    // different deleted reference must not block the newly verified card.
    let conflict: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM accounts
                        WHERE connection_id=?1 AND deleted_ms IS NULL AND external_ref<>?2)",
        params![id, ref_id],
        |r| r.get(0),
    )?;
    if conflict {
        return Err(invalid(
            "Das Advanzia-Konto ist einem anderen Bankkonto zugeordnet. Bitte die Kontozuordnung prüfen; es wurden keine Daten verändert.",
        ));
    }
    compatibility::prepare_ids(db, id, data, &next.import_aliases, legacy.is_some())?;
    // A concurrent import must not overwrite the reconciliation ledger.
    if !stored.settled_posts.is_subset(&next.settled_posts) {
        return Err(invalid("Bitte den Abruf erneut starten."));
    }
    Ok(())
}

#[uniffi::export]
impl Vault {
    /// Initializes only a random namespace in the encrypted vault. No bank access.
    pub fn advanzia_profile(&self, connection_id: i64) -> CoreResult<AdvanziaProfile> {
        let db = self.db()?;
        let (_, mut state) = connection(&db, connection_id)?;
        let scope = login_scope(&db, connection_id)?;
        if state.profile_id.is_empty() || state.login_scope != scope {
            state.login_scope = scope;
            state.profile_id = uuid::Uuid::new_v4().to_string();
            store_state(&db, connection_id, &encode_state(&state)?)?;
        }
        let has_account = db.query_row(
            // Canonical references can be revived by store_fetch after re-adding
            // the login. Deleted legacy references do not identify a bank card.
            "SELECT EXISTS(SELECT 1 FROM accounts
                            WHERE connection_id=?1 AND (deleted_ms IS NULL OR external_ref LIKE 'advanzia:%'))",
            [connection_id],
            |r| r.get(0),
        )?;
        let legacy = compatibility::legacy_account(&db, connection_id)?;
        let awaiting_confirmation = legacy.is_some() && state.bank_account_id.is_none();
        Ok(AdvanziaProfile {
            profile_id: state.profile_id,
            bank_account_id: state.bank_account_id,
            has_account: has_account && !awaiting_confirmation,
            legacy_account_id: legacy.map(|(id, _)| id),
            auth_method: if state.auth_method.is_empty() {
                settings::default_method()
            } else {
                state.auth_method
            },
        })
    }

    /// The browser's last body is the freshly requested current snapshot. Older
    /// bodies contribute posted history only. Balance is required: no estimated saldo.
    pub fn import_advanzia_capture(
        self: Arc<Self>,
        connection_id: i64,
        bank_account_id: String,
        profile_id: String,
        json_bodies: Vec<String>,
        balance_json_number: String,
        target_account_id: Option<i64>,
    ) -> CoreResult<SyncSummary> {
        crate::banking_gate::check_connector("advanzia")?;
        let bank_id = uuid(&bank_account_id)?;
        let entries = parse_captures(&json_bodies)?;
        let balance = parse_balance(&balance_json_number)?;
        let (name, mut state, reference) = {
            let db = self.db()?;
            let (name, state) = connection(&db, connection_id)?;
            let (reference, _) =
                compatibility::reference(&db, connection_id, &state, &bank_id, target_account_id)?;
            (name, state, reference)
        };
        if state.profile_id.is_empty() || state.profile_id != profile_id {
            return Err(invalid("Bitte das Bankfenster erneut öffnen."));
        }
        state.bank_account_id = Some(bank_id.clone());
        state.target_account_id = target_account_id;
        state.import_aliases = entries
            .iter()
            .map(|e| (e.id.clone(), e.legacy_id.clone()))
            .collect();
        let out = FetchResult {
            accounts: vec![RemoteAccount {
                external_ref: reference.clone(),
                name,
                owner: String::new(),
                iban: String::new(),
                account_number: String::new(),
                currency: "EUR".into(),
                kind: AccountKind::CreditCard,
                balance_cents: balance,
                balance_date: super::today(),
            }],
            transactions: entries.into_iter().map(|e| e.remote(&reference)).collect(),
            balances_reported: vec![reference],
            state: Some(encode_state(&state)?),
            // A truncated transaction window is NOT a complete open-hold inventory.
            hold_prefix: None,
            ..Default::default()
        };
        super::store_fetch(&self, ConnectorKind::Advanzia, connection_id, out)
    }

    /// Unresolved holds stay visible until a unique posting arrives or the user
    /// explicitly discards them. Absence from a partial snapshot never deletes one.
    pub fn advanzia_open_holds(
        &self,
        connection_id: i64,
    ) -> CoreResult<Vec<crate::importers::portal_import::PortalHold>> {
        let db = self.db()?;
        connection(&db, connection_id)?;
        let mut q = db.prepare("SELECT t.id,t.booking_date,t.amount_cents,t.counterparty FROM transactions t JOIN accounts a ON a.id=t.account_id WHERE a.connection_id=?1 AND a.deleted_ms IS NULL AND t.deleted_ms IS NULL AND (t.external_id LIKE 'pending-hold-portal-advanzia-%' OR t.external_id LIKE 'advanzia-hold-%') ORDER BY t.booking_date,t.id")?;
        let rows = q.query_map([connection_id], |r| {
            Ok(crate::importers::portal_import::PortalHold {
                id: r.get(0)?,
                booking_date: r.get(1)?,
                amount_cents: r.get(2)?,
                counterparty: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
    pub fn discard_advanzia_hold(&self, connection_id: i64, booking_id: i64) -> CoreResult<()> {
        let db = self.db()?;
        connection(&db, connection_id)?;
        let eligible: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM transactions t JOIN accounts a ON a.id=t.account_id WHERE a.connection_id=?1 AND a.deleted_ms IS NULL AND a.archived_unix IS NULL AND t.id=?2 AND t.deleted_ms IS NULL AND (t.external_id LIKE 'pending-hold-portal-advanzia-%' OR t.external_id LIKE 'advanzia-hold-%') AND NOT EXISTS(SELECT 1 FROM booking_documents WHERE booking_id=t.id) AND NOT EXISTS(SELECT 1 FROM transaction_splits WHERE transaction_id=t.id))",params![connection_id,booking_id],|r|r.get(0))?;
        if !eligible {
            return Err(invalid("Diese Vormerkung darf nicht verworfen werden."));
        }
        crate::sync_merge::soft_delete_booking(&db, booking_id)
    }
}
