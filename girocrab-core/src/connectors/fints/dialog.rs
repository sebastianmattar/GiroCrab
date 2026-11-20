//! Messages and dialogs: builds and sends the signed PIN/TAN (or RAH-10)
//! messages, opens, synchronises and ends dialogs, and handles TAN challenges
//! and decoupled approvals.

use std::collections::HashMap;

use base64::Engine;

use super::syntax::{SegmentBuilder, escape, parse_response};
use super::{BankReply, CONSORSBANK_BLZ, COUNTRY, FinTsConnector, rah10};
use crate::connectors::fints_directory::COMDIRECT_BLZ;
use crate::connectors::{
    Challenge, ChallengeKind, ConnectContext, Phase, ensure_secure_fints_url,
    http_client_no_redirect, read_text_limited,
};
use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};

/// Reply to a decoupled status poll (HKTAN process S): the approval in the app
/// is still outstanding. Standard is 3956; Deutsche Bank answers with warnings
/// only (3060, no success code) until the user has confirmed.
pub(super) fn approval_pending(reply: &BankReply) -> bool {
    // 3045 ("SEPA-Instant Payment Statusabfrage HKIPS veranlassen"): bank accepted the
    // instant payment for processing after approval; polling HKTAN again fails with 9210 (#1121).
    reply.errors().is_empty()
        && !reply.has("3045")
        && (reply.has("3956") || !reply.codes.iter().any(|c| c.0.starts_with('0')))
}

/// `request` failed because the bank wanted a TAN (0030) or an app approval
/// that was not given in time (3956): asking again sends the next one (#1083).
pub(super) fn tan_demanded(e: &CoreError) -> bool {
    matches!(e, CoreError::Bank { code, .. } if matches!(code.as_str(), "0030" | "3956"))
}

/// Whether a TAN medium name already carries the "SO:" (security object) prefix.
pub(super) fn has_so_prefix(medium: &str) -> bool {
    medium
        .get(..3)
        .is_some_and(|p| p.eq_ignore_ascii_case("SO:"))
}

/// Dialog init refused with 9010 ("Ungültiger Signaturaufbau") that is not a stale
/// system ID / TAN method (9931/9942).
fn init_rejected_as_malformed(reply: &BankReply) -> bool {
    reply.has("9010") && !reply.has("9931") && !reply.has("9942")
}

/// The bank code does not belong to the login: 9210 "Bankleitzahl passt nicht
/// zum Benutzer" (comdirect). 9210 alone is any rejected order, so the text decides.
pub(super) fn blz_mismatch(reply: &BankReply) -> bool {
    reply.codes.iter().any(|c| {
        let text = c.2.to_lowercase();
        c.0 == "9210" && text.contains("bankleitzahl") && text.contains("benutzer")
    })
}

/// One of comdirect's bank codes.
pub(super) fn is_comdirect(blz: &str) -> bool {
    COMDIRECT_BLZ.contains(&blz)
}

fn clean_challenge(s: &str) -> String {
    s.replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<b>", "")
        .replace("</b>", "")
        .trim()
        .to_string()
}

/// photoTAN images come as 2-byte length + MIME type + 2-byte length + data.
fn strip_mime_header(b: Vec<u8>) -> Vec<u8> {
    if b.len() > 4 {
        let l = ((b[0] as usize) << 8) | b[1] as usize;
        if l > 0 && l < 64 && b.len() > l + 4 && b[2..2 + l].starts_with(b"image/") {
            return b[l + 4..].to_vec();
        }
    }
    b
}

impl FinTsConnector {
    pub(super) fn now() -> (String, String) {
        let n = jiff::ZonedDateTime::now();
        (
            n.strftime("%Y%m%d").to_string(),
            n.strftime("%H%M%S").to_string(),
        )
    }

    fn system_id(&self) -> &str {
        if self.st.system_id.is_empty() {
            "0"
        } else {
            &self.st.system_id
        }
    }

    /// Security profile of HNSHK/HNVSK: version 1 for the one-step method
    /// (security function 999, synchronisation dialog), version 2 for every
    /// two-step TAN method. Strict banks (Consorsbank) reject a mismatch with
    /// "9010 Ungültiger Signaturaufbau".
    fn security_profile(sec_func: &str) -> &'static str {
        if sec_func == "999" { "PIN:1" } else { "PIN:2" }
    }

    /// Wraps payload segments (numbered from 3) into a signed, "encrypted"
    /// PIN/TAN message and returns the wire bytes.
    pub(super) fn build_message(
        &self,
        ctx: &ConnectContext,
        payload: &[Vec<u8>],
        sec_func: &str,
        tan: Option<&str>,
    ) -> Vec<u8> {
        let (date, time) = Self::now();
        let user = escape(&ctx.login_name);
        let ctrl_ref = format!("{}", rand::random::<u32>() % 9_000_000 + 1_000_000);
        let profile = Self::security_profile(sec_func);
        let mut inner: Vec<u8> = Vec::new();
        inner.extend(
            SegmentBuilder::new("HNSHK", 2, 4)
                .raw(profile)
                .raw(sec_func)
                .raw(&ctrl_ref)
                .raw("1")
                .raw("1")
                .raw(&format!("1::{}", escape(self.system_id())))
                .raw("1")
                .raw(&format!("1:{date}:{time}"))
                .raw("1:999:1")
                .raw("6:10:16")
                .raw(&format!("{COUNTRY}:{}:{user}:S:0:0", self.blz))
                .build(),
        );
        for p in payload {
            inner.extend_from_slice(p);
        }
        let n_sha = 3 + payload.len() as u32;
        let pin = escape(ctx.secret.expose());
        let auth = match tan {
            Some(t) => format!("{pin}:{}", escape(t)),
            None => pin,
        };
        inner.extend(
            SegmentBuilder::new("HNSHA", n_sha, 2)
                .raw(&ctrl_ref)
                .raw("")
                .raw(&auth)
                .build(),
        );

        let hnvsk = SegmentBuilder::new("HNVSK", 998, 3)
            .raw(profile)
            .raw("998")
            .raw("1")
            .raw(&format!("1::{}", escape(self.system_id())))
            .raw(&format!("1:{date}:{time}"))
            .raw("2:2:13:@8@00000000:5:1")
            .raw(&format!("{COUNTRY}:{}:{user}:V:0:0", self.blz))
            .raw("0")
            .build();
        let hnvsd = SegmentBuilder::new("HNVSD", 999, 1).binary(&inner).build();
        let hnhbs = SegmentBuilder::new("HNHBS", n_sha + 1, 1)
            .raw(&self.msg_no.to_string())
            .build();

        let build = |size: usize| {
            let mut m = SegmentBuilder::new("HNHBK", 1, 3)
                .raw(&format!("{size:012}"))
                .raw("300")
                .raw(&escape(&self.dialog_id))
                .raw(&self.msg_no.to_string())
                .build();
            m.extend_from_slice(&hnvsk);
            m.extend_from_slice(&hnvsd);
            m.extend_from_slice(&hnhbs);
            m
        };
        let len = build(0).len();
        build(len)
    }

    pub(super) async fn send(
        &mut self,
        ctx: &ConnectContext,
        payload: Vec<Vec<u8>>,
        sec_func: &str,
        tan: Option<&str>,
    ) -> CoreResult<BankReply> {
        let mut sent = HashMap::new();
        sent.insert(2, "HNSHK".to_string());
        for p in &payload {
            let head = crate::text::decode_bank_bytes(&p[..p.len().min(16)]);
            let mut it = head.split(':');
            if let (Some(id), Some(n)) = (it.next(), it.next().and_then(|n| n.parse::<u32>().ok()))
            {
                sent.insert(n, id.to_string());
            }
        }
        sent.insert(3 + payload.len() as u32, "HNSHA".to_string());
        let raw = if let Some(rah) = &mut self.rah {
            let message = rah.build(
                &payload,
                if self.st.system_id.is_empty() {
                    "0"
                } else {
                    &self.st.system_id
                },
                &self.dialog_id,
                self.msg_no,
            )?;
            let raw = rah10::exchange(&mut self.rah_stream, &message).await?;
            rah.unwrap(&raw, self.msg_no, &self.dialog_id)?
        } else {
            let msg = self.build_message(ctx, &payload, sec_func, tan);
            ensure_secure_fints_url(&self.url)?;
            let client = match &self.client {
                Some(c) => c.clone(),
                None => {
                    let c = http_client_no_redirect()?;
                    self.client = Some(c.clone());
                    c
                }
            };
            let body = base64::engine::general_purpose::STANDARD.encode(&msg);
            let resp = client
                .post(&self.url)
                .header("Content-Type", "text/plain")
                .body(body)
                .send()
                .await?;
            if !resp.status().is_success() {
                return Err(CoreError::Network {
                    message: format!("FinTS server answered HTTP {}", resp.status()),
                });
            }
            let text = read_text_limited(resp).await?;
            let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            let raw = base64::engine::general_purpose::STANDARD
                .decode(cleaned.as_bytes())
                .map_err(|e| CoreError::protocol(format!("base64: {e}")))?;
            parse_response(&raw)?
        };
        let segments = raw;
        self.msg_no += 1;
        let mut codes = Vec::new();
        for s in &segments {
            if s.id() == "HIRMG" || s.id() == "HIRMS" {
                for el in s.elements.iter().skip(1) {
                    let t = |i: usize| el.get(i).map(|v| v.text().to_string()).unwrap_or_default();
                    let params = el.iter().skip(3).map(|v| v.text().to_string()).collect();
                    codes.push((t(0), s.reference(), t(2), params));
                }
            }
        }
        if crate::depot_debug::enabled() {
            // Step trace: segment names and return codes only, no texts or data.
            crate::depot_debug::log(&format!(
                "fints msg: sec={} profile={} sent={:?} codes={:?}",
                if sec_func == "999" {
                    "one-step"
                } else {
                    "two-step"
                },
                Self::security_profile(sec_func),
                payload
                    .iter()
                    .map(|p| {
                        // "HKKAZ:3:7" – segment header only
                        let head = crate::text::decode_bank_bytes(&p[..p.len().min(16)]);
                        head.split(['+', '\''])
                            .next()
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect::<Vec<_>>(),
                codes
                    .iter()
                    .map(|c: &(String, Option<u32>, String, Vec<String>)| match c.1 {
                        Some(n) => format!("{}@{n}", c.0),
                        None => c.0.clone(),
                    })
                    .collect::<Vec<_>>()
            ));
            // Bank's wording for errors and unusual warnings (generic texts, no
            // parameters). Routine codes (no SCA, BPD/UPD hints, TAN list) are skipped.
            for c in codes.iter().filter(|c| {
                c.0.starts_with('9')
                    || (c.0.starts_with('3')
                        && (!matches!(c.0.as_str(), "3050" | "3060" | "3076" | "3920")
                            // 3060 on a segment: e.g. "approval still pending" at some banks
                            || (c.0 == "3060" && c.1.is_some())))
            }) {
                crate::depot_debug::log(&format!(
                    "fints {} {}: {} (params: {})",
                    if c.0.starts_with('9') {
                        "error"
                    } else {
                        "warning"
                    },
                    c.0,
                    c.2.chars().take(160).collect::<String>(),
                    c.3.len()
                ));
            }
        }
        if let Some(h) = segments.iter().find(|s| s.id() == "HNHBK") {
            let did = h.get(3, 0);
            if !did.is_empty() && did != "0" {
                self.dialog_id = did.to_string();
            }
        }
        Ok(BankReply {
            segments,
            codes,
            sent,
        })
    }

    fn reset_dialog(&mut self) {
        self.rah_stream = None;
        self.dialog_id = "0".into();
        self.msg_no = 1;
    }

    pub(super) fn hkidn(&self, ctx: &ConnectContext, n: u32) -> Vec<u8> {
        let customer = self
            .rah
            .as_ref()
            .map(|r| r.identity.customer_id.as_str())
            .filter(|c| !c.is_empty())
            .unwrap_or(&ctx.login_name);
        SegmentBuilder::new("HKIDN", n, 2)
            .raw(&format!("{COUNTRY}:{}", self.blz))
            .de(&[customer])
            .de(&[self.system_id()])
            .raw("1")
            .build()
    }

    pub(super) fn hkvvb(&self, n: u32) -> Vec<u8> {
        SegmentBuilder::new("HKVVB", n, 3)
            .raw("0")
            .raw("0")
            .raw("0")
            .de(&[&self.product_id])
            .de(&[&self.product_version])
            .build()
    }

    pub(super) fn hktan_process4(&self, n: u32, reference_segment: &str) -> Vec<u8> {
        let mut b = SegmentBuilder::new("HKTAN", n, self.version("HKTAN"))
            .raw("4")
            .raw(reference_segment);
        if !self.st.tan_medium.is_empty() {
            for _ in 0..8 {
                b = b.raw("");
            }
            b = b.de(&[&self.st.tan_medium]);
        }
        b.build()
    }

    /// Analyses a reply that may carry a TAN challenge.
    pub(super) fn challenge_from(&mut self, reply: &BankReply) -> CoreResult<Option<Phase>> {
        let hitan = reply.first("HITAN");
        if let Some(h) = hitan {
            let task = h.get(3, 0);
            if !task.is_empty() && task != "noref" {
                self.task_reference = Some(task.to_string());
            }
        }
        let challenge_text = hitan.map(|h| h.get(4, 0).to_string()).unwrap_or_default();
        let method = if self.st.tan_method_name.is_empty() {
            trf("TAN-Verfahren {0}", &[&self.st.sec_func])
        } else {
            self.st.tan_method_name.clone()
        };
        if reply.has("3955") || reply.has("3956") {
            let c = Challenge {
                kind: ChallengeKind::DecoupledPush,
                title: tr("Bitte in der Banking-App freigeben").into(),
                message: if challenge_text.is_empty() {
                    reply
                        .code("3955")
                        .or(reply.code("3956"))
                        .map(|c| c.2.clone())
                        .unwrap_or_default()
                } else {
                    clean_challenge(&challenge_text)
                },
                method_name: method,
                redirect_url: None,
                image: None,
                expires_in_seconds: Some(300),
                poll_interval_seconds: 3,
            };
            self.challenge = Some(c.clone());
            return Ok(Some(if reply.has("3956") {
                Phase::Pending(c)
            } else {
                Phase::Challenge(c)
            }));
        }
        if reply.has("0030") && self.task_reference.is_some() {
            let image = hitan
                .and_then(|h| h.binary(5))
                .map(|b| b.to_vec())
                .filter(|b| b.len() > 64);
            let c = Challenge {
                kind: if image.is_some() {
                    ChallengeKind::ImageCode
                } else {
                    ChallengeKind::Code
                },
                title: tr("TAN eingeben").into(),
                message: clean_challenge(&challenge_text),
                method_name: method,
                redirect_url: None,
                image: image.map(strip_mime_header),
                expires_in_seconds: Some(300),
                poll_interval_seconds: 0,
            };
            self.challenge = Some(c.clone());
            return Ok(Some(Phase::Challenge(c)));
        }
        Ok(None)
    }

    /// Synchronisation (if needed) and the authenticated dialog. comdirect
    /// has several bank codes, and the one in an account's IBAN is not always
    /// the one the login is known under: when the bank answers 9210 "Bankleitzahl
    /// passt nicht zum Benutzer", its other codes are tried in turn (same
    /// address, same PIN) and the one that works is kept in the state. Any
    /// other answer ends the search; all codes failing reports the first error.
    pub(super) async fn login(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        let first = self.login_attempt(ctx).await;
        if first.is_ok() || !self.blz_rejected || self.rah.is_some() || !is_comdirect(&self.blz) {
            return first;
        }
        let tried = self.blz.clone();
        for blz in COMDIRECT_BLZ.iter().filter(|b| **b != tried) {
            self.blz = (*blz).to_string();
            crate::depot_debug::log(&format!(
                "fints: comdirect bank code {tried} rejected, trying {blz}"
            ));
            let attempt = self.login_attempt(ctx).await;
            if attempt.is_ok() {
                self.st.login_blz = self.blz.clone();
                return attempt;
            }
            if !self.blz_rejected {
                return attempt;
            }
        }
        self.blz = tried;
        first
    }

    async fn login_attempt(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        self.blz_rejected = false;
        if self.st.system_id.is_empty() || self.st.sec_func.is_empty() {
            self.sync_dialog(ctx).await?;
        }
        self.open_dialog(ctx).await
    }

    pub(super) async fn sync_dialog(&mut self, ctx: &ConnectContext) -> CoreResult<()> {
        self.reset_dialog();
        let payload = vec![
            self.hkidn(ctx, 3),
            self.hkvvb(4),
            SegmentBuilder::new("HKSYN", 5, 3).raw("0").build(),
        ];
        let reply = self.send(ctx, payload, "999", None).await?;
        self.blz_rejected = blz_mismatch(&reply);
        reply.check()?;
        self.learn_bpd(&reply);
        // Optional: TAN media list (needed by some banks for HKTAN).
        let tab_v = self.version("HKTAB");
        if self.bank_supports("HKTAB")
            && let Ok(r) = self
                .send(
                    ctx,
                    vec![
                        SegmentBuilder::new("HKTAB", 3, tab_v)
                            .raw("0")
                            .raw("A")
                            .build(),
                    ],
                    "999",
                    None,
                )
                .await
        {
            self.learn_bpd(&r);
        }
        self.end_dialog(ctx, "999").await;
        if self.st.system_id.is_empty() {
            return Err(CoreError::protocol(
                "bank did not assign a customer system ID (HISYN)",
            ));
        }
        Ok(())
    }

    pub(super) async fn end_dialog(&mut self, ctx: &ConnectContext, sec_func: &str) {
        if self.dialog_id != "0" {
            let seg = SegmentBuilder::new("HKEND", 3, 1)
                .de(&[&self.dialog_id.clone()])
                .build();
            let _ = self.send(ctx, vec![seg], sec_func, None).await;
        }
        self.reset_dialog();
    }

    /// Whether a business segment must be accompanied by HKTAN process 4:
    /// what HIPINS says, plus banks whose HIPINS is wrong. Consorsbank lists
    /// HKKAZ/HKSAL as not TAN-bound but rejects them without HKTAN (9075).
    pub(super) fn needs_hktan(&self, seg_id: &str) -> bool {
        self.st.tan_required.get(seg_id) == Some(&true)
            || (self.blz == CONSORSBANK_BLZ && matches!(seg_id, "HKKAZ" | "HKSAL"))
    }

    /// Sends one business segment; adds HKTAN process 4 (referencing that
    /// segment) when the bank requires SCA for it (HIPINS), and waits for a
    /// decoupled approval (HKTAN process S polling) if the bank asks for one.
    pub(super) async fn request(
        &mut self,
        ctx: &ConnectContext,
        seg_id: &str,
        segment: Vec<u8>,
    ) -> CoreResult<BankReply> {
        let sec = self.sec();
        let mut payload = vec![segment];
        if sec != "999" && self.needs_hktan(seg_id) {
            payload.push(self.hktan_process4(4, seg_id));
        }
        let mut reply = self.send(ctx, payload, &sec, None).await?;
        if reply.has("3955") || reply.has("3956") {
            let _ = self.challenge_from(&reply)?;
            let task = self.task_reference.clone().ok_or_else(|| {
                CoreError::protocol(trf("{0}: Freigabe ohne Auftragsreferenz", &[&seg_id]))
            })?;
            let mut approved = false;
            for _ in 0..60 {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                let seg = SegmentBuilder::new("HKTAN", 3, self.version("HKTAN"))
                    .raw("S")
                    .raw("")
                    .raw("")
                    .raw("")
                    .de(&[&task])
                    .raw("N")
                    .build();
                let r = self.send(ctx, vec![seg], &sec, None).await?;
                if !approval_pending(&r) {
                    reply = r;
                    approved = true;
                    break;
                }
            }
            if !approved {
                return Err(CoreError::Bank {
                    code: "3956".into(),
                    message: trf(
                        "{0}: Die Freigabe in der Banking-App ist nicht rechtzeitig erfolgt. Bitte erneut abrufen und in der App bestätigen.",
                        &[&seg_id],
                    ),
                });
            }
        } else if reply.has("0030") {
            return Err(CoreError::Bank {
                code: "0030".into(),
                message: trf(
                    "Die Bank verlangt für {0} eine TAN-Eingabe. Bitte ein Push-/App-Verfahren (z. B. SecureGo plus) verwenden oder „Tage beim ersten Abruf“ auf höchstens 90 setzen.",
                    &[&seg_id],
                ),
            });
        }
        reply.check()?;
        Ok(reply)
    }

    /// Authenticated dialog (HKIDN/HKVVB + HKTAN process 4 for SCA).
    pub(super) async fn open_dialog(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        // Authenticated dialog with strong customer authentication.
        self.reset_dialog();
        let sec = self.sec();
        let mut reply = self.send_dialog_init(ctx, &sec).await?;
        // wrong bank code for this login: nothing below can help (`login` may
        // try another one)
        if blz_mismatch(&reply) {
            self.blz_rejected = true;
            reply.check()?;
        }
        // Some banks reject HKTAN in the dialog init as malformed (9010 without a
        // PIN/TAN/system-id code): retry once one-step and remember it.
        if sec != "999" && init_rejected_as_malformed(&reply) {
            self.reset_dialog();
            let retry = self.send_dialog_init(ctx, "999").await?;
            if retry.check().is_ok() {
                self.st.single_step = true;
                reply = retry;
            }
        }
        self.learn_bpd(&reply);
        // The bank wants the TAN device name in every HKTAN and offered no way to
        // look it up (Deutsche Bank BestSign rejects HKTAB without it): say what to do.
        // The name was sent but not accepted: some banks (BestSign) list the device as
        // "SO:<name>". Try that once so the user only has to enter the device name.
        if reply.has("9210")
            && !self.st.tan_medium.is_empty()
            && !has_so_prefix(&self.st.tan_medium)
        {
            let plain = std::mem::take(&mut self.st.tan_medium);
            self.st.tan_medium = format!("SO:{plain}");
            self.end_dialog(ctx, &sec).await;
            let retry = self.send_dialog_init(ctx, &sec).await?;
            self.learn_bpd(&retry);
            if retry.has("9210") {
                self.st.tan_medium = plain;
            } else {
                self.st.tan_medium_so = true;
            }
            reply = retry;
        }
        if reply.has("9210") && self.st.tan_medium.is_empty() {
            return Err(CoreError::Bank {
                code: "9210".into(),
                message: tr("Die Bank verlangt den Namen Ihres TAN-Mediums (z. B. den Gerätenamen aus der BestSign-Verwaltung). Tragen Sie ihn im Bankzugang unter „TAN-Medium“ ein und rufen Sie erneut ab.").into(),
            });
        }
        if let Err(e) = reply.check() {
            // Stale system ID / TAN method -> force re-sync next time.
            if reply.has("9931") || reply.has("9942") || reply.has("9010") {
                self.st.system_id.clear();
            }
            return Err(e);
        }
        if let Some(p) = self.challenge_from(&reply)? {
            return Ok(p);
        }
        Ok(Phase::Ready)
    }

    async fn send_dialog_init(&mut self, ctx: &ConnectContext, sec: &str) -> CoreResult<BankReply> {
        let mut payload = vec![self.hkidn(ctx, 3), self.hkvvb(4)];
        if sec != "999" {
            payload.push(self.hktan_process4(5, "HKIDN"));
        }
        self.send(ctx, payload, sec, None).await
    }

    pub(super) fn sec(&self) -> String {
        if self.rah.is_some() || self.st.single_step || self.st.sec_func.is_empty() {
            "999".into()
        } else {
            self.st.sec_func.clone()
        }
    }

    pub(super) fn check_reply(&self, reply: &BankReply) -> CoreResult<()> {
        if self.transferring {
            reply.check_transfer()
        } else {
            reply.check()
        }
    }
}
