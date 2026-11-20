//! Shared helpers for brokerage connectors (Trade Republic, Scalable Capital,
//! DKB) that download PDF statements:
//!
//! * [`validate_pdf`] – reject anything that is not a PDF (error pages, JSON),
//! * [`source_url`] / [`content_source_url`] – deterministic `source_url` keys,
//!   so `booking_documents` (unique index on `source_url`) deduplicates repeated
//!   downloads,
//! * [`mirror_pdf`] – copy a PDF to a local archive directory (`0600` on Unix).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{CoreError, CoreResult};

const PDF_MAGIC: &[u8] = b"%PDF-";

/// `true` if `bytes` start with the PDF header `%PDF-`.
pub fn is_pdf(bytes: &[u8]) -> bool {
    bytes.starts_with(PDF_MAGIC)
}

/// Fails with [`CoreError::Protocol`] unless `bytes` is a PDF.
pub fn validate_pdf(bytes: &[u8]) -> CoreResult<()> {
    if is_pdf(bytes) {
        Ok(())
    } else {
        Err(CoreError::protocol(format!(
            "downloaded document is not a PDF ({} bytes, missing %PDF- header)",
            bytes.len()
        )))
    }
}

/// Replaces everything except `[A-Za-z0-9._-]` so a value is safe inside a key
/// or a file name (no path separators, no `..` escapes).
fn sanitize_component(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Deterministic dedup key from the provider's own document id:
/// `broker:<provider>:<account_ref>:<document_id>`. Provider, account and id
/// are lower-cased/sanitized for provider and account; the id keeps its case.
/// Presigned query strings must not be part of `document_id`; use
/// [`strip_query`] on URLs first.
pub fn source_url(provider: &str, account_ref: &str, document_id: &str) -> String {
    format!(
        "broker:{}:{}:{}",
        sanitize_component(&provider.to_ascii_lowercase()),
        sanitize_component(account_ref),
        sanitize_component(document_id)
    )
}

/// Fallback key when the broker gives no stable id: SHA-256 of the content,
/// `broker:<provider>:<account_ref>:sha256-<hex>`.
pub fn content_source_url(provider: &str, account_ref: &str, bytes: &[u8]) -> String {
    let digest = hex::encode(Sha256::digest(bytes));
    source_url(provider, account_ref, &format!("sha256-{digest}"))
}

/// Drops query string and fragment from a URL (presigned parameters change on
/// every request and would defeat deduplication).
pub fn strip_query(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

/// Writes `bytes` as `<dir>/<provider>/<file_name>` (names sanitized, `.pdf`
/// ensured), creating directories as needed. On Unix the file is `0600` and
/// the directories `0700`. The write is atomic (temp file + rename), so a
/// crash leaves no half-written PDF. Returns the final path.
pub fn mirror_pdf(
    archive_dir: &Path,
    provider: &str,
    file_name: &str,
    bytes: &[u8],
) -> CoreResult<PathBuf> {
    validate_pdf(bytes)?;
    let dir = archive_dir.join(sanitize_component(&provider.to_ascii_lowercase()));
    create_private_dir(&dir)?;

    let mut name = sanitize_component(file_name)
        .trim_start_matches(['.', '_'])
        .to_string();
    if name.is_empty() {
        name = "document".into();
    }
    if !name.to_ascii_lowercase().ends_with(".pdf") {
        name.push_str(".pdf");
    }
    let target = dir.join(&name);
    let tmp = dir.join(format!(".{name}.tmp"));

    let io = |e: std::io::Error| CoreError::invalid(format!("archive write failed: {e}"));
    let result = (|| -> std::io::Result<()> {
        let mut f = open_private(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, &target)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(io(e));
    }
    Ok(target)
}

fn create_private_dir(dir: &Path) -> CoreResult<()> {
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
        .map_err(|e| CoreError::invalid(format!("archive directory failed: {e}")))
}

fn open_private(path: &Path) -> std::io::Result<fs::File> {
    let mut o = fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "girocrab-broker-docs-{tag}-{}-{}",
            std::process::id(),
            &hex::encode(Sha256::digest(tag.as_bytes()))[..8]
        ));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn validates_pdf_header() {
        assert!(is_pdf(b"%PDF-1.7\n..."));
        assert!(validate_pdf(b"%PDF-1.4").is_ok());
        assert!(!is_pdf(b"%PDF"));
        assert!(!is_pdf(b"<html>"));
        assert!(!is_pdf(b""));
        assert!(matches!(
            validate_pdf(b"{\"error\":1}"),
            Err(CoreError::Protocol { .. })
        ));
    }

    #[test]
    fn source_url_is_deterministic_and_safe() {
        let a = source_url("Trade Republic", "tr:cash", "abc/../123");
        assert_eq!(a, source_url("Trade Republic", "tr:cash", "abc/../123"));
        assert_eq!(a, "broker:trade_republic:tr_cash:abc_.._123");
        assert_ne!(a, source_url("scalable", "tr:cash", "abc/../123"));
        assert!(!a.contains('/'));
    }

    #[test]
    fn content_key_depends_on_bytes() {
        let a = content_source_url("dkb", "acc", b"%PDF-1");
        assert_eq!(a, content_source_url("dkb", "acc", b"%PDF-1"));
        assert_ne!(a, content_source_url("dkb", "acc", b"%PDF-2"));
        assert!(a.starts_with("broker:dkb:acc:sha256-"));
    }

    #[test]
    fn strip_query_removes_presigned_part() {
        assert_eq!(
            strip_query("https://x.example/a/b.pdf?sig=1&exp=2#f"),
            "https://x.example/a/b.pdf"
        );
        assert_eq!(strip_query("https://x.example/a"), "https://x.example/a");
    }

    #[test]
    fn mirror_writes_pdf_privately() {
        let root = scratch("mirror");
        let p = mirror_pdf(&root, "DKB", "../evil name", b"%PDF-1.7 body").unwrap();
        assert_eq!(p, root.join("dkb").join("evil_name.pdf"));
        assert_eq!(fs::read(&p).unwrap(), b"%PDF-1.7 body");
        // idempotent overwrite, no temp leftovers
        mirror_pdf(&root, "DKB", "../evil name", b"%PDF-1.7 new").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"%PDF-1.7 new");
        assert_eq!(fs::read_dir(root.join("dkb")).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&p), 0o600);
            assert_eq!(mode(&root.join("dkb")), 0o700);
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn mirror_rejects_non_pdf() {
        let root = scratch("reject");
        assert!(mirror_pdf(&root, "dkb", "a.pdf", b"nope").is_err());
        assert!(!root.exists());
    }
}
