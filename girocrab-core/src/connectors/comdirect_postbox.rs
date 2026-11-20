//! comdirect PostBox next to a FinTS login (GitHub #838).
//!
//! comdirect offers its PostBox (statements, Finanzreport, trade confirmations,
//! tax documents) only through its REST API, while credit cards and transfers
//! need FinTS. [`FintsWithPostbox`] wraps the FinTS connector of a comdirect
//! login: it first logs in to the REST API (same login name and PIN, client id
//! and secret from the settings, one session approval), downloads the PostBox
//! PDFs and logs out again, then runs the FinTS dialog as usual. The downloads
//! run in short batches, each answered with a [`ChallengeKind::Progress`] step,
//! so the app shows how far it is instead of a stalled approval. The REST part
//! never creates accounts or bookings, and a failure there only skips the
//! documents: the FinTS sync goes on.
//!
//! The REST API does not say which account a document belongs to. After the
//! FinTS fetch, [`assign_account`] looks for the IBAN or account number of the
//! fetched accounts in the PDF text and sends securities documents to the
//! depot; everything else goes to the login's first Girokonto
//! ([`default_account`]). The user can move a document to another account.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;

use super::comdirect::ComdirectConnector;
use super::{
    BankConnector, Challenge, ChallengeKind, ChallengeResponse, ConnectContext, FetchResult, Phase,
    RemoteAccount,
};
use crate::documents::RemoteDocument;
use crate::error::CoreResult;
use crate::i18n::{tr, trf};
use crate::models::AccountKind;

/// The REST side of the PostBox fetch: comdirect's API, a fake in tests.
#[async_trait]
pub(crate) trait PostboxSource: Send {
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase>;
    async fn respond(&mut self, ctx: &ConnectContext, r: ChallengeResponse) -> CoreResult<Phase>;
    /// The documents since `since` to download; `None` = listing failed.
    async fn list(&mut self, since: &str) -> Option<Vec<Value>>;
    /// One listed document, without an account; `None` = download failed.
    async fn download(&mut self, entry: &Value) -> Option<RemoteDocument>;
    /// The listed document without its bytes, for one that could not be
    /// downloaded: a stored copy still gets its account.
    fn stub(&self, entry: &Value) -> Option<RemoteDocument>;
    async fn finish(&mut self, ctx: &ConnectContext);
    /// Name of the document source in the app's steps ("PostBox", "Online-Archiv").
    fn label(&self) -> String {
        tr("PostBox").into()
    }
    /// The account a downloaded document belongs to; `None` leaves it without
    /// one. comdirect: the account whose IBAN or number is in the PDF text,
    /// else the login's first Girokonto.
    fn assign<'a>(
        &self,
        doc: &RemoteDocument,
        accounts: &[&'a RemoteAccount],
    ) -> Option<&'a RemoteAccount> {
        let text = crate::documents::extract_pdf_text(&doc.bytes);
        let found = assign_account(&doc.title, &text, accounts);
        crate::depot_debug::log(&format!(
            "comdirect postbox: {} text={} chars -> {}",
            doc.doc_type,
            text.len(),
            found.map_or("default account", |a| kind_name(a.kind))
        ));
        found.or_else(|| default_account(accounts))
    }
}

#[async_trait]
impl PostboxSource for ComdirectConnector {
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        BankConnector::begin(self, ctx).await
    }
    async fn respond(&mut self, ctx: &ConnectContext, r: ChallengeResponse) -> CoreResult<Phase> {
        BankConnector::respond(self, ctx, r).await
    }
    async fn list(&mut self, since: &str) -> Option<Vec<Value>> {
        self.postbox_entries(since).await
    }
    async fn download(&mut self, entry: &Value) -> Option<RemoteDocument> {
        self.postbox_download(entry).await
    }
    fn stub(&self, entry: &Value) -> Option<RemoteDocument> {
        super::comdirect::postbox_stub(entry)
    }
    async fn finish(&mut self, ctx: &ConnectContext) {
        BankConnector::finish(self, ctx).await
    }
}

/// One download batch ends after this many documents or this much time.
const BATCH_DOCS: usize = 20;
const BATCH_TIME: Duration = Duration::from_millis(1500);

/// Where the wrapped sync stands.
enum Stage {
    /// REST login for the PostBox (challenge open).
    Rest,
    /// Approved: downloading the listed documents in batches.
    Download { entries: Vec<Value>, next: usize },
    /// The FinTS dialog.
    Fints,
}

pub(crate) struct FintsWithPostbox {
    fints: Box<dyn BankConnector>,
    rest: Box<dyn PostboxSource>,
    /// Oldest document date to download (`YYYY-MM-DD`); empty = all.
    since: String,
    stage: Stage,
    docs: Vec<RemoteDocument>,
    /// The PostBox listing worked (even when it held nothing new).
    fetched: bool,
}

impl FintsWithPostbox {
    pub(crate) fn new(fints: Box<dyn BankConnector>, since: String) -> Self {
        Self::with_source(fints, Box::new(ComdirectConnector::new()), since)
    }

    pub(crate) fn with_source(
        fints: Box<dyn BankConnector>,
        rest: Box<dyn PostboxSource>,
        since: String,
    ) -> Self {
        Self {
            fints,
            rest,
            since,
            stage: Stage::Rest,
            docs: Vec::new(),
            fetched: false,
        }
    }

    /// Hands an open REST challenge to the app; once the REST login is
    /// approved, lists the PostBox and starts the downloads. A failed login
    /// or listing goes straight on to FinTS.
    async fn after_rest(
        &mut self,
        ctx: &ConnectContext,
        rest: CoreResult<Phase>,
    ) -> CoreResult<Phase> {
        match rest {
            Ok(Phase::Challenge(mut c)) => {
                c.title = trf("{0}: {1}", &[&self.rest.label(), &c.title]);
                return Ok(Phase::Challenge(c));
            }
            Ok(Phase::Pending(mut c)) => {
                c.title = trf("{0}: {1}", &[&self.rest.label(), &c.title]);
                return Ok(Phase::Pending(c));
            }
            Ok(Phase::Ready) => match self.rest.list(&self.since).await {
                Some(entries) => {
                    crate::depot_debug::log(&format!(
                        "postbox: {} documents since '{}'",
                        entries.len(),
                        self.since
                    ));
                    self.fetched = true;
                    if !entries.is_empty() {
                        let total = entries.len();
                        self.stage = Stage::Download { entries, next: 0 };
                        return Ok(progress(&self.rest.label(), 0, total));
                    }
                }
                None => crate::depot_debug::log("postbox: listing failed"),
            },
            Ok(Phase::ConfirmPayee(_)) => {
                crate::depot_debug::log("postbox: unexpected payee check")
            }
            Err(e) => crate::depot_debug::log(&format!(
                "postbox: login failed ({})",
                e.to_string().chars().take(60).collect::<String>()
            )),
        }
        self.start_fints(ctx).await
    }

    /// Downloads the next batch; reports progress or, when done, starts FinTS.
    async fn download_batch(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        let Stage::Download { entries, next } = &mut self.stage else {
            return self.start_fints(ctx).await;
        };
        let started = Instant::now();
        let mut n = 0;
        while *next < entries.len() && n < BATCH_DOCS && started.elapsed() < BATCH_TIME {
            match self.rest.download(&entries[*next]).await {
                Some(d) => self.docs.push(d),
                None => self.docs.extend(self.rest.stub(&entries[*next])),
            }
            *next += 1;
            n += 1;
        }
        if *next < entries.len() {
            return Ok(progress(&self.rest.label(), *next, entries.len()));
        }
        crate::depot_debug::log(&format!(
            "postbox: {} of {} documents downloaded",
            self.docs.iter().filter(|d| !d.bytes.is_empty()).count(),
            entries.len()
        ));
        self.start_fints(ctx).await
    }

    async fn start_fints(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        self.rest.finish(ctx).await;
        self.stage = Stage::Fints;
        self.fints.begin(ctx).await
    }
}

/// The step the app shows while the PostBox downloads run.
fn progress(label: &str, done: usize, total: usize) -> Phase {
    Phase::Pending(Challenge {
        kind: ChallengeKind::Progress,
        title: trf("{0} wird geladen", &[&label]),
        message: trf(
            "Freigabe erhalten. {0} von {1} Dokumenten geladen, danach folgt der Abruf der Konten.",
            &[&done, &total],
        ),
        method_name: String::new(),
        redirect_url: None,
        image: None,
        expires_in_seconds: None,
        poll_interval_seconds: 1,
    })
}

#[async_trait]
impl BankConnector for FintsWithPostbox {
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        self.stage = Stage::Rest;
        let rest = self.rest.begin(ctx).await;
        self.after_rest(ctx, rest).await
    }

    async fn respond(
        &mut self,
        ctx: &ConnectContext,
        response: ChallengeResponse,
    ) -> CoreResult<Phase> {
        match self.stage {
            Stage::Rest => {
                let rest = self.rest.respond(ctx, response).await;
                self.after_rest(ctx, rest).await
            }
            Stage::Download { .. } => self.download_batch(ctx).await,
            Stage::Fints => self.fints.respond(ctx, response).await,
        }
    }

    async fn fetch(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult> {
        let mut out = self.fints.fetch(ctx).await?;
        let accounts: Vec<&RemoteAccount> = out
            .accounts
            .iter()
            .filter(|a| !ctx.skipped_accounts.contains(&a.external_ref))
            .collect();
        let mut matched = 0usize;
        let mut docs = std::mem::take(&mut self.docs);
        for d in &mut docs {
            if let Some(a) = self.rest.assign(d, &accounts) {
                d.account_ref = a.external_ref.clone();
                matched += 1;
            }
        }
        crate::depot_debug::log(&format!(
            "postbox: {matched} of {} documents got an account",
            docs.len()
        ));
        out.documents.extend(docs);
        out.postbox_fetched = self.fetched;
        Ok(out)
    }

    async fn finish(&mut self, ctx: &ConnectContext) {
        self.rest.finish(ctx).await;
        if matches!(self.stage, Stage::Fints) {
            self.fints.finish(ctx).await;
        }
    }
}

/// Where a document goes that matches no single account: the login's first
/// Girokonto (it carries the PostBox at comdirect), else its first account.
pub(crate) fn default_account<'a>(accounts: &[&'a RemoteAccount]) -> Option<&'a RemoteAccount> {
    accounts
        .iter()
        .copied()
        .find(|a| a.kind == AccountKind::Checking)
        .or_else(|| accounts.first().copied())
}

fn kind_name(kind: AccountKind) -> &'static str {
    match kind {
        AccountKind::Checking => "checking",
        AccountKind::Savings => "savings",
        AccountKind::CreditCard => "credit card",
        AccountKind::Brokerage => "depot",
        AccountKind::Loan => "loan",
        AccountKind::Cash => "cash",
        AccountKind::PaymentService => "payment service",
    }
}

/// Title words of securities documents: they go to the depot when its number
/// is in the text, even if the clearing account's IBAN is there too.
const SECURITIES_WORDS: &[&str] = &[
    "wertpapier",
    "depot",
    "dividend",
    "ertrag",
    "aussch",
    "kauf",
    "verkauf",
    "vorabpauschale",
    "kapitalma",
    "steuermitteilung",
    "kosteninformation",
    "kostenausweis",
    "verlust",
    "optionsschein",
    "fälligkeit",
    "faelligkeit",
    "zertifikat",
    "anleihe",
    "einlösung",
    "einloesung",
];

/// The account a PostBox document belongs to: the one whose IBAN or account
/// number appears in its text. Several matching accounts (a Finanzreport
/// lists all of them) or none leave it without an account. A securities
/// document goes to the depot: the one among the matches, or the login's only
/// depot when none matches (comdirect's trade confirmations mostly have text
/// GiroCrab cannot read).
pub(crate) fn assign_account<'a>(
    title: &str,
    text: &str,
    accounts: &[&'a RemoteAccount],
) -> Option<&'a RemoteAccount> {
    let compact: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_uppercase();
    let numbers: Vec<&str> = text
        .split(|c: char| !c.is_ascii_digit())
        .map(|n| n.trim_start_matches('0'))
        .filter(|n| !n.is_empty())
        .collect();
    let by_iban = |a: &RemoteAccount| {
        let iban: String = a.iban.chars().filter(|c| !c.is_whitespace()).collect();
        iban.len() >= 15 && compact.contains(&iban.to_uppercase())
    };
    let by_number = |a: &RemoteAccount| {
        let digits: String = a
            .account_number
            .chars()
            .filter(char::is_ascii_digit)
            .collect();
        let digits = digits.trim_start_matches('0');
        digits.len() >= 6 && numbers.contains(&digits)
    };
    let hits: Vec<&'a RemoteAccount> = accounts
        .iter()
        .copied()
        .filter(|a| by_iban(a) || by_number(a))
        .collect();
    let lower = title.to_lowercase();
    if SECURITIES_WORDS.iter().any(|w| lower.contains(w)) {
        let depots: Vec<&'a RemoteAccount> = hits
            .iter()
            .copied()
            .filter(|a| a.kind == AccountKind::Brokerage)
            .collect();
        let all_depots: Vec<&'a RemoteAccount> = accounts
            .iter()
            .copied()
            .filter(|a| a.kind == AccountKind::Brokerage)
            .collect();
        match (depots.as_slice(), all_depots.as_slice()) {
            ([depot], _) | ([], [depot]) => return Some(depot),
            _ => {}
        }
    }
    let others: Vec<&'a RemoteAccount> = hits
        .into_iter()
        .filter(|a| a.kind != AccountKind::Brokerage)
        .collect();
    let iban_hits: Vec<_> = others.iter().filter(|a| by_iban(a)).collect();
    match (iban_hits.as_slice(), others.as_slice()) {
        ([one], _) => Some(one),
        (_, [one]) => Some(one),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // invented test data, no real accounts
    const GIRO_IBAN: &str = "DE02200411550123456700";
    const JOINT_IBAN: &str = "DE75200411550765432100";

    fn acc(r: &str, iban: &str, number: &str, kind: AccountKind) -> RemoteAccount {
        RemoteAccount {
            external_ref: r.into(),
            name: String::new(),
            owner: String::new(),
            iban: iban.into(),
            account_number: number.into(),
            currency: "EUR".into(),
            kind,
            balance_cents: 0,
            balance_date: String::new(),
        }
    }

    fn accounts() -> Vec<RemoteAccount> {
        vec![
            acc(GIRO_IBAN, GIRO_IBAN, "123456700", AccountKind::Checking),
            acc(JOINT_IBAN, JOINT_IBAN, "765432100", AccountKind::Checking),
            // the depot shares its number with the Girokonto (#805)
            acc("123456700", "", "123456700", AccountKind::Brokerage),
            acc(
                "4871999999990001",
                "",
                "4871999999990001",
                AccountKind::CreditCard,
            ),
        ]
    }

    fn assign(title: &str, text: &str) -> Option<String> {
        let all = accounts();
        let refs: Vec<&RemoteAccount> = all.iter().collect();
        assign_account(title, text, &refs).map(|a| a.external_ref.clone())
    }

    #[test]
    fn statement_goes_to_the_account_with_its_iban() {
        let text = "Kontoauszug Nr. 11/2026 IBAN DE75 2004 1155 0765 4321 00 Kontostand";
        assert_eq!(assign("Kontoauszug", text).as_deref(), Some(JOINT_IBAN));
    }

    #[test]
    fn iban_beats_an_account_number_of_another_account() {
        // Girokonto statement that also names the depot number (= giro number)
        let text = "IBAN DE02 2004 1155 0123 4567 00 Depot 1234567 00";
        assert_eq!(assign("Kontoauszug", text).as_deref(), Some(GIRO_IBAN));
    }

    #[test]
    fn securities_document_goes_to_the_depot() {
        let text = "Depotnummer 123456700 Verrechnungskonto IBAN DE02 2004 1155 0123 4567 00";
        assert_eq!(
            assign("Wertpapierabrechnung Kauf", text).as_deref(),
            Some("123456700")
        );
    }

    #[test]
    fn unreadable_securities_document_goes_to_the_only_depot() {
        assert_eq!(
            assign("Wertpapierabrechnung Verkauf", "").as_deref(),
            Some("123456700")
        );
        // two depots and no number in the text: unclear
        let mut all = accounts();
        all.push(acc("999888700", "", "999888700", AccountKind::Brokerage));
        let refs: Vec<&RemoteAccount> = all.iter().collect();
        assert!(assign_account("Dividendengutschrift", "", &refs).is_none());
        // tax notices and cost information of trades belong to the depot as well
        assert_eq!(assign("Steuermitteilung", "").as_deref(), Some("123456700"));
        assert_eq!(
            assign("Ex-post-Kosteninformation 2025", "").as_deref(),
            Some("123456700")
        );
        assert_eq!(
            assign("Verlustreport 2025", "").as_deref(),
            Some("123456700")
        );
        // a Jahressteuerbescheinigung is not about the depot alone
        assert_eq!(assign("Jahressteuerbescheinigung 2025", ""), None);
    }

    #[test]
    fn account_number_alone_assigns_when_unique() {
        let text = "Kontonummer 0765432100 Mitteilung";
        assert_eq!(assign("Mitteilung", text).as_deref(), Some(JOINT_IBAN));
    }

    #[test]
    fn report_over_several_accounts_stays_unassigned() {
        let text = "Finanzreport DE02200411550123456700 DE75200411550765432100";
        assert_eq!(assign("Finanzreport", text), None);
    }

    #[test]
    fn unknown_or_unreadable_documents_stay_unassigned() {
        assert_eq!(assign("Information", "Neue Bedingungen ab 1.1.2027"), None);
        assert_eq!(assign("Information", ""), None);
    }

    #[test]
    fn short_numbers_and_parts_of_numbers_do_not_match() {
        // 123456700 only inside a longer number, plus small numbers
        let text = "Vorgang 91234567001 Seite 1 von 2";
        assert_eq!(assign("Information", text), None);
    }

    // --- the wrapped sync: REST approval first, then FinTS ---

    use crate::connectors::{Challenge, ChallengeKind};
    use crate::secret::SecretString;
    use std::sync::{Arc, Mutex};

    fn push(title: &str) -> Challenge {
        Challenge {
            kind: ChallengeKind::DecoupledPush,
            title: title.into(),
            message: String::new(),
            method_name: String::new(),
            redirect_url: None,
            image: None,
            expires_in_seconds: None,
            poll_interval_seconds: 1,
        }
    }

    fn ctx(skipped: &[&str]) -> ConnectContext {
        ConnectContext {
            connection_id: 1,
            secret: SecretString::new("pin".into()),
            skipped_accounts: skipped.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    /// FinTS side: Ready without TAN, reports the invented accounts.
    struct Fints(Arc<Mutex<Vec<&'static str>>>);

    #[async_trait]
    impl BankConnector for Fints {
        async fn begin(&mut self, _: &ConnectContext) -> CoreResult<Phase> {
            self.0.lock().unwrap().push("fints begin");
            Ok(Phase::Ready)
        }
        async fn respond(&mut self, _: &ConnectContext, _: ChallengeResponse) -> CoreResult<Phase> {
            Ok(Phase::Ready)
        }
        async fn fetch(&mut self, _: &ConnectContext) -> CoreResult<FetchResult> {
            self.0.lock().unwrap().push("fints fetch");
            Ok(FetchResult {
                accounts: accounts(),
                ..Default::default()
            })
        }
        async fn finish(&mut self, _: &ConnectContext) {
            self.0.lock().unwrap().push("fints finish");
        }
    }

    /// REST side: a push approval that is pending once, then the given result.
    struct Rest {
        log: Arc<Mutex<Vec<&'static str>>>,
        polls: u32,
        login_fails: bool,
        docs: Option<Vec<RemoteDocument>>,
    }

    #[async_trait]
    impl PostboxSource for Rest {
        async fn begin(&mut self, _: &ConnectContext) -> CoreResult<Phase> {
            self.log.lock().unwrap().push("rest begin");
            if self.login_fails {
                return Err(crate::error::CoreError::Authentication {
                    message: "invalid client".into(),
                });
            }
            Ok(Phase::Challenge(push("Freigeben")))
        }
        async fn respond(&mut self, _: &ConnectContext, _: ChallengeResponse) -> CoreResult<Phase> {
            self.polls += 1;
            Ok(if self.polls < 2 {
                Phase::Pending(push("Freigeben"))
            } else {
                Phase::Ready
            })
        }
        async fn list(&mut self, since: &str) -> Option<Vec<Value>> {
            assert_eq!(since, "2026-11-01");
            self.log.lock().unwrap().push("rest list");
            let n = self.docs.as_ref()?.len();
            Some((0..n).map(|i| serde_json::json!({ "i": i })).collect())
        }
        async fn download(&mut self, entry: &Value) -> Option<RemoteDocument> {
            let i = entry["i"].as_u64()? as usize;
            self.docs
                .as_ref()?
                .get(i)
                .cloned()
                .filter(|d| !d.title.starts_with("Fehler"))
        }
        fn stub(&self, entry: &Value) -> Option<RemoteDocument> {
            let i = entry["i"].as_u64()? as usize;
            let mut d = self.docs.as_ref()?.get(i).cloned()?;
            d.bytes.clear();
            Some(d)
        }
        async fn finish(&mut self, _: &ConnectContext) {
            self.log.lock().unwrap().push("rest finish");
        }
    }

    fn doc(title: &str, text: &str) -> RemoteDocument {
        RemoteDocument {
            account_ref: String::new(),
            booking_external_id: None,
            title: title.into(),
            doc_type: "postbox".into(),
            mime_type: "application/pdf".into(),
            file_name: format!("{title}.pdf"),
            bytes: crate::documents::simple_pdf(&[text.to_string()]),
            source_url: format!("comdirect:postbox:{title}"),
            date: "2026-11-10".into(),
        }
    }

    fn wrapped(
        login_fails: bool,
        docs: Option<Vec<RemoteDocument>>,
    ) -> (FintsWithPostbox, Arc<Mutex<Vec<&'static str>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let rest = Rest {
            log: log.clone(),
            polls: 0,
            login_fails,
            docs,
        };
        let w = FintsWithPostbox::with_source(
            Box::new(Fints(log.clone())),
            Box::new(rest),
            "2026-11-01".into(),
        );
        (w, log)
    }

    #[tokio::test]
    async fn postbox_approval_comes_first_then_fints_runs() {
        let docs = vec![
            doc("Kontoauszug", "IBAN DE75 2004 1155 0765 4321 00"),
            doc(
                "Finanzreport",
                "DE02200411550123456700 DE75200411550765432100",
            ),
        ];
        let (mut w, log) = wrapped(false, Some(docs));
        let c = ctx(&[]);
        let Phase::Challenge(ch) = w.begin(&c).await.unwrap() else {
            panic!("expected the PostBox approval")
        };
        assert_eq!(ch.title, "PostBox: Freigeben");
        assert!(matches!(
            w.respond(&c, ChallengeResponse::Poll).await.unwrap(),
            Phase::Pending(_)
        ));
        assert!(log.lock().unwrap().iter().all(|s| s.starts_with("rest")));
        // approved: the listing, then the downloads as a progress step
        let Phase::Pending(p) = w.respond(&c, ChallengeResponse::Poll).await.unwrap() else {
            panic!("expected the download progress")
        };
        assert_eq!(p.kind, ChallengeKind::Progress);
        assert!(p.message.contains("0 von 2"), "{}", p.message);
        // downloaded: REST logout, then the FinTS dialog
        assert!(matches!(
            w.respond(&c, ChallengeResponse::Poll).await.unwrap(),
            Phase::Ready
        ));
        let out = w.fetch(&c).await.unwrap();
        w.finish(&c).await;
        assert_eq!(
            *log.lock().unwrap(),
            [
                "rest begin",
                "rest list",
                "rest finish",
                "fints begin",
                "fints fetch",
                "rest finish",
                "fints finish"
            ]
        );
        assert!(out.postbox_fetched);
        let refs: Vec<&str> = out
            .documents
            .iter()
            .map(|d| d.account_ref.as_str())
            .collect();
        // the Finanzreport matches both accounts: the first Girokonto
        assert_eq!(refs, [JOINT_IBAN, GIRO_IBAN]);
        assert_eq!(
            out.accounts.len(),
            4,
            "the FinTS result itself is unchanged"
        );
    }

    #[tokio::test]
    async fn failed_postbox_login_does_not_stop_the_fints_sync() {
        let (mut w, log) = wrapped(true, None);
        let c = ctx(&[]);
        assert!(matches!(w.begin(&c).await.unwrap(), Phase::Ready));
        let out = w.fetch(&c).await.unwrap();
        assert!(!out.postbox_fetched, "the PostBox stays due");
        assert!(out.documents.is_empty());
        assert_eq!(out.accounts.len(), 4);
        assert_eq!(
            *log.lock().unwrap(),
            ["rest begin", "rest finish", "fints begin", "fints fetch"]
        );
    }

    #[tokio::test]
    async fn many_documents_are_downloaded_in_batches_with_progress() {
        let docs: Vec<_> = (0..45)
            .map(|i| doc(&format!("Information {i}"), "Neue Bedingungen"))
            .collect();
        let (mut w, _) = wrapped(false, Some(docs));
        let c = ctx(&[]);
        w.begin(&c).await.unwrap();
        w.respond(&c, ChallengeResponse::Poll).await.unwrap();
        let mut seen = Vec::new();
        loop {
            match w.respond(&c, ChallengeResponse::Poll).await.unwrap() {
                Phase::Pending(p) => seen.push(p.message),
                Phase::Ready => break,
                _ => panic!("unexpected step"),
            }
        }
        assert_eq!(seen.len(), 3, "{seen:?}");
        for (m, n) in seen.iter().zip(["0 von 45", "20 von 45", "40 von 45"]) {
            assert!(m.contains(n), "{m}");
        }
        let out = w.fetch(&c).await.unwrap();
        assert_eq!(out.documents.len(), 45);
        assert!(out.documents.iter().all(|d| d.account_ref == GIRO_IBAN));
    }

    #[tokio::test]
    async fn failed_download_still_gives_the_stored_copy_its_account() {
        let docs = vec![
            doc("Fehler Steuermitteilung", "nicht geladen"),
            doc("Finanzreport", "x"),
        ];
        let (mut w, _) = wrapped(false, Some(docs));
        let c = ctx(&[]);
        w.begin(&c).await.unwrap();
        w.respond(&c, ChallengeResponse::Poll).await.unwrap();
        w.respond(&c, ChallengeResponse::Poll).await.unwrap();
        w.respond(&c, ChallengeResponse::Poll).await.unwrap();
        let out = w.fetch(&c).await.unwrap();
        assert!(out.documents[0].bytes.is_empty());
        assert_eq!(
            out.documents[0].account_ref, "123456700",
            "title says depot"
        );
        assert_eq!(out.documents[1].account_ref, GIRO_IBAN);
    }

    #[tokio::test]
    async fn empty_postbox_goes_straight_to_fints() {
        let (mut w, log) = wrapped(false, Some(Vec::new()));
        let c = ctx(&[]);
        w.begin(&c).await.unwrap();
        w.respond(&c, ChallengeResponse::Poll).await.unwrap();
        assert!(matches!(
            w.respond(&c, ChallengeResponse::Poll).await.unwrap(),
            Phase::Ready
        ));
        assert!(w.fetch(&c).await.unwrap().postbox_fetched);
        assert_eq!(
            log.lock().unwrap()[..4],
            ["rest begin", "rest list", "rest finish", "fints begin"]
        );
    }

    #[tokio::test]
    async fn failed_postbox_listing_keeps_the_postbox_due() {
        let (mut w, _) = wrapped(false, None);
        let c = ctx(&[]);
        w.begin(&c).await.unwrap();
        w.respond(&c, ChallengeResponse::Poll).await.unwrap();
        w.respond(&c, ChallengeResponse::Poll).await.unwrap();
        assert!(!w.fetch(&c).await.unwrap().postbox_fetched);
    }

    #[tokio::test]
    async fn cancelled_postbox_approval_never_opens_the_fints_dialog() {
        let (mut w, log) = wrapped(false, Some(Vec::new()));
        let c = ctx(&[]);
        w.begin(&c).await.unwrap();
        w.finish(&c).await;
        assert_eq!(*log.lock().unwrap(), ["rest begin", "rest finish"]);
    }

    #[tokio::test]
    async fn documents_of_skipped_accounts_go_to_the_default_account() {
        let docs = vec![doc("Kontoauszug", "IBAN DE75 2004 1155 0765 4321 00")];
        let (mut w, _) = wrapped(false, Some(docs));
        let c = ctx(&[JOINT_IBAN]);
        w.begin(&c).await.unwrap();
        w.respond(&c, ChallengeResponse::Poll).await.unwrap();
        w.respond(&c, ChallengeResponse::Poll).await.unwrap();
        w.respond(&c, ChallengeResponse::Poll).await.unwrap();
        assert_eq!(
            w.fetch(&c).await.unwrap().documents[0].account_ref,
            GIRO_IBAN
        );
    }

    #[test]
    fn default_account_is_the_first_girokonto() {
        let all = accounts();
        let mut refs: Vec<&RemoteAccount> = all.iter().collect();
        refs.reverse(); // card, depot, joint, giro
        assert_eq!(default_account(&refs).unwrap().external_ref, JOINT_IBAN);
        let cards: Vec<&RemoteAccount> = all
            .iter()
            .filter(|a| a.kind == AccountKind::CreditCard)
            .collect();
        assert_eq!(
            default_account(&cards).unwrap().kind,
            AccountKind::CreditCard
        );
        assert!(default_account(&[]).is_none());
    }

    #[test]
    fn skipped_accounts_are_not_candidates() {
        let all = accounts();
        let refs: Vec<&RemoteAccount> = all
            .iter()
            .filter(|a| a.external_ref != JOINT_IBAN)
            .collect();
        let text = "IBAN DE75 2004 1155 0765 4321 00";
        assert!(assign_account("Kontoauszug", text, &refs).is_none());
    }
}
