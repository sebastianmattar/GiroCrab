//! Invoice and credit-note PDFs of Amazon orders (#353).
//!
//! The order history import ([`crate::importers::amazon_orders`]) creates one booking per order
//! (`amazon:<code>`) and one per refund (`amazon-refund:<code>:<n>:<cents>`). This module covers
//! the document side, without any I/O:
//!
//! * [`extract_links`] finds the "Rechnung" / "Gutschrift" links and the order summary link
//!   (`/gp/css/summary/print.html?orderID=…`) in an order page,
//! * [`to_remote_documents`] maps downloaded PDFs to the matching booking (invoice and summary →
//!   the order, the n-th credit note → the n-th refund). The `source_url` is derived from the
//!   document link, so storing the same document again is a no-op (`booking_documents` has a
//!   unique index on `source_url`, see `insert_doc`).
//!
//! Only `https` links on Amazon hosts are accepted, and only bytes starting with `%PDF-` become
//! documents (login pages and error pages are dropped).

use std::sync::LazyLock;

use regex::Regex;
use url::Url;

use super::html;
use crate::connectors::broker_documents::{is_pdf, source_url};
use crate::documents::RemoteDocument;
use crate::importers::amazon_orders::Booking;

/// What a document link is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    /// "Rechnung" / "Invoice": belongs to the order booking.
    Invoice,
    /// "Gutschrift" / "Credit note": belongs to a refund booking.
    CreditNote,
    /// Order summary (`/gp/css/summary/print.html?orderID=…`): belongs to the order booking.
    Summary,
}

impl DocKind {
    fn doc_type(self) -> &'static str {
        match self {
            DocKind::Invoice => "rechnung",
            DocKind::CreditNote => "gutschrift",
            DocKind::Summary => "beleg",
        }
    }

    fn title(self) -> &'static str {
        match self {
            DocKind::Invoice => "Rechnung",
            DocKind::CreditNote => "Gutschrift",
            DocKind::Summary => "Bestellübersicht",
        }
    }

    fn slug(self) -> &'static str {
        match self {
            DocKind::Invoice => "invoice",
            DocKind::CreditNote => "creditnote",
            DocKind::Summary => "summary",
        }
    }
}

/// A document link found in an order page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocLink {
    pub kind: DocKind,
    /// Absolute https URL on an Amazon host.
    pub url: String,
    /// Order code (`123-1234567-1234567`); empty if neither the link nor the caller knew it.
    pub order_code: String,
}

/// A downloaded document.
#[derive(Debug, Clone)]
pub struct Downloaded {
    pub link: DocLink,
    pub bytes: Vec<u8>,
}

fn is_amazon_host(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    h.split('.').count() >= 2
        && (h.starts_with("amazon.") || h.contains(".amazon."))
        && h.rsplit('.').next().is_some_and(|tld| tld.len() >= 2)
}

fn classify(href: &Url, text: &str) -> Option<DocKind> {
    let t = text.to_lowercase();
    if t.contains("gutschrift") || t.contains("credit note") {
        Some(DocKind::CreditNote)
    } else if t.contains("rechnung") || t.contains("invoice") {
        Some(DocKind::Invoice)
    } else if href.path().contains("/gp/css/summary/print.html") {
        Some(DocKind::Summary)
    } else {
        None
    }
}

fn order_code_in(s: &str) -> Option<String> {
    static CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b\d{3}-\d{7}-\d{7}\b").unwrap());
    CODE.find(s).map(|m| m.as_str().to_string())
}

/// Document links of one order page. `base_url` resolves relative links, `fallback_order`
/// is used for links that do not carry the order code themselves (`orderID=…` wins).
/// Links to other hosts, non-https links and duplicates are dropped; order of appearance is kept.
pub fn extract_links(html: &str, base_url: &str, fallback_order: &str) -> Vec<DocLink> {
    static ANCHOR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?is)<a\b[^>]*?\bhref\s*=\s*["']([^"']+)["'][^>]*>(.*?)</a>"#).unwrap()
    });
    let Ok(base) = Url::parse(base_url) else {
        return Vec::new();
    };
    let mut out: Vec<DocLink> = Vec::new();
    for cap in ANCHOR.captures_iter(html) {
        let href = cap[1].replace("&amp;", "&");
        let Ok(mut url) = base.join(href.trim()) else {
            continue;
        };
        if url.scheme() != "https" || !url.host_str().is_some_and(is_amazon_host) {
            continue;
        }
        url.set_fragment(None);
        let Some(kind) = classify(&url, &html::text(&cap[2])) else {
            continue;
        };
        let order_code = url
            .query_pairs()
            .find(|(k, _)| k.eq_ignore_ascii_case("orderid"))
            .map(|(_, v)| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .or_else(|| order_code_in(url.as_str()))
            .unwrap_or_else(|| fallback_order.trim().to_string());
        let link = DocLink {
            kind,
            url: url.to_string(),
            order_code,
        };
        if !out.contains(&link) {
            out.push(link);
        }
    }
    out
}

/// Stable id of a document inside its order: the same link always gives the same id.
fn document_id(link: &DocLink) -> String {
    let code = &link.order_code;
    let kind = link.kind.slug();
    if link.kind == DocKind::Summary {
        return format!("{code}-{kind}");
    }
    let path = Url::parse(&link.url)
        .map(|u| u.path().trim_matches('/').replace('/', "-"))
        .unwrap_or_default();
    format!("{code}-{kind}-{path}")
}

/// Maps downloaded PDFs to the bookings of `bookings` (as returned by `amazon_orders::bookings`).
///
/// * invoice / summary → `amazon:<code>`,
/// * the n-th credit note of an order → the n-th `amazon-refund:<code>:…` booking; a credit note
///   without a refund booking stays unlinked (account document), so nothing is attached wrongly.
///
/// Anything that is not a PDF, has no order code, or whose order booking is not in `bookings`
/// (cancelled / unbilled order) is skipped. `account_ref` is the connection's
/// `amazon-orders-<id>`. The result is deterministic: running it again yields the same
/// `source_url`s, which the vault ignores.
pub fn to_remote_documents(
    account_ref: &str,
    bookings: &[Booking],
    downloads: Vec<Downloaded>,
) -> Vec<RemoteDocument> {
    let mut out = Vec::new();
    let mut credit_seen: std::collections::HashMap<String, usize> = Default::default();
    for d in downloads {
        let code = d.link.order_code.trim();
        if code.is_empty() || !is_pdf(&d.bytes) {
            continue;
        }
        let order_id = format!("amazon:{code}");
        let booking_external_id = match d.link.kind {
            DocKind::Invoice | DocKind::Summary => {
                if !bookings.iter().any(|b| b.id == order_id) {
                    continue;
                }
                Some(order_id)
            }
            DocKind::CreditNote => {
                let n = credit_seen.entry(code.to_string()).or_default();
                let prefix = format!("amazon-refund:{code}:");
                let id = bookings
                    .iter()
                    .filter(|b| b.id.starts_with(&prefix))
                    .nth(*n)
                    .map(|b| b.id.clone());
                *n += 1;
                id
            }
        };
        let date = booking_external_id
            .as_ref()
            .and_then(|id| bookings.iter().find(|b| &b.id == id))
            .map(|b| b.date.clone())
            .unwrap_or_default();
        let id = document_id(&d.link);
        out.push(RemoteDocument {
            account_ref: account_ref.to_string(),
            booking_external_id,
            title: format!("{} {code}", d.link.kind.title()),
            doc_type: d.link.kind.doc_type().into(),
            mime_type: "application/pdf".into(),
            file_name: format!("amazon-{}-{code}.pdf", d.link.kind.slug()),
            bytes: d.bytes,
            source_url: source_url("amazon", account_ref, &id),
            date,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str =
        "https://www.amazon.de/gp/your-account/order-details?orderID=303-1234567-7654321";
    const CODE: &str = "303-1234567-7654321";
    const PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<<>>\nendobj\n%%EOF";

    fn booking(id: &str, date: &str) -> Booking {
        Booking {
            id: id.into(),
            date: date.into(),
            cents: -1000,
            purpose: String::new(),
            text: "Amazon-Bestellung",
            item_refs: String::new(),
        }
    }

    fn page() -> String {
        format!(
            r#"<html><body>
            <a href="/gp/css/summary/print.html?orderID={CODE}&amp;ref=ppx">Bestellübersicht drucken</a>
            <a class="x" href="/documents/download/aaa-111/invoice.pdf"><span>Rechnung</span></a>
            <a href="/documents/download/bbb-222/creditnote.pdf">Gutschrift 1</a>
            <a href="/documents/download/ccc-333/creditnote.pdf">Gutschrift 2</a>
            <a href="https://evil.example.com/amazon.de/invoice.pdf">Rechnung</a>
            <a href="http://www.amazon.de/documents/download/ddd/invoice.pdf">Rechnung</a>
            <a href="/gp/help">Hilfe</a>
            <a href="/documents/download/aaa-111/invoice.pdf#top">Rechnung</a>
            </body></html>"#
        )
    }

    #[test]
    fn extracts_invoice_credit_notes_and_summary() {
        let links = extract_links(&page(), BASE, CODE);
        let kinds: Vec<_> = links.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [
                DocKind::Summary,
                DocKind::Invoice,
                DocKind::CreditNote,
                DocKind::CreditNote
            ]
        );
        assert!(links.iter().all(|l| l.order_code == CODE));
        assert_eq!(
            links[0].url,
            format!("https://www.amazon.de/gp/css/summary/print.html?orderID={CODE}&ref=ppx")
        );
        assert_eq!(
            links[1].url,
            "https://www.amazon.de/documents/download/aaa-111/invoice.pdf"
        );
    }

    #[test]
    fn foreign_hosts_http_and_unrelated_links_are_dropped() {
        let links = extract_links(&page(), BASE, CODE);
        assert!(
            links
                .iter()
                .all(|l| l.url.starts_with("https://www.amazon.de/"))
        );
        assert!(
            extract_links("<a href='https://notamazon.de/x'>Rechnung</a>", BASE, CODE).is_empty()
        );
        assert!(extract_links(&page(), "not a url", CODE).is_empty());
    }

    #[test]
    fn order_code_comes_from_link_else_fallback() {
        let html = r#"<a href="/gp/css/summary/print.html?orderID=111-2222222-3333333">x</a>
                      <a href="/documents/download/q/invoice.pdf">Invoice</a>"#;
        let links = extract_links(html, "https://www.amazon.com/", CODE);
        assert_eq!(links[0].order_code, "111-2222222-3333333");
        assert_eq!(links[1].order_code, CODE);
        assert_eq!(links[1].kind, DocKind::Invoice);
    }

    fn download(links: &[DocLink], bytes: &[u8]) -> Vec<Downloaded> {
        links
            .iter()
            .map(|l| Downloaded {
                link: l.clone(),
                bytes: bytes.to_vec(),
            })
            .collect()
    }

    fn bookings() -> Vec<Booking> {
        vec![
            booking(&format!("amazon:{CODE}"), "2026-10-01"),
            booking(&format!("amazon-refund:{CODE}:0:500"), "2026-10-01"),
            booking(&format!("amazon-refund:{CODE}:1:250"), "2026-10-01"),
        ]
    }

    #[test]
    fn documents_are_attached_to_order_and_refund_bookings() {
        let links = extract_links(&page(), BASE, CODE);
        let docs = to_remote_documents("amazon-orders-7", &bookings(), download(&links, PDF));
        assert_eq!(docs.len(), 4);
        let ext = |i: usize| docs[i].booking_external_id.clone().unwrap();
        assert_eq!(ext(0), format!("amazon:{CODE}"));
        assert_eq!(docs[0].doc_type, "beleg");
        assert_eq!(ext(1), format!("amazon:{CODE}"));
        assert_eq!(docs[1].doc_type, "rechnung");
        assert_eq!(ext(2), format!("amazon-refund:{CODE}:0:500"));
        assert_eq!(docs[2].doc_type, "gutschrift");
        assert_eq!(ext(3), format!("amazon-refund:{CODE}:1:250"));
        assert!(docs.iter().all(|d| d.mime_type == "application/pdf"
            && d.account_ref == "amazon-orders-7"
            && d.date == "2026-10-01"));
        let mut urls: Vec<_> = docs.iter().map(|d| d.source_url.clone()).collect();
        urls.sort();
        urls.dedup();
        assert_eq!(urls.len(), 4, "every document has its own key");
    }

    #[test]
    fn source_urls_are_stable_across_runs() {
        let links = extract_links(&page(), BASE, CODE);
        let a = to_remote_documents("amazon-orders-7", &bookings(), download(&links, PDF));
        let b = to_remote_documents("amazon-orders-7", &bookings(), download(&links, PDF));
        let key = |v: &[RemoteDocument]| v.iter().map(|d| d.source_url.clone()).collect::<Vec<_>>();
        assert_eq!(key(&a), key(&b));
        assert!(key(&a)[1].starts_with("broker:amazon:amazon-orders-7:"));
    }

    #[test]
    fn non_pdf_and_unknown_orders_are_skipped() {
        let links = extract_links(&page(), BASE, CODE);
        assert!(
            to_remote_documents("a", &bookings(), download(&links, b"<html>login</html>"))
                .is_empty()
        );
        // order not among the bookings (cancelled): invoice dropped, credit notes stay unlinked
        let docs = to_remote_documents("a", &[], download(&links, PDF));
        assert_eq!(docs.len(), 2);
        assert!(docs.iter().all(|d| d.booking_external_id.is_none()));
        // no order code at all
        let anon = extract_links("<a href='/d/invoice.pdf'>Rechnung</a>", BASE, "");
        assert!(to_remote_documents("a", &bookings(), download(&anon, PDF)).is_empty());
    }

    #[test]
    fn extra_credit_note_without_refund_stays_unlinked() {
        let only_one = vec![
            booking(&format!("amazon:{CODE}"), "2026-10-01"),
            booking(&format!("amazon-refund:{CODE}:0:500"), "2026-10-01"),
        ];
        let links = extract_links(&page(), BASE, CODE);
        let docs = to_remote_documents("a", &only_one, download(&links, PDF));
        assert_eq!(docs.len(), 4);
        assert!(docs[2].booking_external_id.is_some());
        assert!(docs[3].booking_external_id.is_none());
    }
}
