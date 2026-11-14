//! Explicit inspection and removal of managed build caches, never checkouts.
use super::{
    HANDED_OUT, Worktree, budget, human, is_id, measurement, move_aside, owner, present, recorded,
};
use crate::journal::Lease;
use serde::Serialize;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Serialize)]
pub(crate) struct Report {
    schema: &'static str,
    apply: bool,
    repository: PathBuf,
    budget_bytes: u64,
    total_bytes: Option<u64>,
    reclaimable_bytes: Option<u64>,
    removed_bytes: u64,
    entries: Vec<Entry>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Serialize)]
struct Entry {
    path: PathBuf,
    worktree: Option<String>,
    bytes: Option<u64>,
    action: &'static str,
    reason: String,
}

struct Cache {
    entry: Entry,
    id: String,
    worktree: Worktree,
    at: SystemTime,
    identity: (u64, u64),
    // Held through selection and removal: no task may start using this cache
    // between its measurement and its removal. Preview drops it after reading.
    lease: Option<Lease>,
    pruning: bool,
}

fn metadata(path: &Path) -> Result<fs::Metadata, String> {
    fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn directory(path: &Path) -> Result<fs::Metadata, String> {
    let meta = metadata(path)?;
    if !meta.is_dir() {
        return Err(format!(
            "{} is not a directory (symbolic links are not followed)",
            path.display()
        ));
    }
    Ok(meta)
}

fn inspect(path: &Path, id: &str, pruning: bool, apply: bool) -> Result<Cache, String> {
    let meta = directory(path)?;
    let worktree = owner(path, id)?;
    if !metadata(&path.join("lease"))?.is_file() {
        return Err(format!("{} has no regular lease file", path.display()));
    }
    let lease = Lease::inspect_existing(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut entry = Entry {
        path: path.to_path_buf(),
        worktree: Some(worktree.root.clone()),
        bytes: None,
        action: "keep",
        reason: "in use".into(),
    };
    if lease.is_some() {
        // A task may have removed its cache, but never its marker or lease.
        let contents = path.join(HANDED_OUT);
        entry.bytes = match fs::symlink_metadata(&contents) {
            Ok(meta) if meta.is_dir() => {
                let (bytes, errors) = measurement(&contents);
                if !errors.is_empty() {
                    return Err(errors.join("; "));
                }
                Some(bytes)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(0),
            Err(e) => return Err(format!("{}: {e}", contents.display())),
            Ok(_) => {
                return Err(format!(
                    "{} is not a directory (symbolic links are not followed)",
                    contents.display()
                ));
            }
        };
        entry.reason = if pruning {
            "interrupted removal"
        } else if present(&worktree)? {
            "within budget"
        } else {
            "worktree gone"
        }
        .into();
        if pruning || entry.reason == "worktree gone" {
            entry.action = "remove";
        }
    }
    let at = recorded(path).map(|(_, at)| at).unwrap_or(
        metadata(&path.join("worktree.json"))?
            .modified()
            .map_err(|e| e.to_string())?,
    );
    Ok(Cache {
        entry,
        id: id.into(),
        worktree,
        at,
        identity: (meta.dev(), meta.ino()),
        lease: if apply { lease } else { None },
        pruning,
    })
}

/// The same stale-worktree and oldest-first policy as automatic retention.
/// Sizes are measured now, not taken from a possibly old `size` record.
pub(crate) fn clean(apply: bool) -> Result<Report, String> {
    let repository = crate::repo::repository()?;
    let mut parent = repository.common_dir.clone();
    let mut report = Report {
        schema: "sykli-clean.v1",
        apply,
        repository: repository.root.clone(),
        budget_bytes: budget(),
        total_bytes: Some(0),
        reclaimable_bytes: Some(0),
        removed_bytes: 0,
        entries: Vec::new(),
        warnings: Vec::new(),
    };
    // No create_dir calls on this path, even with --apply on a fresh clone.
    for part in ["sykli", "worktrees"] {
        parent.push(part);
        match fs::symlink_metadata(&parent) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
            _ => {
                directory(&parent)?;
            }
        }
    }
    let paths = fs::read_dir(&parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let mut caches = Vec::new();
    for path in paths {
        let path = match path {
            Ok(path) => path.path(),
            Err(e) => {
                report.warnings.push(e.to_string());
                continue;
            }
        };
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let pruning = name.strip_prefix(".pruning-").and_then(|rest| {
            let mut parts = rest.split('-');
            let id = parts.next()?;
            parts.next()?.parse::<u32>().ok()?;
            parts.next()?.parse::<u64>().ok()?;
            (is_id(id) && parts.next().is_none()).then_some(id)
        });
        let id = if is_id(name) { Some(name) } else { pruning };
        let inspected = id
            .ok_or_else(|| "unrecognized or unfinished cache directory; retained".to_owned())
            .and_then(|id| inspect(&path, id, pruning.is_some(), apply));
        match inspected {
            Ok(cache) => {
                if cache.entry.bytes.is_none() {
                    report
                        .warnings
                        .push(format!("{} is in use; its size is unknown", path.display()));
                }
                caches.push(cache);
            }
            Err(error) => {
                report.warnings.push(format!("{}: {error}", path.display()));
                report.entries.push(Entry {
                    path,
                    worktree: None,
                    bytes: None,
                    action: "keep",
                    reason: error,
                });
            }
        }
    }
    // Unknown sizes cannot make the total look complete. Still collect known
    // orphans, but do not guess how many other caches must go for the budget.
    let complete = report.warnings.is_empty();
    let total = caches
        .iter()
        .try_fold(0u64, |sum, cache| sum.checked_add(cache.entry.bytes?));
    if complete && total.is_none() {
        report
            .warnings
            .push("cache sizes exceed the supported total; coverage is partial".into());
    }
    report.total_bytes = total.filter(|_| complete);
    caches.sort_by(|a, b| (a.at, &a.entry.path).cmp(&(b.at, &b.entry.path)));
    if let Some(mut remaining) = report.total_bytes {
        for cache in &caches {
            if cache.entry.action == "remove" {
                remaining = remaining.saturating_sub(cache.entry.bytes.unwrap_or(0));
            }
        }
        for cache in &mut caches {
            if report.budget_bytes == 0 || remaining <= report.budget_bytes {
                break;
            }
            if cache.entry.action == "keep" {
                cache.entry.action = "remove";
                cache.entry.reason = "over budget".into();
                remaining = remaining.saturating_sub(cache.entry.bytes.unwrap_or(0));
            }
        }
    }
    report.reclaimable_bytes = if report.total_bytes.is_some() {
        caches
            .iter()
            .filter(|c| c.entry.action == "remove")
            .try_fold(0u64, |sum, c| sum.checked_add(c.entry.bytes?))
    } else {
        None
    };
    for mut cache in caches {
        if cache.entry.reason == "within budget" {
            if report.total_bytes.is_none() {
                cache.entry.reason = "budget unknown; retained".into();
            } else if report.budget_bytes == 0 {
                cache.entry.reason = "budget eviction disabled".into();
            }
        }
        if apply && cache.entry.action == "remove" {
            match remove(&mut cache, &parent) {
                Ok(()) => {
                    cache.entry.action = "removed";
                    report.removed_bytes = report
                        .removed_bytes
                        .saturating_add(cache.entry.bytes.unwrap_or(0));
                }
                Err(e) => {
                    cache.entry.action = "keep";
                    cache.entry.reason = e.clone();
                    report
                        .warnings
                        .push(format!("{}: {e}", cache.entry.path.display()));
                }
            }
        }
        report.entries.push(cache.entry);
    }
    report.entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(report)
}

fn remove(cache: &mut Cache, parent: &Path) -> Result<(), String> {
    let lease = cache
        .lease
        .as_ref()
        .ok_or("cache is not exclusively held")?;
    crate::checkpoint("clean-before-remove")?;
    let meta = directory(&cache.entry.path)?;
    if (meta.dev(), meta.ino()) != cache.identity {
        return Err("cache changed during cleanup; retained".into());
    }
    let now = owner(&cache.entry.path, &cache.id)?;
    if now.root != cache.worktree.root
        || now.gitdir != cache.worktree.gitdir
        || now.incarnation != cache.worktree.incarnation
    {
        return Err("cache owner changed during cleanup; retained".into());
    }
    if cache.entry.reason == "worktree gone" && present(&now)? {
        return Err("worktree returned during cleanup; retained".into());
    }
    lease.check_existing(&cache.entry.path)?;
    let path = if cache.pruning {
        cache.entry.path.clone()
    } else {
        move_aside(parent, &cache.id, &cache.entry.path)?
    };
    crate::checkpoint("clean-after-rename").map_err(|e| format!("{}: {e}", path.display()))?;
    fs::remove_dir_all(&path).map_err(|e| format!("could not remove {}: {e}", path.display()))
}

impl Report {
    pub(crate) fn describe(&self) -> String {
        let size = |bytes: Option<u64>| {
            bytes
                .map(human)
                .unwrap_or_else(|| "unknown (partial coverage)".into())
        };
        let mut out = format!(
            "Managed build caches for {}\nTotal: {}; eligible: {}\n",
            self.repository.display(),
            size(self.total_bytes),
            size(self.reclaimable_bytes)
        );
        out.push_str(&format!(
            "Retention limit: {}\n",
            if self.budget_bytes == 0 {
                "disabled".into()
            } else {
                human(self.budget_bytes)
            }
        ));
        for entry in &self.entries {
            let action = if entry.action == "remove" {
                "would remove"
            } else {
                entry.action
            };
            out.push_str(&format!(
                "{action}: {} — {} — {}{}\n",
                entry.path.display(),
                size(entry.bytes),
                entry.reason,
                entry
                    .worktree
                    .as_ref()
                    .map(|root| format!(" (worktree {root})"))
                    .unwrap_or_default()
            ));
        }
        if self.apply {
            out.push_str(&format!(
                "Removed an estimated {} of build caches.\n",
                human(self.removed_bytes)
            ));
        } else {
            out.push_str(
                "Preview only; run sykli clean --apply to recheck and remove eligible caches.\n",
            );
        }
        out
    }
}
