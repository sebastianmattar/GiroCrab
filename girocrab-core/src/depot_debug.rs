//! Structural, non-sensitive trace of depot/holdings syncs.
//!
//! **Off by default.** Only active when the environment variable
//! `GIROCRAB_DEPOT_DEBUG=1` is set; then it appends to
//! `girocrab-depot-debug.log` in the per-user temporary directory
//! (`$TMPDIR`). The file is created with mode `0600`, never through a
//! symbolic link, and truncated once it grows beyond 5 MB.
//!
//! NEVER pass account numbers, IBANs, WKNs/ISINs, names or amounts here –
//! only step names, counts, codes and versions.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Environment switch (`1` = on).
pub const ENV: &str = "GIROCRAB_DEPOT_DEBUG";
const FILE_NAME: &str = "girocrab-depot-debug.log";
/// Size cap: a larger log is truncated before the next line is written.
const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// Whether `GIROCRAB_DEPOT_DEBUG=1` is set (read once per process).
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var(ENV).is_ok_and(|v| v.trim() == "1"))
}

/// Location of the log (per-user temporary directory).
pub fn path() -> PathBuf {
    std::env::temp_dir().join(FILE_NAME)
}

pub fn log(msg: &str) {
    if enabled() {
        let _ = append(&path(), msg);
    }
}

fn open_private(path: &Path) -> std::io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // owner-only, and never follow a (planted) symbolic link
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let f = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = f.metadata()?;
        // a hard link to someone else's file, or a file of another user
        if meta.nlink() > 1 || meta.uid() != crate::sys::getuid() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "debug log is not a private file",
            ));
        }
        // an older, world-readable log from a previous version
        if meta.mode() & 0o077 != 0 {
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(f)
}

fn append(path: &Path, msg: &str) -> std::io::Result<()> {
    let mut f = open_private(path)?;
    if f.metadata()?.len() > MAX_BYTES {
        f.set_len(0)?;
    }
    writeln!(f, "{} {msg}", crate::connectors::today())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("girocrab-debug-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn off_without_env() {
        // the test process never sets GIROCRAB_DEPOT_DEBUG
        if std::env::var(ENV).is_err() {
            assert!(!enabled());
        }
    }

    #[cfg(unix)]
    #[test]
    fn private_file_and_size_cap() {
        use std::os::unix::fs::PermissionsExt;
        let p = tmp("cap");
        append(&p, "one").unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        // oversize → truncated before the next line
        let f = OpenOptions::new().append(true).open(&p).unwrap();
        f.set_len(MAX_BYTES + 1).unwrap();
        append(&p, "two").unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.ends_with(" two\n") && !text.contains("one"));
        // a world-readable leftover gets tightened
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        append(&p, "three").unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_file(&p);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink() {
        let target = tmp("target");
        std::fs::write(&target, b"victim\n").unwrap();
        let link = tmp("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(append(&link, "x").is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "victim\n");
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_file(&target);
    }
}
