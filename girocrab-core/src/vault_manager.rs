//! Multiple vaults in one directory (#410): listing, creating and deleting
//! individual `.girocrab` vault files together with their SQLite sidecars.
//!
//! The module only deals with files; opening and keying a vault stays in
//! [`crate::vault`].

use std::path::{Path, PathBuf};

use crate::error::CoreError;
use crate::i18n::tr;

/// File extension of a vault (without the dot).
pub const VAULT_EXTENSION: &str = "girocrab";

/// Suffixes appended to the vault path by SQLite (WAL mode, rollback journal).
const SIDECAR_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];

const MAX_NAME_CHARS: usize = 100;

/// One vault found in a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultEntry {
    /// Stable identifier: the file name without the extension.
    pub id: String,
    /// Display name (currently equal to the id).
    pub name: String,
    /// Path of the main vault file.
    pub path: PathBuf,
}

/// All files that belong to a single vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultResources {
    /// The main database file.
    pub main: PathBuf,
    /// `-wal`, `-shm` and `-journal` files (they may not exist).
    pub sidecars: Vec<PathBuf>,
}

fn io_err(path: &Path, e: std::io::Error) -> CoreError {
    CoreError::Database {
        message: format!("{}: {e}", path.display()),
    }
}

fn has_vault_extension(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some(VAULT_EXTENSION)
}

fn entry_for(path: PathBuf) -> Option<VaultEntry> {
    if !has_vault_extension(&path) {
        return None;
    }
    let stem = path.file_stem()?.to_str()?.to_string();
    if stem.is_empty() {
        return None;
    }
    Some(VaultEntry {
        id: stem.clone(),
        name: stem,
        path,
    })
}

/// Checks a user-chosen vault name; returns it trimmed.
fn validate_name(name: &str) -> Result<String, CoreError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CoreError::invalid(tr(
            "Der Name des Tresors darf nicht leer sein",
        )));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(CoreError::invalid(tr("Der Name des Tresors ist zu lang")));
    }
    if name.starts_with('.')
        || name.ends_with('.')
        || name.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
    {
        return Err(CoreError::invalid(tr(
            "Der Name des Tresors enthält unzulässige Zeichen",
        )));
    }
    Ok(name.to_string())
}

impl VaultResources {
    /// Resources of the vault whose main file is `path`.
    pub fn for_path(path: &Path) -> Self {
        let sidecars = SIDECAR_SUFFIXES
            .iter()
            .map(|suffix| {
                let mut os = path.as_os_str().to_owned();
                os.push(suffix);
                PathBuf::from(os)
            })
            .collect();
        Self {
            main: path.to_path_buf(),
            sidecars,
        }
    }

    /// Main file followed by all sidecars, existing or not.
    pub fn all_paths(&self) -> Vec<&Path> {
        std::iter::once(self.main.as_path())
            .chain(self.sidecars.iter().map(PathBuf::as_path))
            .collect()
    }

    /// Only the files that exist right now.
    pub fn existing(&self) -> Vec<PathBuf> {
        self.all_paths()
            .into_iter()
            .filter(|p| p.exists())
            .map(Path::to_path_buf)
            .collect()
    }

    /// Deletes this vault's files (sidecars first, the main file last, so an
    /// interrupted run never leaves sidecars of a vanished vault behind
    /// unnoticed). Other vaults are never touched. Missing files are fine.
    pub fn delete(&self) -> Result<(), CoreError> {
        if !has_vault_extension(&self.main) || self.main.file_name().is_none() || self.main.is_dir()
        {
            return Err(CoreError::invalid(tr("Keine Tresor-Datei")));
        }
        for p in self.sidecars.iter().chain(std::iter::once(&self.main)) {
            match std::fs::remove_file(p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_err(p, e)),
            }
        }
        Ok(())
    }
}

/// Lists the vaults (`*.girocrab`) directly inside `dir`, sorted by name.
/// A missing directory yields an empty list.
pub fn list_vaults(dir: &Path) -> Result<Vec<VaultEntry>, CoreError> {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err(dir, e)),
    };
    let mut out = Vec::new();
    for item in read {
        let item = item.map_err(|e| io_err(dir, e))?;
        let is_file = item.file_type().map(|t| t.is_file()).unwrap_or(false);
        if is_file {
            out.extend(entry_for(item.path()));
        }
    }
    out.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(out)
}

/// Reserves a new vault named `name` in `dir` (created if missing) by
/// exclusively creating an empty `<name>.girocrab`; SQLite treats an empty file
/// as a new database, so [`crate::vault::Vault`] can open it with a key next.
/// Fails when a vault of that name exists (compared case-insensitively).
pub fn create_vault(dir: &Path, name: &str) -> Result<VaultEntry, CoreError> {
    let name = validate_name(name)?;
    std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    let clash = list_vaults(dir)?
        .iter()
        .any(|v| v.name.to_lowercase() == name.to_lowercase());
    let path = dir.join(format!("{name}.{VAULT_EXTENSION}"));
    if clash {
        return Err(CoreError::invalid(tr(
            "Ein Tresor mit diesem Namen existiert bereits",
        )));
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                CoreError::invalid(tr("Ein Tresor mit diesem Namen existiert bereits"))
            } else {
                io_err(&path, e)
            }
        })?;
    entry_for(path).ok_or_else(|| CoreError::invalid(tr("Ungültiger Tresorname")))
}

/// Deletes the vault `entry` with all its sidecars; other vaults stay untouched.
pub fn delete_vault(entry: &VaultEntry) -> Result<(), CoreError> {
    VaultResources::for_path(&entry.path).delete()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "girocrab-vault-manager-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn create_and_list_sorted() {
        let dir = temp_dir("list");
        create_vault(&dir, "Zweit").unwrap();
        create_vault(&dir, "alpha").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        std::fs::write(dir.join("alpha.girocrab-wal"), b"x").unwrap();
        std::fs::create_dir(dir.join("dir.girocrab")).unwrap();
        let names: Vec<_> = list_vaults(&dir)
            .unwrap()
            .into_iter()
            .map(|v| v.name)
            .collect();
        assert_eq!(names, ["alpha", "Zweit"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_missing_dir_is_empty() {
        let dir = temp_dir("missing").join("nope");
        assert!(list_vaults(&dir).unwrap().is_empty());
        std::fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn create_rejects_bad_and_duplicate_names() {
        let dir = temp_dir("create");
        create_vault(&dir, "Privat").unwrap();
        assert!(create_vault(&dir, "privat").is_err());
        for bad in ["", "  ", "../x", "a/b", ".hidden", "a\\b", "x\0y"] {
            assert!(create_vault(&dir, bad).is_err(), "{bad:?}");
        }
        assert_eq!(list_vaults(&dir).unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn duplicate_name_is_reported_in_the_ui_language() {
        use crate::i18n::{Lang, with_language};
        let dir = temp_dir("duplicate-en");
        create_vault(&dir, "Privat").unwrap();
        let result = with_language(Lang::En, || create_vault(&dir, "privat"));
        std::fs::remove_dir_all(&dir).ok();
        let Err(CoreError::InvalidInput { message }) = result else {
            panic!("expected InvalidInput");
        };
        assert_eq!(message, "A vault with this name already exists");
    }

    #[test]
    fn resources_cover_sidecars() {
        let res = VaultResources::for_path(Path::new("/x/Privat.girocrab"));
        let all: Vec<_> = res
            .all_paths()
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            all,
            [
                "/x/Privat.girocrab",
                "/x/Privat.girocrab-wal",
                "/x/Privat.girocrab-shm",
                "/x/Privat.girocrab-journal",
            ]
        );
    }

    #[test]
    fn delete_removes_only_one_vault() {
        let dir = temp_dir("delete");
        let a = create_vault(&dir, "A").unwrap();
        let b = create_vault(&dir, "A2").unwrap();
        for suffix in ["-wal", "-shm"] {
            for v in [&a, &b] {
                let mut os = v.path.as_os_str().to_owned();
                os.push(suffix);
                std::fs::write(PathBuf::from(os), b"x").unwrap();
            }
        }
        assert_eq!(VaultResources::for_path(&a.path).existing().len(), 3);
        delete_vault(&a).unwrap();
        assert!(VaultResources::for_path(&a.path).existing().is_empty());
        assert_eq!(VaultResources::for_path(&b.path).existing().len(), 3);
        assert_eq!(list_vaults(&dir).unwrap(), vec![b.clone()]);
        // Deleting again is harmless.
        delete_vault(&a).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn delete_refuses_non_vault_files() {
        let dir = temp_dir("refuse");
        let other = dir.join("keep.txt");
        std::fs::write(&other, b"x").unwrap();
        let entry = VaultEntry {
            id: "keep".into(),
            name: "keep".into(),
            path: other.clone(),
        };
        assert!(delete_vault(&entry).is_err());
        assert!(other.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
