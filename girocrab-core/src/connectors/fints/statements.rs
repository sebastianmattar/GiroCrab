//! Statements: bookings via camt.052 (HKCAZ), MT940 (HKKAZ) and card
//! transactions (HKKKU/DKKKU), and PDF statements (HKEKA/HKEKP) with their
//! receipts (HKQTG).

use super::syntax::{self, SegmentBuilder};
use super::{FinTsConnector, SepaAccount, UpdAccount, camt, fints_date, kku, mt940};
use crate::connectors::{ConnectContext, RemoteTransaction, today};
use crate::error::{CoreError, CoreResult};
use crate::i18n::trf;

/// Start date of a card statement request: `since`, but not further back than the
/// bank's storage period (`storage_days`, 0 = unknown). Dates are `YYYY-MM-DD`.
pub(super) fn card_since(since: &str, today: &str, storage_days: i64) -> String {
    let parse = |d: &str| jiff::civil::Date::strptime("%Y-%m-%d", d).ok();
    match (parse(since), parse(today)) {
        (Some(s), Some(t)) if storage_days > 0 => {
            // the bank's number: an absurd value must not overflow the date arithmetic
            // (a panic on a hostile reply); no bound cuts the range then
            let oldest = jiff::Span::new()
                .try_days(storage_days)
                .and_then(|d| t.checked_sub(d))
                .ok();
            match oldest {
                Some(oldest) => crate::dates::iso(s.max(oldest)),
                None => since.to_string(),
            }
        }
        _ => since.to_string(),
    }
}

/// Label, period and date of a HIEKP statement. DKB (v2): PDF, period
/// `von:bis`, creation date, year, number, IBAN, BIC, …, file name, receipt
/// code. Label = `number/year (period)` when both are there, else the period.
pub(super) fn hiekp_details(s: &syntax::Segment) -> (String, String, String) {
    let (period, date) = match (fints_date(s.get(2, 0)), fints_date(s.get(2, 1))) {
        // DKB puts the issue date in both fields: show it once
        (Some(from), Some(to)) if from == to => (to.clone(), to),
        (Some(from), Some(to)) => (format!("{from}–{to}"), to),
        _ => statement_period(s),
    };
    let (year, number) = (s.get(4, 0), s.get(5, 0).trim_start_matches('0'));
    let label = if year.len() == 4
        && year.bytes().all(|b| b.is_ascii_digit())
        && !number.is_empty()
        && number.len() <= 4
        && number.bytes().all(|b| b.is_ascii_digit())
    {
        format!("{number}/{year} ({period})")
    } else {
        period.clone()
    };
    (label, period, date)
}

/// Period and date from the FinTS dates (`YYYYMMDD`) anywhere in a segment,
/// earliest to latest; today when it carries none (fallback for other banks).
pub(super) fn statement_period(s: &syntax::Segment) -> (String, String) {
    let mut dates: Vec<String> = s
        .elements
        .iter()
        .flatten()
        .filter_map(|v| fints_date(v.text()))
        .collect();
    dates.sort();
    dates.dedup();
    match (dates.first(), dates.last()) {
        (Some(a), Some(b)) if a != b => (format!("{a}–{b}"), b.clone()),
        (Some(a), _) => (a.clone(), a.clone()),
        _ => {
            let today = today();
            (today.clone(), today)
        }
    }
}

/// File name the bank sends with a statement: the text field ending in `.pdf`,
/// reduced to its last path component without control characters.
pub(super) fn statement_file_name(s: &syntax::Segment) -> Option<String> {
    s.elements.iter().flatten().find_map(|v| {
        let syntax::Value::Text(t) = v else {
            return None;
        };
        let name: String = t
            .rsplit(['/', '\\'])
            .next()?
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        let name = name.trim();
        (name.len() > 4 && name.len() <= 200 && name.to_lowercase().ends_with(".pdf"))
            .then(|| name.to_string())
    })
}

/// Display title from a bank file name: without `.pdf`, `_` as spaces
/// (`Kontoauszug_10_2026_vom_05.10.2026.pdf` → `Kontoauszug 10 2026 vom 05.10.2026`).
pub(super) fn statement_title(file_name: &str) -> String {
    let stem = &file_name[..file_name.len().saturating_sub(4)];
    stem.replace('_', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Shape of a reply segment for the debug log: kind and length of each field,
/// never its content (`bin(48211) date8 num4 pdfname(31) text(12)`).
pub(super) fn segment_layout(s: &syntax::Segment) -> String {
    s.elements
        .iter()
        .skip(1)
        .map(|el| {
            el.iter()
                .map(|v| match v {
                    syntax::Value::Binary(b) => format!("bin({})", b.len()),
                    syntax::Value::Text(t) if t.is_empty() => "-".into(),
                    syntax::Value::Text(t) if fints_date(t).is_some() => "date8".into(),
                    syntax::Value::Text(t) if t.bytes().all(|b| b.is_ascii_digit()) => {
                        format!("num{}", t.len())
                    }
                    syntax::Value::Text(t) if t.to_lowercase().ends_with(".pdf") => {
                        format!("pdfname({})", t.chars().count())
                    }
                    syntax::Value::Text(t) => format!("text({})", t.chars().count()),
                })
                .collect::<Vec<_>>()
                .join(":")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Receipt code of a statement (HIEKP/HIEKA): the binary field that is not the
/// PDF (DKB: the last field, 36 bytes), as hex for the persisted state.
pub(super) fn receipt_code(s: &syntax::Segment) -> Option<String> {
    s.elements
        .iter()
        .flatten()
        .filter_map(|v| match v {
            syntax::Value::Binary(b) if !b.is_empty() && !b.starts_with(b"%PDF") => Some(b),
            _ => None,
        })
        .next_back()
        .map(|b| b.iter().map(|x| format!("{x:02x}")).collect())
}

pub(super) fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

impl FinTsConnector {
    pub(super) fn hkcaz(
        &self,
        acc: &SepaAccount,
        format: &str,
        from: &str,
        to: &str,
        touchdown: Option<&str>,
    ) -> Vec<u8> {
        let mut b = SegmentBuilder::new("HKCAZ", 3, self.version("HKCAZ"))
            .raw(&Self::kti(acc))
            .de(&[format])
            .raw("N")
            .raw(from)
            .raw(to)
            .raw("");
        if let Some(t) = touchdown {
            b = b.de(&[t]);
        }
        b.build()
    }

    pub(super) async fn statements_camt(
        &mut self,
        ctx: &ConnectContext,
        acc: &SepaAccount,
        label: &str,
    ) -> CoreResult<(Vec<RemoteTransaction>, Option<i64>)> {
        let format = self.st.camt_formats.first().cloned().unwrap_or_default();
        let from = ctx.since.replace('-', "");
        let to = today().replace('-', "");
        let mut touchdown: Option<String> = None;
        let mut txs = Vec::new();
        let mut closing = None;
        // Not yet booked entries ("vorgemerkt"); a page may repeat them.
        let mut pending: Vec<RemoteTransaction> = Vec::new();
        let mut add_pending = |p: Vec<RemoteTransaction>| {
            for t in p {
                if !pending.iter().any(|q| q.external_id == t.external_id) {
                    pending.push(t);
                }
            }
        };
        for _page in 0..50 {
            let seg = self.hkcaz(acc, &format, &from, &to, touchdown.as_deref());
            let r = self.request(ctx, "HKCAZ", seg).await?;
            for s in r.all("HICAZ") {
                // element 3: booked camt documents (one or more binaries)
                for v in s.element(3) {
                    if let syntax::Value::Binary(xml) = v {
                        // raw @len@ slice, decoded as the XML's own encoding (UTF-8) – never via the Latin-1 envelope
                        let (t, rep) = camt::parse(&crate::text::decode_xml_bytes(xml), label);
                        txs.extend(t);
                        add_pending(rep.pending);
                        if rep.closing_balance_cents.is_some() {
                            closing = rep.closing_balance_cents;
                        }
                    }
                }
                // element 4: not yet booked entries (one camt document)
                for v in s.element(4) {
                    if let syntax::Value::Binary(xml) = v {
                        add_pending(camt::parse_notbooked(
                            &crate::text::decode_xml_bytes(xml),
                            label,
                        ));
                    }
                }
            }
            touchdown = r.code("3040").and_then(|c| c.3.first().cloned());
            if touchdown.is_none() {
                break;
            }
        }
        txs.extend(pending);
        Ok((txs, closing))
    }

    /// The card-bookings transaction a card account can be fetched with: HKKKU, or
    /// its predecessor DKKKU (comdirect, #807), each only when the bank offers it
    /// (BPD) and the account lists it (HIUPD).
    pub(super) fn card_tx(&self, upd: Option<&UpdAccount>) -> Option<&'static str> {
        let allowed = |tx: &str| upd.is_some_and(|u| u.allowed.contains(tx));
        ["HKKKU", "DKKKU"]
            .into_iter()
            .find(|tx| self.bank_supports(tx) && allowed(tx))
    }

    /// HKKKU v1 / DKKKU v2 request: account, card number, sub-account, date range,
    /// max. entries, touchdown. HKKKU names the account by KTI, DKKKU (older) by KTV;
    /// `account = false` leaves it out (HIKKUS: not required).
    pub(super) fn card_request(
        tx: &str,
        version: u32,
        acc: &SepaAccount,
        account: bool,
        from: &str,
        to: &str,
        touchdown: Option<&str>,
    ) -> Vec<u8> {
        let account = match (account, tx) {
            (false, _) => String::new(),
            (true, "DKKKU") => Self::ktv(acc),
            (true, _) => Self::kti(acc),
        };
        let mut b = SegmentBuilder::new(tx, 3, version)
            .raw(&account)
            .de(&[&acc.account_number])
            .de(&[&acc.sub_account])
            .raw(from)
            .raw(to)
            .raw("");
        if let Some(t) = touchdown {
            b = b.de(&[t]);
        }
        b.build()
    }

    /// HKKKU / DKKKU (`tx`): bookings and balance of a credit card account, all pages.
    pub(super) async fn statements_card(
        &mut self,
        ctx: &ConnectContext,
        tx: &str,
        acc: &SepaAccount,
        label: &str,
        currency: &str,
    ) -> CoreResult<(Vec<RemoteTransaction>, Option<kku::CardBalance>)> {
        let v = self.version(tx);
        // HIKKU / DIKKU
        let reply_id = format!("{}I{}", &tx[..1], &tx[2..]);
        let (from, to) = if self.st.kku_no_date_range {
            (String::new(), String::new())
        } else {
            // Never ask for more than the bank keeps: an older start date is an error.
            let since = card_since(&ctx.since, &today(), self.st.kku_storage_days);
            (since.replace('-', ""), today().replace('-', ""))
        };
        let mut touchdown: Option<String> = None;
        let mut txs = Vec::new();
        let mut balance = None;
        for page in 0..50 {
            let seg = Self::card_request(
                tx,
                v,
                acc,
                !self.st.kku_no_account,
                &from,
                &to,
                touchdown.as_deref(),
            );
            let r = self.request(ctx, tx, seg).await?;
            let (t, bal) = kku::parse(r.all(&reply_id), label, currency);
            if crate::depot_debug::enabled() {
                let groups: Vec<usize> = r
                    .all(&reply_id)
                    .map(|s| s.elements.len().saturating_sub(6))
                    .collect();
                crate::depot_debug::log(&format!(
                    "fints {tx} v{v} page {page}: {reply_id} segments={} groups={groups:?} parsed={} balance={}",
                    groups.len(),
                    t.len(),
                    bal.is_some()
                ));
                // Layout check without content: kind of every field of the first
                // segment head and of the first booking group.
                if let Some(s) = r.all(&reply_id).next() {
                    crate::depot_debug::log(&format!("fints {reply_id} shape: {}", kku::shape(s)));
                }
            }
            txs.extend(t);
            if balance.is_none() {
                balance = bal;
            }
            touchdown = r.code("3040").and_then(|c| c.3.first().cloned());
            if touchdown.is_none() {
                break;
            }
        }
        Ok((txs, balance))
    }

    pub(super) async fn statements_mt940(
        &mut self,
        ctx: &ConnectContext,
        acc: &SepaAccount,
        label: &str,
        currency: &str,
    ) -> CoreResult<(Vec<RemoteTransaction>, Option<i64>)> {
        let v = self.version("HKKAZ");
        let acct = if v >= 7 {
            Self::kti(acc)
        } else {
            Self::ktv(acc)
        };
        let from = ctx.since.replace('-', "");
        let to = today().replace('-', "");
        let mut touchdown: Option<String> = None;
        let mut data = Vec::<u8>::new();
        for _page in 0..50 {
            let mut b = SegmentBuilder::new("HKKAZ", 3, v)
                .raw(&acct)
                .raw("N")
                .raw(&from)
                .raw(&to)
                .raw("");
            if let Some(t) = &touchdown {
                b = b.de(&[t]);
            }
            let r = self.request(ctx, "HKKAZ", b.build()).await?;
            if r.has("3997") {
                self.partial_statements = true;
            }
            for s in r.all("HIKAZ") {
                if let Some(b) = s.binary(1) {
                    data.extend_from_slice(b);
                    data.extend_from_slice(b"\r\n");
                }
            }
            touchdown = r.code("3040").and_then(|c| c.3.first().cloned());
            if touchdown.is_none() {
                break;
            }
        }
        let (txs, stmt) = mt940::parse(&crate::text::decode_bank_bytes(&data), label, currency);
        Ok((txs, stmt.closing_balance_cents))
    }

    /// The statement transaction the bank offers: HKEKA (statement in a chosen
    /// format) or HKEKP (statement as PDF, e.g. DKB, #449).
    pub(super) fn statement_tx(&self) -> Option<&'static str> {
        ["HKEKA", "HKEKP"]
            .into_iter()
            .find(|tx| self.bank_supports(tx))
    }

    /// Acknowledges the statements stored by earlier fetches (HKQTG). A receipt
    /// the bank rejects is dropped (it would fail again); one lost to a network
    /// error stays for the next fetch. Never fails the statements fetch.
    pub(super) async fn send_pending_receipts(&mut self, ctx: &ConnectContext) {
        if self.st.pending_receipts.is_empty() || !self.bank_supports("HKQTG") {
            return;
        }
        let v = self.version("HKQTG");
        let mut kept = Vec::new();
        let mut sent = 0;
        for code in std::mem::take(&mut self.st.pending_receipts) {
            let Some(bytes) = hex_decode(&code) else {
                continue;
            };
            let seg = SegmentBuilder::new("HKQTG", 3, v).binary(&bytes).build();
            match self.request(ctx, "HKQTG", seg).await {
                Ok(_) => sent += 1,
                Err(CoreError::Bank { code: c, .. }) => {
                    crate::depot_debug::log(&format!("statements: HKQTG rejected ({c}), dropped"));
                }
                Err(_) => kept.push(code),
            }
        }
        self.st.pending_receipts = kept;
        crate::depot_debug::log(&format!(
            "statements: HKQTG sent {sent} receipt(s), {} pending",
            self.st.pending_receipts.len()
        ));
    }

    /// PDF statements via HKEKA (format 3) or HKEKP, all pages. Acknowledges the
    /// statements of earlier fetches first (HKQTG).
    pub(super) async fn statements_pdf(
        &mut self,
        ctx: &ConnectContext,
        acc: &SepaAccount,
        label: &str,
    ) -> CoreResult<Vec<crate::documents::RemoteDocument>> {
        self.send_pending_receipts(ctx).await;
        let tx = self.statement_tx().unwrap_or("HKEKA");
        let v = self.version(tx);
        let pdf_only = tx == "HKEKP";
        // HKEKA from v5 and HKEKP from v2 address the account by IBAN (KTI).
        let acct = if (pdf_only && v >= 2) || (!pdf_only && v >= 5) {
            Self::kti(acc)
        } else {
            Self::ktv(acc)
        };
        let reply_id = format!("HI{}", &tx[2..]);
        let mut docs = Vec::new();
        let mut touchdown: Option<String> = None;
        for _page in 0..12 {
            // HKEKA: account, format 3 (PDF), number, year, max entries.
            // HKEKP: account, number, year, max entries (always PDF).
            let mut b = SegmentBuilder::new(tx, 3, v).raw(&acct);
            if !pdf_only {
                b = b.raw("3");
            }
            b = b.raw("").raw("").raw("");
            if let Some(t) = &touchdown {
                b = b.de(&[t]);
            }
            let r = self.request(ctx, tx, b.build()).await?;
            for s in r.all(&reply_id) {
                crate::depot_debug::log(&format!(
                    "statements: {reply_id} v{} layout: {}",
                    s.version(),
                    segment_layout(s)
                ));
                // the bank's own file name (e.g. DKB), else one of ours
                let bank_file_name = statement_file_name(s);
                if let Some(code) = receipt_code(s)
                    && !self.st.pending_receipts.contains(&code)
                {
                    self.st.pending_receipts.push(code);
                }
                let (stmt_label, period, date) = if pdf_only {
                    hiekp_details(s)
                } else {
                    let to = s.get(2, 1);
                    let period = format!("{}–{}", s.get(2, 0), to);
                    (period.clone(), period, fints_date(to).unwrap_or_else(today))
                };
                for el in &s.elements {
                    for val in el {
                        if let syntax::Value::Binary(pdf) = val
                            && pdf.starts_with(b"%PDF")
                        {
                            // HKEKP periods are not reliable enough for an
                            // id; the bank re-sends unacknowledged statements,
                            // so the content keeps them from being stored twice.
                            let source_url = if pdf_only {
                                format!("fints:hkekp:{label}:{:016x}", crate::fnv::fnv1a64(pdf))
                            } else {
                                format!("fints:hkeka:{label}:{period}")
                            };
                            docs.push(crate::documents::RemoteDocument {
                                account_ref: label.to_string(),
                                booking_external_id: None,
                                // like MoneyMoney: the bank's file name, readable
                                title: bank_file_name
                                    .as_deref()
                                    .map(statement_title)
                                    .unwrap_or_else(|| trf("Kontoauszug {0}", &[&stmt_label])),
                                doc_type: "kontoauszug".into(),
                                mime_type: "application/pdf".into(),
                                file_name: bank_file_name.clone().unwrap_or_else(|| {
                                    format!("Kontoauszug_{}_{}.pdf", label, date)
                                }),
                                source_url,
                                bytes: pdf.clone(),
                                date: date.clone(),
                            });
                        }
                    }
                }
            }
            touchdown = r.code("3040").and_then(|c| c.3.first().cloned());
            if touchdown.is_none() {
                break;
            }
        }
        crate::depot_debug::log(&format!(
            "statements: {tx} v{v} delivered {} PDF(s)",
            docs.len()
        ));
        Ok(docs)
    }

    /// Non-sensitive trace whether the bank offers PDF statements (HIEKAS), e.g.
    /// to check banks like ING (GitHub #57). No account data is logged.
    pub(super) fn log_statement_support(&self, upd_allows: Option<bool>) {
        crate::depot_debug::log(&format!(
            "statements: blz={} HKEKA={:?} HKEKP={:?} using={:?} upd_allows={:?} tan_required={:?}",
            self.blz,
            self.st.versions.get("HKEKA"),
            self.st.versions.get("HKEKP"),
            self.statement_tx(),
            upd_allows,
            self.statement_tx()
                .and_then(|tx| self.st.tan_required.get(tx)),
        ));
    }
}
