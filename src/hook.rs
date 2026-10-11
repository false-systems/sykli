//! Git hook entrypoints. They check the tree a commit actually carries, never
//! merely the files on disk, and they only refuse: nothing is staged, stashed
//! or rewritten on the operator's behalf.

use crate::{commit_tree_oid, git, load_pinned, refusal, resolve_contract, run, verify};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode};

const REFUSED: u8 = 1;
const CANNOT_EVALUATE: u8 = 2;

/// Git runs hooks from the repository root; make sure of it, since the
/// contract and receipts are looked for there.
fn enter_repository(hook: &str) -> Result<PathBuf, ExitCode> {
    let root = git(&["rev-parse", "--show-toplevel"]).map_err(|_| {
        eprintln!("sykli hook {hook}: not inside a git repository");
        ExitCode::from(CANNOT_EVALUATE)
    })?;
    std::env::set_current_dir(&root).map_err(|error| {
        eprintln!("sykli hook {hook}: cannot enter {root}: {error}");
        ExitCode::from(CANNOT_EVALUATE)
    })?;
    Ok(PathBuf::from(root))
}

/// The root's contract, as `run` would pick it: `sykli.rs`, else `sykli.json`.
fn root_contract() -> Option<PathBuf> {
    let contract = resolve_contract(PathBuf::from("sykli.rs"));
    contract.exists().then_some(contract)
}

/// Paths a git listing prints with `-z`, for messages.
fn git_names(args: &[&str]) -> Result<Vec<String>, String> {
    let output = ProcessCommand::new("git")
        .args(args)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().into());
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect())
}

fn listing(names: &[String]) -> String {
    const SHOWN: usize = 5;
    let mut shown = names.iter().take(SHOWN).cloned().collect::<Vec<_>>();
    if names.len() > SHOWN {
        shown.push(format!("and {} more", names.len() - SHOWN));
    }
    shown.join(", ")
}

/// Why a run now would not check the tree being committed, if it would not.
/// A run hashes every non-ignored file on disk; the commit carries the index.
/// Respects `GIT_INDEX_FILE`, so `commit -a` and `commit PATH` compare against
/// the index git is about to commit.
fn disk_differs_from_commit() -> Result<Option<String>, String> {
    let unstaged = git_names(&["diff", "--name-only", "-z"])?;
    if !unstaged.is_empty() {
        return Ok(Some(format!(
            "what is on disk for {} differs from what this commit carries (unstaged, or left out \
             of a partial commit), so a run would check other content; stage it (git add) or set \
             it aside, then commit again",
            listing(&unstaged)
        )));
    }
    let untracked = git_names(&["ls-files", "--others", "--exclude-standard", "-z"])?
        .into_iter()
        .filter(|name| !name.starts_with(".sykli/"))
        .collect::<Vec<_>>();
    if !untracked.is_empty() {
        return Ok(Some(format!(
            "{} untracked, so a run would include files the commit does not carry; \
             add, ignore or remove them, then commit again",
            listing(&untracked)
        )));
    }
    Ok(None)
}

/// A contract named with `--contract` (already absolute), which must exist:
/// a gate that was asked for and is missing cannot pass for one.
fn named_contract(hook: &str, contract: &Path) -> Result<PathBuf, ExitCode> {
    if contract.is_file() {
        return Ok(contract.to_path_buf());
    }
    eprintln!(
        "sykli hook {hook}: cannot evaluate: the contract {} does not exist",
        contract.display()
    );
    Err(ExitCode::from(CANNOT_EVALUATE))
}

/// `sykli hook pre-commit`: the graph must pass on exactly the staged tree.
/// `contract`, when given, is run instead of the repository's own, such as
/// a contract kept outside the repository.
pub(crate) fn pre_commit(contract: Option<&Path>) -> ExitCode {
    const HOOK: &str = "pre-commit";
    let root = match enter_repository(HOOK) {
        Ok(root) => root,
        Err(code) => return code,
    };
    let contract = match contract.map(|contract| named_contract(HOOK, contract)) {
        Some(Ok(contract)) => contract,
        Some(Err(code)) => return code,
        None => match root_contract() {
            Some(contract) => contract,
            None => {
                eprintln!(
                    "sykli hook {HOOK}: no sykli.json or sykli.rs in {}; nothing to check",
                    root.display()
                );
                return ExitCode::SUCCESS;
            }
        },
    };
    if git(&["rev-parse", "--verify", "--quiet", "HEAD"]).is_err() {
        eprintln!(
            "sykli hook {HOOK}: no commit yet, and a receipt binds to one; the first commit is not checked"
        );
        return ExitCode::SUCCESS;
    }
    let refuse = |reason: String| {
        eprintln!("sykli hook {HOOK}: refusing the commit: {reason}");
        ExitCode::from(REFUSED)
    };
    let cannot = |error: String| {
        eprintln!("sykli hook {HOOK}: cannot check the commit: {error}");
        ExitCode::from(CANNOT_EVALUATE)
    };
    match disk_differs_from_commit() {
        Ok(None) => {}
        Ok(Some(reason)) => return refuse(reason),
        Err(error) => return cannot(error),
    }
    let receipt = match load_pinned(&contract)
        .and_then(|((loaded, levels, hash), lock)| run(&loaded, &levels, hash, lock, false))
    {
        Ok((true, receipt)) => receipt,
        Ok((false, receipt)) => {
            return refuse(format!(
                "the graph failed on the staged tree; see {}",
                receipt.display()
            ));
        }
        Err(error) => return cannot(error),
    };
    match verify(&receipt, &contract, None) {
        Ok(checks) => {
            if let Some(failed) = checks.iter().find(|check| !check.ok) {
                return refuse(format!(
                    "the run's receipt does not verify: {}",
                    refusal(failed)
                ));
            }
        }
        Err(error) => return cannot(error),
    }
    // The checks above are early, readable refusals; this is the guarantee.
    // `git diff` is silent for assume-unchanged and skip-worktree files, so
    // compare the tree that ran with the tree the index will commit.
    let ran = std::fs::read(&receipt)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|receipt| receipt["subject"]["tree_oid"].as_str().map(String::from));
    let committed = git(&["write-tree"]).and_then(|tree| commit_tree_oid(Path::new("."), &tree));
    match (ran, committed) {
        (Some(ran), Ok(committed)) if ran == committed => {
            eprintln!("sykli hook {HOOK}: the staged tree passed");
            ExitCode::SUCCESS
        }
        (Some(ran), Ok(committed)) => refuse(format!(
            "the run's tree {ran} is not the tree being committed {committed}; a file marked \
             assume-unchanged or skip-worktree differs from what is staged (`git ls-files -v` marks them)"
        )),
        (None, _) => cannot(format!("cannot read the tree from {}", receipt.display())),
        (_, Err(error)) => cannot(error),
    }
}

/// One line of git's pre-push input.
struct Update {
    local_sha: String,
    remote_ref: String,
    remote_sha: String,
}

fn updates(input: &str) -> Result<Vec<Update>, String> {
    input
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(
            |line| match line.split_whitespace().collect::<Vec<_>>()[..] {
                [_, local_sha, remote_ref, remote_sha] => Ok(Update {
                    local_sha: local_sha.into(),
                    remote_ref: remote_ref.into(),
                    remote_sha: remote_sha.into(),
                }),
                _ => Err(format!("unexpected pre-push input line {line:?}")),
            },
        )
        .collect()
}

/// An all-zero object id: git's "no such ref" in pre-push lines.
fn absent(sha: &str) -> bool {
    sha.bytes().all(|byte| byte == b'0')
}

/// How the gated branches were chosen.
enum Chosen {
    /// Named with `--branch`, or the default branch `refs/remotes/REMOTE/HEAD` names.
    Known,
    /// No `refs/remotes/REMOTE/HEAD`: `main` and `master` are gated.
    Unset,
    /// `refs/remotes/REMOTE/HEAD` names this ref, which does not exist (the
    /// default branch was renamed or deleted, then pruned): gating it would
    /// gate nothing, so `main` and `master` are gated instead.
    Stale(String),
}

/// The remote's default branch as `refs/remotes/REMOTE/HEAD` names it, when
/// the ref it names exists; else `main` and `master`.
fn default_branches(remote: Option<&str>) -> (Vec<String>, Chosen) {
    let fallback = || vec!["refs/heads/main".into(), "refs/heads/master".into()];
    let Some(remote) = remote else {
        return (fallback(), Chosen::Unset);
    };
    let Ok(target) = git(&[
        "symbolic-ref",
        "--quiet",
        &format!("refs/remotes/{remote}/HEAD"),
    ]) else {
        return (fallback(), Chosen::Unset);
    };
    let Some(branch) = target.strip_prefix(&format!("refs/remotes/{remote}/")) else {
        return (fallback(), Chosen::Unset);
    };
    if git(&["rev-parse", "--verify", "--quiet", &target]).is_err() {
        return (fallback(), Chosen::Stale(target));
    }
    (vec![format!("refs/heads/{branch}")], Chosen::Known)
}

/// The refs `--branch` names: exactly `refs/heads/BRANCH` for each. A value
/// that is not a branch name would gate nothing, so it is an error instead.
fn named_branches(branches: &[String]) -> Result<Vec<String>, String> {
    branches
        .iter()
        .map(|branch| {
            let name = format!("refs/heads/{branch}");
            if branch.starts_with("refs/") || git(&["check-ref-format", &name]).is_err() {
                return Err(format!(
                    "--branch {branch:?} is not a branch name; name it as `git push REMOTE NAME` \
                     would (main, not refs/heads/main)"
                ));
            }
            Ok(name)
        })
        .collect()
}

/// The root contract a commit carries, as the hook gates on it: `sykli.json`,
/// else `sykli.rs` (which `verify --commit` cannot read, so it refuses).
fn contract_in(commit: &str) -> Option<PathBuf> {
    ["sykli.json", "sykli.rs"]
        .into_iter()
        .find(|name| git(&["cat-file", "-e", &format!("{commit}:{name}")]).is_ok())
        .map(PathBuf::from)
}

/// What the remote's branch points at before this push, as far as this
/// repository knows: the line's remote sha when its commit is here, else the
/// remote-tracking branch.
fn remote_tip(update: &Update, remote: Option<&str>) -> Option<String> {
    if !absent(&update.remote_sha)
        && git(&[
            "cat-file",
            "-e",
            &format!("{}^{{commit}}", update.remote_sha),
        ])
        .is_ok()
    {
        return Some(update.remote_sha.clone());
    }
    let branch = update.remote_ref.strip_prefix("refs/heads/")?;
    git(&[
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("refs/remotes/{}/{branch}", remote?),
    ])
    .ok()
}

/// Receipts this worktree holds whose tree is `tree`, newest first: the
/// documented gate's `.sykli/receipt.json` and every receipt `run` wrote.
fn receipts_of(tree: &str) -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from(".sykli/receipt.json")];
    if let Ok(entries) = std::fs::read_dir(".sykli/receipts") {
        paths.extend(
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "json")),
        );
    }
    let mut found = paths
        .into_iter()
        .filter_map(|path| {
            let receipt: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
            (receipt["subject"]["tree_oid"].as_str() == Some(tree))
                .then(|| (receipt["started_at_ms"].as_u64().unwrap_or(0), path))
        })
        .collect::<Vec<_>>();
    found.sort_by_key(|(started, _)| std::cmp::Reverse(*started));
    found.into_iter().map(|(_, path)| path).collect()
}

/// A receipt in this worktree that passes `verify --commit COMMIT`, with the
/// bytes that verified (read before and after, and the same), or why there is
/// none. Kept evidence is those bytes, never a second read of the path.
fn receipt_for(contract: &Path, commit: &str) -> Result<(PathBuf, Vec<u8>), String> {
    let tree = commit_tree_oid(Path::new("."), commit)?;
    let candidates = receipts_of(&tree);
    let mut newest_refusal = None;
    for receipt in candidates {
        let before = std::fs::read(&receipt).ok();
        let refused = match verify(&receipt, contract, Some(commit)) {
            Ok(checks) => match checks.iter().find(|check| !check.ok) {
                None => match before {
                    Some(bytes) if std::fs::read(&receipt).ok().as_ref() == Some(&bytes) => {
                        return Ok((receipt, bytes));
                    }
                    _ => "the receipt changed while it was verified".into(),
                },
                Some(failed) => refusal(failed),
            },
            Err(error) => error,
        };
        newest_refusal.get_or_insert(format!("{}: {refused}", receipt.display()));
    }
    Err(match newest_refusal {
        Some(refused) => format!("no receipt of its tree verifies; the newest says {refused}"),
        None => "no receipt in this worktree is a run of its tree".into(),
    })
}

/// `sykli hook pre-push`: a push that moves the default branch needs a clean,
/// passing receipt of exactly the commit it pushes. Whether a push is gated is
/// decided from the commits, never from the working tree: the pushed commit's
/// contract gates it, and a commit that drops the contract the branch had is
/// refused, so taking the gate off takes an explicit `--no-verify`.
///
/// With `contract`, that contract gates every push to a gated branch,
/// whatever the commits carry: a person's own gate, kept outside the
/// repository, is the same for every commit, and `verify --commit` reads it
/// from its file.
///
/// With `branches`, the gated branches are exactly those: whoever installs
/// the hook knows which branch they protect, while `refs/remotes/REMOTE/HEAD`
/// is a guess that a rename, an unset HEAD or a protected non-default branch
/// gets wrong.
pub(crate) fn pre_push(
    remote: Option<&str>,
    contract: Option<&Path>,
    branches: &[String],
) -> ExitCode {
    const HOOK: &str = "pre-push";
    if let Err(code) = enter_repository(HOOK) {
        return code;
    }
    let gate = match contract.map(|contract| named_contract(HOOK, contract)) {
        Some(Ok(contract)) => Some(contract),
        Some(Err(code)) => return code,
        None => None,
    };
    let named = match named_branches(branches) {
        Ok(named) => named,
        Err(error) => {
            eprintln!("sykli hook {HOOK}: cannot evaluate: {error}");
            return ExitCode::from(CANNOT_EVALUATE);
        }
    };
    let mut input = String::new();
    let parsed = std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| error.to_string())
        .and_then(|_| updates(&input));
    let updates = match parsed {
        Ok(updates) => updates,
        Err(error) => {
            eprintln!("sykli hook {HOOK}: cannot read what is pushed: {error}");
            return ExitCode::from(CANNOT_EVALUATE);
        }
    };
    let (defaults, chosen) = if named.is_empty() {
        default_branches(remote)
    } else {
        (named, Chosen::Known)
    };
    let pushed = updates
        .iter()
        .filter(|update| !absent(&update.local_sha))
        .collect::<Vec<_>>();
    let named_remote = remote.unwrap_or("REMOTE");
    match &chosen {
        Chosen::Known => {}
        // Always said: what is gated changed without anyone choosing it.
        Chosen::Stale(target) => eprintln!(
            "sykli hook {HOOK}: refs/remotes/{named_remote}/HEAD names {target}, which does not exist \
             (the default branch was renamed or removed), so only main and master are gated; \
             name the default branch with: git remote set-head {named_remote} --auto, \
             or name the gated branches with --branch"
        ),
        Chosen::Unset => {
            if pushed.iter().any(|update| {
                update.remote_ref.starts_with("refs/heads/")
                    && !defaults.contains(&update.remote_ref)
            }) {
                eprintln!(
                    "sykli hook {HOOK}: {named_remote}'s default branch is unknown here, so only main and master are gated; \
                     name it with: git remote set-head {named_remote} --auto, or name the gated branches with --branch"
                );
            }
        }
    }
    let mut accepted = Vec::new();
    for update in pushed
        .into_iter()
        .filter(|update| defaults.contains(&update.remote_ref))
    {
        let short = &update.local_sha[..update.local_sha.len().min(12)];
        let Some(contract) = gate.clone().or_else(|| contract_in(&update.local_sha)) else {
            let before = remote_tip(update, remote);
            if let Some(tip) = before.filter(|tip| contract_in(tip).is_some()) {
                eprintln!(
                    "sykli hook {HOOK}: refusing to push {short} to {}: it has no sykli.json or sykli.rs, but {} there does; \
                     this push would take the gate off. To do that on purpose, push with --no-verify",
                    update.remote_ref,
                    &tip[..tip.len().min(12)]
                );
                return ExitCode::from(REFUSED);
            }
            eprintln!(
                "sykli hook {HOOK}: {short} has no sykli.json or sykli.rs, nor did {} before; not gated",
                update.remote_ref
            );
            continue;
        };
        match receipt_for(&contract, &update.local_sha) {
            Ok((receipt, bytes)) => {
                eprintln!(
                    "sykli hook {HOOK}: {short} to {}: {} is a passing run of its tree",
                    update.remote_ref,
                    receipt.display()
                );
                accepted.push((short.to_string(), bytes));
            }
            Err(reason) => {
                eprintln!(
                    "sykli hook {HOOK}: refusing to push {short} to {}: {reason}",
                    update.remote_ref
                );
                eprintln!(
                    "sykli hook {HOOK}: to produce a receipt, check out {short} with nothing uncommitted and run: sykli run {}",
                    contract.display()
                );
                return ExitCode::from(REFUSED);
            }
        }
    }
    // Only once every ref is accepted: a refused push keeps nothing. Keeping
    // evidence is a convenience, never a reason to stop the push.
    // The hook runs before the remote answers, and `--dry-run` reaches it
    // indistinguishably, so this is evidence for the commit, not proof that
    // the commit reached the remote.
    for (short, bytes) in accepted {
        match keep_evidence(&bytes) {
            Ok(kept) => eprintln!(
                "sykli hook {HOOK}: kept {} as evidence for {short}",
                kept.display()
            ),
            Err(error) => eprintln!(
                "sykli hook {HOOK}: warning: the receipt was not kept as evidence ({error}); the push goes ahead"
            ),
        }
    }
    ExitCode::SUCCESS
}

/// Where accepted pushes' receipts are kept: `sykli/receipts/rcpt_<sha256 of
/// the receipt's bytes>.json` in the common git directory, the scheme of
/// `.sykli/receipts/`. A worktree's `.sykli/` goes when the worktree is
/// removed; this outlives it, and `verify --commit` reads nothing else.
///
/// Bounded like `.sykli/receipts/`: past the receipt budget
/// (`SYKLI_RECEIPT_BUDGET`, default 200, `0` keeps all) the oldest kept
/// receipts are removed, and so are temporary files an interrupted copy left.
/// Keeping a receipt again makes it the newest. Nothing is written through a
/// symbolic link. The directory confers no trust: whoever reads a receipt
/// from it must run `verify --commit` on it.
fn keep_evidence(bytes: &[u8]) -> Result<PathBuf, String> {
    let error = |what: &Path, error: std::io::Error| format!("{}: {error}", what.display());
    let mut directory = PathBuf::from(git(&[
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
    ])?);
    for part in ["sykli", "receipts"] {
        directory.push(part);
        ensure_directory(&directory, |path| std::fs::create_dir(path))?;
    }
    let hash = crate::sha256(bytes);
    let kept = directory.join(format!("rcpt_{hash}.json"));
    match std::fs::symlink_metadata(&kept) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(format!(
                "{} is a symbolic link; not writing through it",
                kept.display()
            ));
        }
        // Named by its content, so a file of that name already holds it. It
        // becomes the newest, so eviction cannot take the evidence for a
        // commit that was just pushed again.
        Ok(_) => {
            std::fs::File::options()
                .write(true)
                .open(&kept)
                .and_then(|file| file.set_modified(std::time::SystemTime::now()))
                .map_err(|e| error(&kept, e))?;
            prune_evidence(&directory, &kept);
            return Ok(kept);
        }
        Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => {}
        Err(other) => return Err(error(&kept, other)),
    }
    let temporary = directory.join(format!(".rcpt_{hash}.json.tmp-{}", std::process::id()));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .and_then(|mut file| std::io::Write::write_all(&mut file, bytes))
        .and_then(|_| std::fs::rename(&temporary, &kept));
    if let Err(failed) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(error(&kept, failed));
    }
    prune_evidence(&directory, &kept);
    Ok(kept)
}

/// A directory at `path`, never a symbolic link. Another push may create it
/// between the look and `create`; losing that race is fine, so the look is
/// repeated rather than the push's evidence dropped.
fn ensure_directory(
    path: &Path,
    create: impl Fn(&Path) -> std::io::Result<()>,
) -> Result<(), String> {
    for _ in 0..2 {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!(
                    "{} is a symbolic link; not writing through it",
                    path.display()
                ));
            }
            Ok(meta) if meta.is_dir() => return Ok(()),
            Ok(_) => return Err(format!("{} is not a directory", path.display())),
            Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => match create(path) {
                Ok(()) => return Ok(()),
                Err(raced) if raced.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(failed) => return Err(format!("{}: {failed}", path.display())),
            },
            Err(other) => return Err(format!("{}: {other}", path.display())),
        }
    }
    Err(format!("{} keeps changing; not writing", path.display()))
}

/// How old a temporary copy must be before pruning takes it for one an
/// interrupted push left behind, rather than one a push is writing now.
const STALE_TEMPORARY: std::time::Duration = std::time::Duration::from_secs(600);

/// Drop the oldest kept receipts past the budget, never the one just kept,
/// and temporary copies older than STALE_TEMPORARY.
fn prune_evidence(directory: &Path, keep: &Path) {
    let budget = crate::receipt_budget();
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let now = std::time::SystemTime::now();
    let mut kept = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(modified) = entry.metadata().ok().and_then(|meta| meta.modified().ok()) else {
            continue;
        };
        if name.starts_with(".rcpt_") && name.contains(".json.tmp-") {
            if now
                .duration_since(modified)
                .is_ok_and(|age| age > STALE_TEMPORARY)
            {
                let _ = std::fs::remove_file(entry.path());
            }
        } else if name.starts_with("rcpt_") && name.ends_with(".json") {
            kept.push((modified, entry.path()));
        }
    }
    if budget == 0 || kept.len() <= budget {
        return;
    }
    kept.sort();
    let excess = kept.len() - budget;
    for (_, path) in kept
        .into_iter()
        .filter(|(_, path)| path != keep)
        .take(excess)
    {
        let _ = std::fs::remove_file(path);
    }
}

/// What `install` writes: the check lives in sykli, the hook only calls it.
/// The marker line is how a later install recognises its own file.
const MARKER: &str = "# Installed by `sykli hook install`; the check lives in sykli.";

fn script(name: &str) -> String {
    format!("#!/bin/sh\n{MARKER}\nexec sykli hook {name} \"$@\"\n")
}

/// `sykli hook install`: thin hooks in the common git directory, so every
/// worktree of the repository shares them.
pub(crate) fn install(pre_commit: bool, pre_push: bool, force: bool) -> ExitCode {
    let names = match (pre_commit, pre_push) {
        (false, false) => vec!["pre-commit", "pre-push"],
        _ => [("pre-commit", pre_commit), ("pre-push", pre_push)]
            .into_iter()
            .filter_map(|(name, chosen)| chosen.then_some(name))
            .collect(),
    };
    let located =
        git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]).and_then(|common| {
            let active = git(&["rev-parse", "--path-format=absolute", "--git-path", "hooks"])?;
            Ok((PathBuf::from(common).join("hooks"), PathBuf::from(active)))
        });
    let (directory, active) = match located {
        Ok(found) => found,
        Err(_) => {
            eprintln!("sykli hook install: not inside a git repository");
            return ExitCode::from(CANNOT_EVALUATE);
        }
    };
    if active != directory {
        eprintln!(
            "sykli hook install: refusing: core.hooksPath sends git to {}, so hooks in {} would never run; \
             unset it, or call `sykli hook NAME \"$@\"` from the hooks there",
            active.display(),
            directory.display()
        );
        return ExitCode::from(REFUSED);
    }
    // Decide every hook before writing any, so a refusal changes nothing.
    let mut plan = Vec::new();
    for name in names {
        let path = directory.join(name);
        let wanted = script(name);
        // Only sykli's exact script is its own: a file carrying the marker
        // with other lines has been edited, and is someone's work too.
        let state = match std::fs::read(&path) {
            Err(_) => "installed",
            Ok(existing) if existing == wanted.as_bytes() => "unchanged",
            Ok(_) if force => "replaced",
            Ok(existing) => {
                let why = if String::from_utf8_lossy(&existing).contains(MARKER) {
                    "is a sykli hook that has been edited"
                } else {
                    "exists and sykli did not write it"
                };
                eprintln!(
                    "sykli hook install: refusing: {} {why}; pass --force to replace it (it is kept beside the new one)",
                    path.display()
                );
                return ExitCode::from(REFUSED);
            }
        };
        plan.push((path, wanted, state));
    }
    for (path, wanted, state) in plan {
        let kept = (state == "replaced").then(|| kept_aside(&path));
        let written = match (state, &kept) {
            ("unchanged", _) => Ok(()),
            (_, Some(kept)) => {
                std::fs::rename(&path, kept).and_then(|_| std::fs::write(&path, wanted))
            }
            _ => std::fs::create_dir_all(&directory).and_then(|_| std::fs::write(&path, wanted)),
        };
        if let Err(error) = written.and_then(|_| executable(&path)) {
            eprintln!(
                "sykli hook install: cannot write {}: {error}",
                path.display()
            );
            return ExitCode::from(CANNOT_EVALUATE);
        }
        match kept {
            Some(kept) => println!(
                "{state}: {} (the previous hook is kept at {})",
                path.display(),
                kept.display()
            ),
            None => println!("{state}: {}", path.display()),
        }
    }
    ExitCode::SUCCESS
}

/// Where a replaced hook is kept: `NAME.pre-sykli`, or with `.N` added when
/// that is taken, so an earlier copy is never overwritten.
fn kept_aside(path: &Path) -> PathBuf {
    let base = format!("{}.pre-sykli", path.display());
    std::iter::once(PathBuf::from(&base))
        .chain((1..).map(|n| PathBuf::from(format!("{base}.{n}"))))
        .find(|candidate| !candidate.exists())
        .expect("an unbounded sequence has a free name")
}

#[cfg(unix)]
fn executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

/// Git for Windows runs hooks through its own `sh` without an executable bit.
#[cfg(not(unix))]
fn executable(_: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The losing side of two first pushes: the directory is missing when
    /// looked at and present when created. Losing that race is fine.
    #[test]
    fn a_directory_created_by_a_concurrent_push_is_used_not_refused() {
        let root = std::env::temp_dir().join(format!(
            "sykli-hook-race-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let directory = root.join("sykli");
        let other_push_first = |path: &Path| {
            std::fs::create_dir(path).unwrap();
            std::fs::create_dir(path)
        };
        assert_eq!(ensure_directory(&directory, other_push_first), Ok(()));
        assert!(directory.is_dir());
        // Still never through a symbolic link, however it appeared.
        let linked = root.join("linked");
        let other_links = |path: &Path| {
            std::os::unix::fs::symlink(&root, path).unwrap();
            std::fs::create_dir(path)
        };
        assert!(ensure_directory(&linked, other_links).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
