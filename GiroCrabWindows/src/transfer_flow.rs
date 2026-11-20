//! SEPA transfer dialog flow: drives a `TransferSession` through the same PIN / TAN / push / redirect
//! prompts as the bank sync (`sync_flow::Ui`) plus the recipient name check, independent of Slint.
//! Nothing is retried automatically: an unclear outcome is reported and never resent.

use std::time::{Duration, Instant};

use girocrab_core::connectors::PayeeCheck;
use girocrab_core::{Challenge, ChallengeKind, TransferStep};

use crate::sync_flow::{CANCELLED, Ui, safe_https_url};

const DIALOG_TIMEOUT: Duration = Duration::from_secs(10 * 60);

pub trait TransferUi: Ui {
    /// The recipient name check did not confirm the payee: `true` = send anyway.
    fn confirm_payee(&self, check: &PayeeCheck) -> bool;
}

pub trait TransferStepper {
    fn start(&self) -> Result<TransferStep, String>;
    fn submit_code(&self, code: String) -> Result<TransferStep, String>;
    fn poll(&self) -> Result<TransferStep, String>;
    fn confirm_payee(&self) -> Result<TransferStep, String>;
    fn cancel(&self);
}

pub fn payee_text(c: &PayeeCheck) -> String {
    use girocrab_core::connectors::PayeeMatch::*;
    let mut s = match c.result {
        Match => "Der Name stimmt mit dem Kontoinhaber überein.".to_string(),
        CloseMatch => format!(
            "Der Name weicht leicht ab. Kontoinhaber laut Empfängerbank: {}.",
            c.bank_name.as_deref().unwrap_or("unbekannt")
        ),
        NoMatch => "Der Name stimmt NICHT mit dem Kontoinhaber überein.".to_string(),
        NotPossible => format!(
            "Die Namensprüfung war nicht möglich{}.",
            c.reason
                .as_deref()
                .map(|r| format!(" ({r})"))
                .unwrap_or_default()
        ),
    };
    if let Some(n) = c.notice.as_deref().filter(|n| !n.is_empty()) {
        s.push('\n');
        s.push_str(n);
    }
    s
}

/// Runs the transfer to its end; `Ok` = text for the status line.
pub fn drive_transfer<S: TransferStepper, U: TransferUi>(
    session: &S,
    ui: &U,
) -> Result<String, String> {
    let deadline = Instant::now() + DIALOG_TIMEOUT;
    let mut step = session.start()?;
    loop {
        if Instant::now() > deadline {
            session.cancel();
            return Err("Zeitüberschreitung: Die Überweisung wurde abgebrochen.".into());
        }
        step = match step {
            TransferStep::Finished { outcome } => {
                let mut msg = format!(
                    "Überweisung gesendet ({}). Referenz: {}",
                    if outcome.instant { "Echtzeit" } else { "SEPA" },
                    outcome.end_to_end_id
                );
                if let Some(e) = outcome.record_error {
                    msg += &format!(
                        "\nSie wurde nicht gespeichert ({e}) und erscheint mit dem nächsten Abruf. NICHT erneut senden."
                    );
                }
                return Ok(msg);
            }
            TransferStep::InstantDeclined { reason } => {
                return Err(format!("Nicht ausgeführt: {reason}"));
            }
            TransferStep::OutcomeUnknown { reason, .. } => {
                return Err(format!(
                    "Ergebnis unklar: {reason} Bitte das Konto prüfen und NICHT erneut senden."
                ));
            }
            TransferStep::NeedsPayeeConfirmation { check } => {
                if ui.confirm_payee(&check) {
                    session.confirm_payee()?
                } else {
                    session.cancel();
                    return Err(CANCELLED_TRANSFER.into());
                }
            }
            TransferStep::StillWaiting { challenge } => wait_and_poll(session, ui, &challenge)?,
            TransferStep::NeedsChallenge { challenge } => match challenge.kind {
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

pub const CANCELLED_TRANSFER: &str = "Überweisung abgebrochen.";

fn wait_and_poll<S: TransferStepper, U: TransferUi>(
    session: &S,
    ui: &U,
    c: &Challenge,
) -> Result<TransferStep, String> {
    if !ui.wait_push(
        c,
        Duration::from_secs(c.poll_interval_seconds.clamp(1, 30) as u64),
    ) {
        session.cancel();
        return Err(CANCELLED_TRANSFER.into());
    }
    session.poll()
}

fn submit<S: TransferStepper, U: TransferUi>(
    session: &S,
    ui: &U,
    c: &Challenge,
) -> Result<TransferStep, String> {
    match ui.ask_code(c) {
        Some(mut code) if !code.trim().is_empty() => {
            session.submit_code(std::mem::take(&mut *code).trim().to_string())
        }
        _ => {
            session.cancel();
            Err(CANCELLED.replace("Abruf", "Überweisung"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use girocrab_core::connectors::{PayeeMatch, TransferOutcome};
    use std::cell::RefCell;
    use zeroize::Zeroizing;

    struct Fake {
        steps: RefCell<Vec<TransferStep>>,
        calls: RefCell<Vec<String>>,
    }
    impl Fake {
        fn new(mut steps: Vec<TransferStep>) -> Self {
            steps.reverse();
            Fake {
                steps: RefCell::new(steps),
                calls: RefCell::new(vec![]),
            }
        }
        fn next(&self, what: &str) -> Result<TransferStep, String> {
            self.calls.borrow_mut().push(what.into());
            Ok(self.steps.borrow_mut().pop().expect("no more steps"))
        }
    }
    impl TransferStepper for Fake {
        fn start(&self) -> Result<TransferStep, String> {
            self.next("start")
        }
        fn submit_code(&self, code: String) -> Result<TransferStep, String> {
            self.next(&format!("code:{code}"))
        }
        fn poll(&self) -> Result<TransferStep, String> {
            self.next("poll")
        }
        fn confirm_payee(&self) -> Result<TransferStep, String> {
            self.next("confirm")
        }
        fn cancel(&self) {
            self.calls.borrow_mut().push("cancel".into());
        }
    }

    struct FakeUi {
        code: Option<&'static str>,
        payee_ok: bool,
    }
    impl Ui for FakeUi {
        fn ask_secret(&self, _: &str) -> Option<Zeroizing<String>> {
            None
        }
        fn ask_code(&self, _: &Challenge) -> Option<Zeroizing<String>> {
            self.code.map(|c| Zeroizing::new(c.to_string()))
        }
        fn wait_push(&self, _: &Challenge, _: Duration) -> bool {
            true
        }
        fn open_url(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
    }
    impl TransferUi for FakeUi {
        fn confirm_payee(&self, _: &PayeeCheck) -> bool {
            self.payee_ok
        }
    }

    fn challenge(kind: ChallengeKind) -> Challenge {
        Challenge {
            kind,
            title: String::new(),
            message: String::new(),
            method_name: String::new(),
            redirect_url: None,
            image: None,
            expires_in_seconds: None,
            poll_interval_seconds: 1,
        }
    }

    fn done() -> TransferStep {
        TransferStep::Finished {
            outcome: TransferOutcome {
                instant: false,
                end_to_end_id: "GC123".into(),
                record_error: None,
            },
        }
    }

    fn check() -> PayeeCheck {
        PayeeCheck {
            result: PayeeMatch::NoMatch,
            entered_name: "A".into(),
            iban: "DE00".into(),
            bank_name: None,
            reason: None,
            notice: Some("Hinweis der Bank".into()),
        }
    }

    #[test]
    fn tan_is_submitted_trimmed_then_finishes() {
        let s = Fake::new(vec![
            TransferStep::NeedsChallenge {
                challenge: challenge(ChallengeKind::Code),
            },
            done(),
        ]);
        let ui = FakeUi {
            code: Some(" 123456 "),
            payee_ok: false,
        };
        let msg = drive_transfer(&s, &ui).unwrap();
        assert!(msg.contains("GC123"));
        assert_eq!(*s.calls.borrow(), vec!["start", "code:123456"]);
    }

    #[test]
    fn declined_payee_check_cancels_without_sending() {
        let s = Fake::new(vec![TransferStep::NeedsPayeeConfirmation {
            check: check(),
        }]);
        let ui = FakeUi {
            code: None,
            payee_ok: false,
        };
        assert_eq!(drive_transfer(&s, &ui).unwrap_err(), CANCELLED_TRANSFER);
        assert_eq!(*s.calls.borrow(), vec!["start", "cancel"]);
        assert!(payee_text(&check()).contains("NICHT"));
        assert!(payee_text(&check()).contains("Hinweis der Bank"));
    }

    #[test]
    fn confirmed_payee_continues() {
        let s = Fake::new(vec![
            TransferStep::NeedsPayeeConfirmation { check: check() },
            done(),
        ]);
        let ui = FakeUi {
            code: None,
            payee_ok: true,
        };
        assert!(drive_transfer(&s, &ui).is_ok());
        assert_eq!(*s.calls.borrow(), vec!["start", "confirm"]);
    }

    #[test]
    fn cancelled_tan_cancels_and_unknown_outcome_is_never_retried() {
        let s = Fake::new(vec![TransferStep::NeedsChallenge {
            challenge: challenge(ChallengeKind::Code),
        }]);
        let ui = FakeUi {
            code: None,
            payee_ok: false,
        };
        assert!(drive_transfer(&s, &ui).unwrap_err().contains("abgebrochen"));
        assert_eq!(s.calls.borrow().last().unwrap(), "cancel");

        let s = Fake::new(vec![TransferStep::OutcomeUnknown {
            reason: "Verbindung verloren.".into(),
            end_to_end_id: "X".into(),
        }]);
        let e = drive_transfer(&s, &ui).unwrap_err();
        assert!(e.contains("NICHT erneut senden"));
        assert_eq!(*s.calls.borrow(), vec!["start"]);
    }
}
