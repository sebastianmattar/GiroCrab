//! Bank sync flow of the Windows app, independent of Slint so it can be unit-tested.
//!
//! `sync_connection` asks for the PIN (FinTS, comdirect, Trade Republic) only when the connector
//! needs one, hands it to the core and keeps no copy; `drive` then runs the multi-step TAN
//! dialog (code, photoTAN, decoupled push, PSD2 redirect) against a `Stepper`.
//! PIN and TAN live only in memory (`Zeroizing`), are never logged and never written to disk.

use std::time::{Duration, Instant};

use girocrab_core::connectors::SyncSummary;
use girocrab_core::{Challenge, ChallengeKind, ConnectorKind, SyncStep};
use zeroize::Zeroizing;

/// Upper bound for one bank dialog (push approval, TAN entry).
const DIALOG_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// What the app has to ask the user before a connector can sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    /// PIN / password first, then the bank's TAN challenges.
    Secret,
    /// No secret up front (PSD2 aggregation): the bank page in the browser authorises.
    Browser,
    /// Needs an API key or similar that the Windows app cannot collect yet.
    Unsupported,
}

pub fn auth_for(kind: ConnectorKind) -> Auth {
    match kind {
        ConnectorKind::VolksbankKasselGoettingen
        | ConnectorKind::FinTsGeneric
        | ConnectorKind::Direkt1822
        | ConnectorKind::Comdirect
        | ConnectorKind::TradeRepublic => Auth::Secret,
        ConnectorKind::AmericanExpress
        | ConnectorKind::AmazonVisa
        | ConnectorKind::EnableBanking => Auth::Browser,
        _ => Auth::Unsupported,
    }
}

/// The user interface as the flow sees it. `None` / `false` = the user cancelled.
pub trait Ui {
    fn ask_secret(&self, bank: &str) -> Option<Zeroizing<String>>;
    /// TAN / code (also photoTAN, with `challenge.image`, and the PSD2 redirect result).
    fn ask_code(&self, challenge: &Challenge) -> Option<Zeroizing<String>>;
    /// Shows the push approval prompt and waits `interval`; `false` = cancelled.
    fn wait_push(&self, challenge: &Challenge, interval: Duration) -> bool;
    fn open_url(&self, url: &str) -> Result<(), String>;
}

/// The running bank session (`SyncSession` in production, a fake in tests).
pub trait Stepper {
    fn start(&self) -> Result<SyncStep, String>;
    fn submit_code(&self, code: String) -> Result<SyncStep, String>;
    fn poll(&self) -> Result<SyncStep, String>;
    fn cancel(&self);
}

pub const CANCELLED: &str = "Abruf abgebrochen.";

/// Asks for the secret if needed, opens the session with it and drives it to the end.
pub fn sync_connection<S: Stepper, U: Ui>(
    kind: ConnectorKind,
    bank: &str,
    ui: &U,
    begin: impl FnOnce(String) -> Result<S, String>,
) -> Result<SyncSummary, String> {
    let secret = match auth_for(kind) {
        Auth::Secret => match ui.ask_secret(bank) {
            Some(s) if !s.trim().is_empty() => s,
            Some(_) => return Err("Bitte die PIN eingeben.".into()),
            None => return Err(CANCELLED.into()),
        },
        Auth::Browser => Zeroizing::new(String::new()),
        Auth::Unsupported => {
            return Err(format!(
                "{bank}: Dieser Zugang wird von der Windows-App noch nicht unterstützt."
            ));
        }
    };
    // Move the PIN out: the core wipes its copy when the session ends, ours is left empty.
    let mut secret = secret;
    let session = begin(std::mem::take(&mut *secret))?;
    drive(&session, ui)
}

pub fn drive<S: Stepper, U: Ui>(session: &S, ui: &U) -> Result<SyncSummary, String> {
    let deadline = Instant::now() + DIALOG_TIMEOUT;
    let mut step = session.start()?;
    loop {
        if Instant::now() > deadline {
            session.cancel();
            return Err("Zeitüberschreitung: Der Abruf wurde abgebrochen.".into());
        }
        step = match step {
            SyncStep::Finished { summary } => return Ok(summary),
            SyncStep::StillWaiting { challenge } => wait_and_poll(session, ui, &challenge)?,
            SyncStep::NeedsChallenge { challenge } => match challenge.kind {
                ChallengeKind::DecoupledPush | ChallengeKind::Progress => {
                    wait_and_poll(session, ui, &challenge)?
                }
                ChallengeKind::Code | ChallengeKind::ImageCode => submit(session, ui, &challenge)?,
                ChallengeKind::Redirect => {
                    let url = challenge.redirect_url.as_deref().unwrap_or_default();
                    if let Err(e) = safe_https_url(url).and_then(|u| ui.open_url(u)) {
                        session.cancel();
                        return Err(format!("Die Bank-Seite konnte nicht geöffnet werden: {e}"));
                    }
                    submit(session, ui, &challenge)?
                }
            },
        };
    }
}

fn wait_and_poll<S: Stepper, U: Ui>(
    session: &S,
    ui: &U,
    challenge: &Challenge,
) -> Result<SyncStep, String> {
    let interval = Duration::from_secs(challenge.poll_interval_seconds.clamp(1, 30) as u64);
    if !ui.wait_push(challenge, interval) {
        session.cancel();
        return Err(CANCELLED.into());
    }
    session.poll()
}

fn submit<S: Stepper, U: Ui>(
    session: &S,
    ui: &U,
    challenge: &Challenge,
) -> Result<SyncStep, String> {
    match ui.ask_code(challenge) {
        Some(mut code) if !code.trim().is_empty() => {
            session.submit_code(std::mem::take(&mut *code).trim().to_string())
        }
        _ => {
            session.cancel();
            Err(CANCELLED.into())
        }
    }
}

/// Only plain https URLs are handed to the system: no other scheme, no control characters,
/// whitespace or quotes.
pub fn safe_https_url(url: &str) -> Result<&str, String> {
    let url = url.trim();
    let ok = url.len() > "https://".len()
        && url.len() <= 4096
        && url.starts_with("https://")
        && !url
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '"' || c == '\\');
    if ok {
        Ok(url)
    } else {
        Err("ungültige Adresse (nur https erlaubt)".into())
    }
}

/// Opens `url` in the user's default browser.
pub fn open_in_browser(url: &str) -> Result<(), String> {
    let url = safe_https_url(url)?;
    #[cfg(windows)]
    // `rundll32 url.dll,FileProtocolHandler` is what `start` does, but without a cmd.exe
    // command line, where `&` and `%VAR%` in a bank URL would be interpreted.
    let mut cmd = {
        let mut c = std::process::Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    fn challenge(kind: ChallengeKind, url: Option<&str>) -> Challenge {
        Challenge {
            kind,
            title: "TAN".into(),
            message: "Bitte freigeben".into(),
            method_name: "pushTAN".into(),
            redirect_url: url.map(String::from),
            image: None,
            expires_in_seconds: None,
            poll_interval_seconds: 2,
        }
    }

    fn summary() -> SyncSummary {
        SyncSummary {
            accounts_updated: 1,
            transactions_imported: 3,
            transactions_categorized: 0,
            transactions_skipped_archived: 0,
            new_accounts: vec![],
            revoked_accounts: vec![],
            postbox_documents: None,
            warnings: vec![],
        }
    }

    #[derive(Default)]
    struct FakeSession {
        steps: RefCell<VecDeque<Result<SyncStep, String>>>,
        log: RefCell<Vec<String>>,
    }
    impl FakeSession {
        fn with(steps: Vec<Result<SyncStep, String>>) -> Self {
            Self {
                steps: RefCell::new(steps.into()),
                log: RefCell::default(),
            }
        }
        fn next(&self, what: String) -> Result<SyncStep, String> {
            self.log.borrow_mut().push(what);
            self.steps
                .borrow_mut()
                .pop_front()
                .expect("unexpected call")
        }
    }
    impl Stepper for FakeSession {
        fn start(&self) -> Result<SyncStep, String> {
            self.next("start".into())
        }
        fn submit_code(&self, code: String) -> Result<SyncStep, String> {
            self.next(format!("code:{code}"))
        }
        fn poll(&self) -> Result<SyncStep, String> {
            self.next("poll".into())
        }
        fn cancel(&self) {
            self.log.borrow_mut().push("cancel".into());
        }
    }

    #[derive(Default)]
    struct FakeUi {
        pin: Option<&'static str>,
        codes: RefCell<VecDeque<Option<&'static str>>>,
        push_ok: bool,
        opened: RefCell<Vec<String>>,
        asked_pin: RefCell<u32>,
    }
    impl Ui for FakeUi {
        fn ask_secret(&self, _bank: &str) -> Option<Zeroizing<String>> {
            *self.asked_pin.borrow_mut() += 1;
            self.pin.map(|p| Zeroizing::new(p.to_string()))
        }
        fn ask_code(&self, _c: &Challenge) -> Option<Zeroizing<String>> {
            self.codes
                .borrow_mut()
                .pop_front()
                .flatten()
                .map(|c| Zeroizing::new(c.to_string()))
        }
        fn wait_push(&self, _c: &Challenge, _i: Duration) -> bool {
            self.push_ok
        }
        fn open_url(&self, url: &str) -> Result<(), String> {
            self.opened.borrow_mut().push(url.into());
            Ok(())
        }
    }

    fn need(kind: ChallengeKind, url: Option<&str>) -> Result<SyncStep, String> {
        Ok(SyncStep::NeedsChallenge {
            challenge: challenge(kind, url),
        })
    }
    fn done() -> Result<SyncStep, String> {
        Ok(SyncStep::Finished { summary: summary() })
    }

    #[test]
    fn fints_pin_then_code_tan_finishes() {
        let ui = FakeUi {
            pin: Some("12345"),
            codes: RefCell::new([Some(" 654321 ")].into()),
            ..Default::default()
        };
        let session = FakeSession::with(vec![need(ChallengeKind::Code, None), done()]);
        let mut got_pin = String::new();
        let out = sync_connection(ConnectorKind::FinTsGeneric, "Testbank", &ui, |pin| {
            got_pin = pin;
            Ok(session)
        });
        assert_eq!(out.unwrap().transactions_imported, 3);
        assert_eq!(got_pin, "12345");
    }

    #[test]
    fn tan_is_trimmed_and_submitted_once() {
        let ui = FakeUi {
            codes: RefCell::new([Some(" 654321 ")].into()),
            ..Default::default()
        };
        let session = FakeSession::with(vec![need(ChallengeKind::ImageCode, None), done()]);
        drive(&session, &ui).unwrap();
        assert_eq!(*session.log.borrow(), ["start", "code:654321"]);
    }

    #[test]
    fn cancelled_pin_never_opens_a_session() {
        let ui = FakeUi {
            pin: None,
            ..Default::default()
        };
        let out = sync_connection::<FakeSession, _>(ConnectorKind::Direkt1822, "Bank", &ui, |_| {
            panic!("session must not start without a PIN")
        });
        assert_eq!(out.unwrap_err(), CANCELLED);
    }

    #[test]
    fn empty_pin_is_rejected_before_the_bank_is_contacted() {
        let ui = FakeUi {
            pin: Some("  "),
            ..Default::default()
        };
        let out = sync_connection::<FakeSession, _>(ConnectorKind::Comdirect, "Bank", &ui, |_| {
            panic!("no session for an empty PIN")
        });
        assert!(out.is_err());
    }

    #[test]
    fn cancelled_tan_cancels_the_session() {
        let ui = FakeUi {
            codes: RefCell::new([None].into()),
            ..Default::default()
        };
        let session = FakeSession::with(vec![need(ChallengeKind::Code, None)]);
        assert_eq!(drive(&session, &ui).unwrap_err(), CANCELLED);
        assert_eq!(*session.log.borrow(), ["start", "cancel"]);
    }

    #[test]
    fn decoupled_push_polls_until_approved() {
        let ui = FakeUi {
            push_ok: true,
            ..Default::default()
        };
        let waiting = || {
            Ok(SyncStep::StillWaiting {
                challenge: challenge(ChallengeKind::DecoupledPush, None),
            })
        };
        let session = FakeSession::with(vec![
            need(ChallengeKind::DecoupledPush, None),
            waiting(),
            done(),
        ]);
        drive(&session, &ui).unwrap();
        assert_eq!(*session.log.borrow(), ["start", "poll", "poll"]);
    }

    #[test]
    fn cancelling_the_push_prompt_stops_polling() {
        let ui = FakeUi {
            push_ok: false,
            ..Default::default()
        };
        let session = FakeSession::with(vec![need(ChallengeKind::DecoupledPush, None)]);
        assert_eq!(drive(&session, &ui).unwrap_err(), CANCELLED);
        assert_eq!(*session.log.borrow(), ["start", "cancel"]);
    }

    #[test]
    fn psd2_redirect_opens_browser_then_submits_pasted_url() {
        let url = "https://bank.example/auth?client=1&state=abc";
        let pasted = "https://girocrab.de/callback?code=XYZ&state=abc";
        let ui = FakeUi {
            codes: RefCell::new([Some(pasted)].into()),
            ..Default::default()
        };
        let session = FakeSession::with(vec![need(ChallengeKind::Redirect, Some(url)), done()]);
        let out = sync_connection(ConnectorKind::EnableBanking, "N26", &ui, |pin| {
            assert!(pin.is_empty(), "PSD2 asks no PIN");
            Ok(session)
        });
        assert!(out.is_ok());
        assert_eq!(*ui.opened.borrow(), [url]);
        assert_eq!(*ui.asked_pin.borrow(), 0);
    }

    #[test]
    fn psd2_redirect_rejects_non_https_urls() {
        let ui = FakeUi::default();
        let session = FakeSession::with(vec![need(
            ChallengeKind::Redirect,
            Some("file:///C:/Windows/calc.exe"),
        )]);
        assert!(drive(&session, &ui).is_err());
        assert!(ui.opened.borrow().is_empty());
        assert_eq!(*session.log.borrow(), ["start", "cancel"]);
    }

    #[test]
    fn url_check() {
        assert!(safe_https_url("https://a.example/x?y=1&z=%20").is_ok());
        for bad in [
            "http://a.example",
            "https://",
            "https://a b",
            "https://a\"&calc",
            "javascript:1",
            "",
        ] {
            assert!(safe_https_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn connector_auth_modes() {
        assert_eq!(auth_for(ConnectorKind::FinTsGeneric), Auth::Secret);
        assert_eq!(auth_for(ConnectorKind::EnableBanking), Auth::Browser);
        assert_eq!(auth_for(ConnectorKind::PayPal), Auth::Unsupported);
    }
}
