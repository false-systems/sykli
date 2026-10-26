//! `${worktree_cache}`: a directory of each worktree's own, outside it.
//!
//! A task's declared `env` values may name `${worktree_cache}`. It expands to
//! `<git-common-dir>/sykli/worktrees/<id>/cache`. `<id>` is the first 16 hex
//! digits of the identity (`sykli-worktree.v1`) of the worktree's
//! incarnation: its top level and its own git directory, both with symbolic
//! links resolved, and the token in that git directory's `sykli-incarnation`
//! (made by the first run that needs it). Git deletes a worktree's git
//! directory with it, so a worktree removed and made again at the same path,
//! which git gives the same git directory, is another incarnation and gets
//! another directory: it never inherits the build of the one it replaced,
//! which a checkout that restores file times would find fresh.
//!
//! The directory is made when a run needs it and lies outside every work
//! tree, so it changes neither `git status` nor any tree a receipt binds. It
//! is for a build tool's output: with one Cargo target directory for every
//! worktree (`[build] target-dir` in Cargo's own configuration), the final
//! artifacts land in shared `debug/<name>` paths, and a run in one worktree
//! can run a binary another worktree built; its receipt would claim a pass
//! for code that never ran. A shared `[build] build-dir` does the same with
//! the intermediate artifacts. `CARGO_TARGET_DIR=${worktree_cache}/cargo-target`
//! with `CARGO_BUILD_BUILD_DIR=${worktree_cache}/cargo-build` gives each
//! worktree its own (a Cargo without `build-dir` ignores the second), at the
//! cost of a cold first build in each.
//!
//! The directory is the tasks' own: sykli keeps what it needs beside it in
//! `<id>/` (`worktree.json`, whose directory this is, and `lease`), so a task
//! that empties or removes it (`cargo clean`) takes nothing of sykli's, and
//! the next run makes it again.
//!
//! **Where a contract is written or run** (`validate`, `lock`, `plan`,
//! `run`, `resume`, the pre-commit hook): in an `env` value any other
//! `${…}`, one left unclosed, or a `$${` (kept free for an escape of `${`
//! later, so that one never changes what a valid contract means) is
//! refused, never handed to a task literally, and so is `$worktree_cache`
//! without braces, which sykli does not expand; and the placeholder is
//! refused anywhere else in a task, where a shell would make it empty. Any
//! other `$` that opens no `${` is kept as written. **Where a contract is
//! judged as evidence** (`verify`, `status --commit`, the pre-push hook),
//! none of this applies: a contract run before these rules keeps the
//! meaning it ran with.
//!
//! **What a cache key covers is the declaration**, `${worktree_cache}` as
//! written, never the directory it names (the task is keyed as declared). A
//! pass is evidence about the declared inputs it ran with, and where the
//! build went does not change what was built once it went somewhere no other
//! worktree writes. So identical work in two worktrees has one key, a
//! worktree that moves keeps its cached passes, and a contract that moves
//! from a shared directory to `${worktree_cache}` changes its declaration:
//! no pass earned under the shared one is reused. Sykli records the
//! directory nowhere, in no receipt and no journal; a task's own captured
//! output may name it.
//!
//! Retention (Linux and macOS): a process that expands `${worktree_cache}`
//! holds the directory's lease shared until it ends, and so does every task
//! that names it, and what that task starts (they inherit it, as they inherit
//! an attempt's lease). Whenever a run is recorded, in any worktree, the
//! directory of every worktree incarnation that is gone is set aside, never
//! one whose lease is held, and the run deletes what it set aside once its
//! work is done, so a large directory never delays a run's start. An
//! incarnation is gone when its git directory holds another token or none,
//! or its top level's `.git` no longer leads to that git directory (it was
//! removed or moved), unless git keeps it locked (`git worktree lock`, for
//! one on removable media). Windows has no leases here, so there these
//! directories are made but never pruned.
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use sykli::{Contract, Task};

/// The one placeholder there is.
const WORKTREE_CACHE: &str = "worktree_cache";

/// What names a worktree's directory, as `worktree.json` keeps it.
const MARKER: &str = "sykli-worktree-cache.v1";

/// The directory a worktree's tasks get, inside its `<id>/`.
const HANDED_OUT: &str = "cache";

/// A piece of a declared `env` value.
enum Piece<'a> {
    Text(&'a str),
    WorktreeCache,
}

/// A declared value in pieces, or why it cannot be expanded.
fn pieces(value: &str) -> Result<Vec<Piece<'_>>, String> {
    let mut pieces = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        if start > 0 {
            pieces.push(Piece::Text(&rest[..start]));
        }
        let inside = &rest[start + 2..];
        let Some(end) = inside.find('}') else {
            return Err(format!(
                "an unclosed placeholder {:?}; the only placeholder is ${{{WORKTREE_CACHE}}}",
                &rest[start..]
            ));
        };
        let name = &inside[..end];
        if name != WORKTREE_CACHE {
            return Err(format!(
                "an unknown placeholder ${{{name}}}; the only placeholder is ${{{WORKTREE_CACHE}}}"
            ));
        }
        pieces.push(Piece::WorktreeCache);
        rest = &inside[end + 1..];
    }
    if !rest.is_empty() {
        pieces.push(Piece::Text(rest));
    }
    Ok(pieces)
}

/// What a task may say about placeholders where its contract is written or
/// run (never where it is judged): in an `env` value, only
/// `${worktree_cache}`, closed, no `$${` (kept free for an escape), and no
/// `$worktree_cache` without braces, which a task would get as written;
/// anywhere else, not the placeholder at all, which a shell would make empty.
pub(crate) fn check(task: &Task) -> Result<(), String> {
    for (name, value) in &task.env {
        if value.contains("$${") {
            return Err(format!(
                "task {:?} env {name} uses \"$${{\", which is reserved for an escape of \"${{\"; \
                 the only placeholder is ${{{WORKTREE_CACHE}}}",
                task.name
            ));
        }
        pieces(value).map_err(|error| format!("task {:?} env {name} uses {error}", task.name))?;
        if names_unbraced(value) {
            return Err(format!(
                "task {:?} env {name} names ${WORKTREE_CACHE} without braces, which sykli does \
                 not expand (a task would get it as written); write ${{{WORKTREE_CACHE}}}",
                task.name
            ));
        }
    }
    let mut elsewhere = vec![("its run command", task.run.as_str())];
    if let Some(workdir) = task.workdir.as_ref().and_then(|path| path.to_str()) {
        elsewhere.push(("its workdir", workdir));
    }
    elsewhere.extend(task.inputs.iter().map(|path| ("its inputs", path.as_str())));
    elsewhere.extend(
        task.outputs
            .iter()
            .map(|path| ("its outputs", path.as_str())),
    );
    elsewhere.extend(task.env.keys().map(|name| ("an env name", name.as_str())));
    for (field, text) in elsewhere {
        if names_placeholder(text) {
            return Err(format!(
                "task {:?} names ${{{WORKTREE_CACHE}}} in {field}, where sykli does not expand it \
                 (a shell makes it empty); declare it in an env value, as \
                 \"env\": {{\"DIR\": \"${{{WORKTREE_CACHE}}}\"}}, and use \"$DIR\"",
                task.name
            ));
        }
    }
    Ok(())
}

/// Whether `text` names the placeholder as a shell variable would:
/// `$worktree_cache` or `${worktree_cache…`, the name not going on.
fn names_placeholder(text: &str) -> bool {
    let mut rest = text;
    while let Some(at) = rest.find(WORKTREE_CACHE) {
        let before = &rest[..at];
        let after = &rest[at + WORKTREE_CACHE.len()..];
        let ends = !after.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_');
        if ends && (before.ends_with('$') || before.ends_with("${")) {
            return true;
        }
        rest = after;
    }
    false
}

/// Whether `text` names the placeholder as `$worktree_cache`, without
/// braces, the name not going on: in an `env` value sykli expands only
/// `${worktree_cache}`, so this one would reach the task as written.
fn names_unbraced(text: &str) -> bool {
    let mut rest = text;
    while let Some(at) = rest.find(WORKTREE_CACHE) {
        let after = &rest[at + WORKTREE_CACHE.len()..];
        let ends = !after.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_');
        if ends && rest[..at].ends_with('$') {
            return true;
        }
        rest = after;
    }
    false
}

/// Whether a validated task's `env` names `${worktree_cache}`.
pub(crate) fn named_by(task: &Task) -> bool {
    task.env.values().any(|value| value.contains("${"))
}

/// The directory this process expands `${worktree_cache}` to, with its
/// lease held shared for as long as the process lives.
struct Prepared {
    directory: PathBuf,
    #[cfg(unix)]
    lease: crate::journal::Lease,
}

static PREPARED: OnceLock<Prepared> = OnceLock::new();

/// Make this worktree's directory ready for `contract`, when one of its
/// tasks names it: made if it is missing, and its lease held until this
/// process ends. A contract that names no placeholder costs nothing.
pub(crate) fn prepare(contract: &Contract) -> Result<(), String> {
    if PREPARED.get().is_some() || !contract.tasks.iter().any(named_by) {
        return Ok(());
    }
    let repository = crate::repo::repository()
        .map_err(|_| "${worktree_cache} names a directory of a git worktree, and there is no git repository here".to_string())?;
    let common = fs::canonicalize(&repository.common_dir)
        .map(usable)
        .map_err(|error| format!("{}: {error}", repository.common_dir.display()))?;
    let root = repository
        .resolved()
        .ok_or_else(|| format!("{}: cannot be resolved", repository.root.display()))?;
    let gitdir = gitdir_of(root).ok_or_else(|| {
        format!(
            "${{worktree_cache}}: cannot tell which worktree {} is: its .git is neither a git \
             directory nor a `gitdir:` file",
            root.display()
        )
    })?;
    let worktree = Worktree {
        root: text(root)?,
        incarnation: incarnation(&gitdir)?,
        gitdir: text(&gitdir)?,
    };
    let prepared = held(&common, &worktree)?;
    let _ = PREPARED.set(prepared);
    Ok(())
}

/// A resolved path as the tools a task runs take it. On Windows,
/// `fs::canonicalize` gives a verbatim path (`\\?\C:\…`), in which `/` is
/// not a separator and which a POSIX `sh` does not understand, so the
/// expanded value is the same path without that prefix.
fn usable(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    if let Some(text) = path.to_str() {
        if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{share}"));
        }
        if let Some(local) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(local);
        }
    }
    path
}

/// The task's declared `env`, with `${worktree_cache}` expanded.
pub(crate) fn environment(task: &Task) -> Result<Vec<(&str, OsString)>, String> {
    task.env
        .iter()
        .map(|(name, value)| {
            let mut expanded = OsString::new();
            for piece in pieces(value)? {
                match piece {
                    Piece::Text(text) => expanded.push(text),
                    Piece::WorktreeCache => expanded.push(
                        &PREPARED
                            .get()
                            .ok_or("${worktree_cache} was not prepared for this run")?
                            .directory,
                    ),
                }
            }
            Ok((name.as_str(), expanded))
        })
        .collect()
}

/// Let a task that names the directory, and everything it starts, hold its
/// lease: a task that outlives its run's coordinator keeps it from pruning.
#[cfg(unix)]
pub(crate) fn inherit(task: &Task, command: &mut std::process::Command) {
    if named_by(task) {
        if let Some(prepared) = PREPARED.get() {
            prepared.lease.inherit(command);
        }
    }
}

/// A worktree's incarnation: its top level and its own git directory,
/// resolved, and the token in that git directory.
#[derive(serde::Serialize)]
struct Worktree {
    root: String,
    gitdir: String,
    incarnation: String,
}

/// `worktree.json`: whose directory this is.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    schema: String,
    root: String,
    gitdir: String,
    incarnation: String,
}

/// Hex digits of an id: 64 bits. A clone holds at most thousands of
/// worktree incarnations over its life, so two sharing an id is about as
/// likely as n²/2⁶⁵ (below 10⁻¹¹ for ten thousand); the id is no secret
/// either way, since any process that can write the common git directory can
/// write there. The full identity would add 48 characters to every artifact
/// path, which counts against Windows' MAX_PATH.
const ID_LENGTH: usize = 16;

impl Worktree {
    fn id(&self) -> Result<String, String> {
        let identity = crate::canonical::identity("sykli-worktree.v1", self)?;
        Ok(identity[..ID_LENGTH].to_owned())
    }
}

/// Whether `name` is a directory id, as `Worktree::id` makes them.
#[cfg(unix)]
fn is_id(name: &str) -> bool {
    name.len() == ID_LENGTH
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The file in a worktree's own git directory that names its incarnation.
const INCARNATION: &str = "sykli-incarnation";

/// The token of the worktree whose git directory is `gitdir`, made by the
/// first run that needs it: published whole (synced, then linked into
/// place, never over another), so a concurrent first run reads this one.
/// Git ignores the file, and deletes it with the worktree's git directory.
fn incarnation(gitdir: &Path) -> Result<String, String> {
    if let Some(token) = current_incarnation(gitdir) {
        return Ok(token);
    }
    let token = crate::sha256(
        format!(
            "{}:{}:{}:{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|now| now.as_nanos())
                .unwrap_or_default(),
            crate::canonical::NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            gitdir.display()
        )
        .as_bytes(),
    );
    let published = crate::canonical::publish_with(
        &gitdir.join(INCARNATION),
        format!("{token}\n").as_bytes(),
        crate::canonical::Durability::Fsync,
    );
    match (published, current_incarnation(gitdir)) {
        // Ours, or another first run's that won.
        (_, Some(token)) => Ok(token),
        (Err(error), None) => Err(format!(
            "${{worktree_cache}}: cannot record this worktree's incarnation in {}: {error}",
            gitdir.join(INCARNATION).display()
        )),
        (Ok(()), None) => Err(format!(
            "${{worktree_cache}}: {} is unreadable",
            gitdir.join(INCARNATION).display()
        )),
    }
}

/// The token in `gitdir`, if there is one; never made.
fn current_incarnation(gitdir: &Path) -> Option<String> {
    fs::read_to_string(gitdir.join(INCARNATION))
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|token| !token.is_empty())
}

fn text(path: &Path) -> Result<String, String> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        format!(
            "${{worktree_cache}} needs a worktree whose paths are UTF-8; {} is not",
            path.display()
        )
    })
}

/// The git directory `root/.git` leads to, resolved: the directory itself
/// (a main checkout), or the one a `gitdir: PATH` file names (a linked
/// worktree, a submodule), relative to `root` when it is relative.
fn gitdir_of(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    let target = if dot_git.is_dir() {
        dot_git
    } else {
        let text = fs::read_to_string(&dot_git).ok()?;
        root.join(text.strip_prefix("gitdir: ")?.trim_end())
    };
    fs::canonicalize(target).ok()
}

/// `<common>/sykli/worktrees`, made if missing, never through a symbolic link.
fn parent(common: &Path) -> Result<PathBuf, String> {
    let mut parent = common.to_path_buf();
    for part in ["sykli", "worktrees"] {
        parent.push(part);
        crate::hook::ensure_directory(&parent, |path| fs::create_dir(path))?;
    }
    Ok(parent)
}

/// The worktree's directory, made if missing, with its lease held. A
/// worktree's `<id>/` is made aside, with its marker, its lease and the
/// directory tasks get, and renamed into place, so one under its final name
/// always says whose it is.
fn held(common: &Path, worktree: &Worktree) -> Result<Prepared, String> {
    let id = worktree.id()?;
    let parent = parent(common)?;
    let directory = parent.join(&id);
    let handed_out = || -> Result<PathBuf, String> {
        // A task may have removed it; it is the tasks' own.
        let path = directory.join(HANDED_OUT);
        crate::hook::ensure_directory(&path, |path| fs::create_dir(path))?;
        Ok(path)
    };
    #[cfg(not(unix))]
    {
        there(&parent, &id, worktree, &directory)?;
        Ok(Prepared {
            directory: handed_out()?,
        })
    }
    // A remover may move the directory aside between finding it and holding
    // its lease; then it is found, or made, again.
    #[cfg(unix)]
    {
        for _ in 0..3 {
            there(&parent, &id, worktree, &directory)?;
            if let Some(lease) = crate::journal::Lease::share(&directory.join("lease"))? {
                return Ok(Prepared {
                    directory: handed_out()?,
                    lease,
                });
            }
        }
        Err(format!(
            "{} keeps being removed while it is taken; not using it",
            directory.display()
        ))
    }
}

/// The worktree's directory is there: found, or made.
fn there(parent: &Path, id: &str, worktree: &Worktree, directory: &Path) -> Result<(), String> {
    match fs::symlink_metadata(directory) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(format!(
            "{} is not a directory; not using it",
            directory.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            make(parent, id, worktree, directory)
        }
        Err(error) => Err(format!("{}: {error}", directory.display())),
    }
}

fn make(parent: &Path, id: &str, worktree: &Worktree, directory: &Path) -> Result<(), String> {
    use crate::canonical::{Durability, sync, sync_directory};
    use std::io::Write;
    let error = |path: &Path, error: std::io::Error| format!("{}: {error}", path.display());
    let staging = parent.join(format!(
        ".creating-{id}-{}-{}",
        std::process::id(),
        crate::canonical::NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    fs::create_dir(&staging).map_err(|e| error(&staging, e))?;
    let staged = (|| -> Result<(), String> {
        let marker = serde_json::to_vec_pretty(&Marker {
            schema: MARKER.into(),
            root: worktree.root.clone(),
            gitdir: worktree.gitdir.clone(),
            incarnation: worktree.incarnation.clone(),
        })
        .map_err(|e| e.to_string())?;
        // Synced: a marker torn by a power cut would read as a gone worktree.
        // Closed before the rename: Windows refuses to rename a directory
        // that holds an open file.
        let path = staging.join("worktree.json");
        {
            let mut file = fs::File::create(&path).map_err(|e| error(&path, e))?;
            file.write_all(&marker)
                .and_then(|()| sync(&file, Durability::Fsync))
                .map_err(|e| error(&path, e))?;
        }
        fs::write(staging.join("lease"), b"").map_err(|e| error(&staging, e))?;
        fs::create_dir(staging.join(HANDED_OUT)).map_err(|e| error(&staging, e))?;
        sync_directory(&staging, Durability::Fsync)?;
        match fs::rename(&staging, directory) {
            Ok(()) => sync_directory(parent, Durability::Fsync),
            // Another run of this worktree made it first.
            Err(_) if directory.is_dir() => Ok(()),
            Err(e) => Err(error(directory, e)),
        }
    })();
    let _ = fs::remove_dir_all(&staging);
    staged
}

/// Whether the worktree incarnation whose directory `directory` is still
/// exists: the git directory its marker names still holds its token, and its
/// top level's `.git` still leads there, or git keeps that worktree locked;
/// and the marker is what the directory is named by.
#[cfg(unix)]
fn exists(directory: &Path, id: &str) -> bool {
    let Some(marker) = fs::read(directory.join("worktree.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Marker>(&bytes).ok())
        .filter(|marker| marker.schema == MARKER)
    else {
        return false;
    };
    let worktree = Worktree {
        root: marker.root,
        gitdir: marker.gitdir,
        incarnation: marker.incarnation,
    };
    // Its git directory holds its token: git has not removed and made it
    // again (as for a worktree made again at the same path).
    let same = current_incarnation(Path::new(&worktree.gitdir))
        .is_some_and(|token| token == worktree.incarnation);
    let reachable = || {
        fs::canonicalize(&worktree.root).is_ok_and(|root| {
            root == Path::new(&worktree.root)
                && gitdir_of(&root).is_some_and(|gitdir| gitdir == Path::new(&worktree.gitdir))
        })
    };
    worktree.id().is_ok_and(|named| named == id)
        && same
        && (Path::new(&worktree.gitdir).join("locked").exists() || reachable())
}

/// Set aside the directory of every worktree of the clone that is gone,
/// never one whose lease is held, to be deleted by `remove_pruned` once the
/// run's work is done. Cheap: a rename per gone worktree, under its lease.
#[cfg(unix)]
pub(crate) fn prune(common: &Path) {
    use crate::journal::Lease;
    let parent = common.join("sykli").join("worktrees");
    let Ok(entries) = fs::read_dir(&parent) else {
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_id(name) || exists(&path, name) {
            continue;
        }
        // Held by no one, or it is not ours to take.
        let Ok(Some(_lease)) = Lease::acquire_existing(&path) else {
            continue;
        };
        // Looked at again under the lease: a worktree made again at the same
        // place, with the same git directory, meanwhile is this one.
        if exists(&path, name) {
            continue;
        }
        let aside = parent.join(format!(
            ".pruning-{name}-{}-{}",
            std::process::id(),
            crate::canonical::NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        // The lease goes with this iteration: once the directory is aside,
        // no run takes it again (`Lease::share` looks at its path).
        let _ = fs::rename(&path, &aside);
    }
}

/// Delete every directory `prune` set aside, in this run or an earlier one
/// that stopped before it got here, and what a maker that died left behind
/// (a minute old). Best effort: what cannot be deleted now is by a later run.
#[cfg(unix)]
pub(crate) fn remove_pruned() {
    let Ok(repository) = crate::repo::repository() else {
        return;
    };
    let Ok(entries) = fs::read_dir(repository.common_dir.join("sykli").join("worktrees")) else {
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let stale = name.starts_with(".creating-")
            && fs::symlink_metadata(&path)
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|at| at.elapsed().ok())
                .is_some_and(|age| age > std::time::Duration::from_secs(60));
        if name.starts_with(".pruning-") || stale {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(env: &[(&str, &str)]) -> Task {
        serde_json::from_value(serde_json::json!({
            "name": "t",
            "run": "true",
            "env": env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<std::collections::BTreeMap<_, _>>(),
        }))
        .unwrap()
    }

    #[test]
    fn only_worktree_cache_is_a_placeholder() {
        for value in [
            "",
            "plain",
            "$HOME/x",
            "a$b$",
            "$$",
            "$ {x}",
            "${worktree_cache}",
            "x${worktree_cache}/y${worktree_cache}",
        ] {
            assert!(check(&task(&[("V", value)])).is_ok(), "{value}");
        }
        for (value, says) in [
            ("${nope}", "unknown placeholder ${nope}"),
            (
                "${ worktree_cache }",
                "unknown placeholder ${ worktree_cache }",
            ),
            ("${}", "unknown placeholder ${}"),
            (
                "x${worktree_cache",
                "unclosed placeholder \"${worktree_cache\"",
            ),
            ("${worktree_cache}${", "unclosed placeholder \"${\""),
            ("$${worktree_cache}", "reserved"),
            ("a$${b}", "reserved"),
        ] {
            let error = check(&task(&[("V", value)])).unwrap_err();
            assert!(error.contains(says), "{value}: {error}");
            assert!(error.contains("task \"t\" env V uses"), "{error}");
        }
        assert!(!named_by(&task(&[("V", "$HOME")])));
        assert!(named_by(&task(&[("V", "${worktree_cache}/x")])));
    }

    #[test]
    fn the_placeholder_is_named_only_as_a_shell_would_name_it() {
        for text in [
            "${worktree_cache}",
            "x $worktree_cache",
            "${worktree_cache:-/x}",
            "\"$worktree_cache\"/y",
        ] {
            assert!(names_placeholder(text), "{text}");
        }
        for text in [
            "worktree_cache",
            "$worktree_cache_dir",
            "${worktree_caches}",
            "$HOME/worktree_cache",
            "",
        ] {
            assert!(!names_placeholder(text), "{text}");
        }
    }

    #[test]
    fn a_worktree_is_named_by_its_incarnation() {
        let id = |root: &str, gitdir: &str, incarnation: &str| {
            Worktree {
                root: root.into(),
                gitdir: gitdir.into(),
                incarnation: incarnation.into(),
            }
            .id()
            .unwrap()
        };
        let one = id("/r", "/c/worktrees/r", "1");
        assert_eq!(one.len(), ID_LENGTH);
        assert!(one.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(one, id("/r", "/c/worktrees/r", "1"));
        assert_ne!(one, id("/s", "/c/worktrees/r", "1"));
        assert_ne!(one, id("/r", "/c/worktrees/s", "1"));
        // Made again at the same path, with the same git directory.
        assert_ne!(one, id("/r", "/c/worktrees/r", "2"));
    }

    #[test]
    fn a_git_directory_keeps_one_incarnation_until_it_goes() {
        let gitdir = std::env::temp_dir().join(format!(
            "sykli-incarnation-{}-{}",
            std::process::id(),
            crate::canonical::NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&gitdir).unwrap();
        assert_eq!(current_incarnation(&gitdir), None);
        let first = incarnation(&gitdir).unwrap();
        assert_eq!(incarnation(&gitdir).unwrap(), first);
        assert_eq!(
            current_incarnation(&gitdir).as_deref(),
            Some(first.as_str())
        );
        fs::remove_dir_all(&gitdir).unwrap();
        fs::create_dir_all(&gitdir).unwrap();
        assert_ne!(incarnation(&gitdir).unwrap(), first);
        fs::remove_dir_all(&gitdir).unwrap();
    }
}
