//! The clones on this machine whose worktree directories share one budget.
//!
//! A run that gives a worktree its directory registers the worktree's clone,
//! once, in the user's state directory: `$XDG_STATE_HOME/sykli/clones/`, or
//! `~/.local/state/sykli/clones/` without it. One file per clone, named by
//! the first 16 hex digits of the SHA-256 of its common git directory's
//! resolved path, and holding that path; written whole, never over another.
//! A run that bounds the budget, and `clean`, read every clone's store
//! (`<common>/sykli/worktrees`) from the file system: no git process per
//! clone.
//!
//! Bound: one file per clone whose worktrees a run gave a directory. A
//! clone's file is removed by the first run that bounds the budget, or
//! `clean --apply`, that finds its store gone, as it is once the clone is
//! deleted; a run there registers it again. Nothing else is kept there.
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// The registry: `<state>/sykli/clones`, where `<state>` is
/// `XDG_STATE_HOME` when it is an absolute path, or `~/.local/state`.
/// `None` when neither names one.
fn registry() -> Option<PathBuf> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let state = absolute("XDG_STATE_HOME")
        .or_else(|| absolute("HOME").map(|home| home.join(".local").join("state")))?;
    Some(state.join("sykli").join("clones"))
}

const NO_REGISTRY: &str = "neither XDG_STATE_HOME nor HOME names an absolute directory";

/// The file name of the clone whose resolved common git directory is
/// `common`.
fn name(common: &Path) -> String {
    crate::sha256(common.as_os_str().as_bytes())[..super::ID_LENGTH].to_owned()
}

/// Register the clone whose resolved common git directory is `common`, if
/// it is not yet: one look at the file when it is.
pub(super) fn register(common: &Path) -> Result<(), String> {
    let registry = registry().ok_or(NO_REGISTRY)?;
    let file = registry.join(name(common));
    if fs::symlink_metadata(&file).is_ok() {
        return Ok(());
    }
    let sykli = registry.parent().ok_or("no parent")?;
    if let Some(state) = sykli.parent() {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(state)
            .map_err(|e| format!("{}: {e}", state.display()))?;
    }
    for directory in [sykli, registry.as_path()] {
        crate::hook::ensure_directory(directory, |path| fs::create_dir(path))?;
    }
    let mut bytes = common.as_os_str().as_bytes().to_vec();
    bytes.push(b'\n');
    crate::canonical::publish(&file, &bytes)
}

/// What the registry holds: a clone, by its file and common git directory,
/// or an entry that names none.
pub(super) enum Registered {
    Clone { file: PathBuf, common: PathBuf },
    Unknown(String),
}

/// Every registration; none when nothing was registered yet. An error when
/// the registry cannot be found or read: then no other clone is known.
pub(super) fn registered() -> Result<Vec<Registered>, String> {
    let registry = registry().ok_or(NO_REGISTRY)?;
    let entries = match fs::read_dir(&registry) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", registry.display())),
    };
    let mut found = Vec::new();
    for entry in entries {
        let file = match entry {
            Ok(entry) => entry.path(),
            Err(e) => {
                found.push(Registered::Unknown(format!("{}: {e}", registry.display())));
                continue;
            }
        };
        // `publish` writes a temporary beside its file, then links it.
        if file
            .file_name()
            .is_some_and(|name| name.as_bytes().starts_with(b"."))
        {
            continue;
        }
        found.push(match common(&file) {
            Ok(common) => Registered::Clone { file, common },
            Err(error) => Registered::Unknown(format!("{}: {error}", file.display())),
        });
    }
    Ok(found)
}

/// The common git directory a registration names, if it names one by the
/// name it is filed under.
fn common(file: &Path) -> Result<PathBuf, String> {
    let bytes = fs::read(file).map_err(|e| e.to_string())?;
    let path = PathBuf::from(OsStr::from_bytes(
        bytes.strip_suffix(b"\n").unwrap_or(&bytes),
    ));
    if !path.is_absolute() || file.file_name() != Some(OsStr::new(&name(&path))) {
        return Err("does not name a clone's common git directory".into());
    }
    Ok(path)
}

/// Drop a registration whose clone is gone. Best effort: a later run drops
/// it otherwise.
pub(super) fn forget(file: &Path) {
    let _ = fs::remove_file(file);
}

/// A clone's store, `<common>/sykli/worktrees`; `None` once it is gone (the
/// clone was deleted). Refused when any part of it is not a directory:
/// symbolic links are never followed.
pub(super) fn store(common: &Path) -> Result<Option<PathBuf>, String> {
    let mut path = common.to_path_buf();
    for part in [None, Some("sykli"), Some("worktrees")] {
        if let Some(part) = part {
            path.push(part);
        }
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(format!(
                    "{} is not a directory (symbolic links are not followed)",
                    path.display()
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok(Some(path))
}
