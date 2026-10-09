//! Git hook entrypoints. They check the tree a commit actually carries, never
//! merely the files on disk, and they only refuse: nothing is staged, stashed
//! or rewritten on the operator's behalf.

use crate::{commit_tree_oid, git, load, refusal, resolve_contract, run, verify};
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
            "changes to {} are not staged, so a run would check content the commit does not carry; \
             stage them (git add) or set them aside, then commit again",
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

/// `sykli hook pre-commit`: the graph must pass on exactly the staged tree.
pub(crate) fn pre_commit() -> ExitCode {
    const HOOK: &str = "pre-commit";
    let root = match enter_repository(HOOK) {
        Ok(root) => root,
        Err(code) => return code,
    };
    let Some(contract) = root_contract() else {
        eprintln!(
            "sykli hook {HOOK}: no sykli.json or sykli.rs in {}; nothing to check",
            root.display()
        );
        return ExitCode::SUCCESS;
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
    let receipt = match load(&contract)
        .and_then(|(loaded, levels, hash)| run(&loaded, &levels, hash, false))
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

/// The remote's default branch as `refs/remotes/REMOTE/HEAD` names it, and
/// `true`; else `main` and `master`, and `false`.
fn default_branches(remote: Option<&str>) -> (Vec<String>, bool) {
    remote
        .and_then(|remote| {
            git(&[
                "symbolic-ref",
                "--quiet",
                &format!("refs/remotes/{remote}/HEAD"),
            ])
            .ok()?
            .strip_prefix(&format!("refs/remotes/{remote}/"))
            .map(|branch| (vec![format!("refs/heads/{branch}")], true))
        })
        .unwrap_or_else(|| {
            (
                vec!["refs/heads/main".into(), "refs/heads/master".into()],
                false,
            )
        })
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

/// A receipt in this worktree that passes `verify --commit COMMIT`, or why
/// there is none.
fn receipt_for(contract: &Path, commit: &str) -> Result<PathBuf, String> {
    let tree = commit_tree_oid(Path::new("."), commit)?;
    let candidates = receipts_of(&tree);
    let mut newest_refusal = None;
    for receipt in candidates {
        let refused = match verify(&receipt, contract, Some(commit)) {
            Ok(checks) => match checks.iter().find(|check| !check.ok) {
                None => return Ok(receipt),
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
pub(crate) fn pre_push(remote: Option<&str>) -> ExitCode {
    const HOOK: &str = "pre-push";
    if let Err(code) = enter_repository(HOOK) {
        return code;
    }
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
    let (defaults, known) = default_branches(remote);
    let pushed = updates
        .iter()
        .filter(|update| !absent(&update.local_sha))
        .collect::<Vec<_>>();
    if !known
        && pushed.iter().any(|update| {
            update.remote_ref.starts_with("refs/heads/") && !defaults.contains(&update.remote_ref)
        })
    {
        let remote = remote.unwrap_or("REMOTE");
        eprintln!(
            "sykli hook {HOOK}: {remote}'s default branch is unknown here, so only main and master are gated; name it with: git remote set-head {remote} --auto"
        );
    }
    for update in pushed
        .into_iter()
        .filter(|update| defaults.contains(&update.remote_ref))
    {
        let short = &update.local_sha[..update.local_sha.len().min(12)];
        let Some(contract) = contract_in(&update.local_sha) else {
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
            Ok(receipt) => eprintln!(
                "sykli hook {HOOK}: {short} to {}: {} is a clean, passing run of it",
                update.remote_ref,
                receipt.display()
            ),
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
    ExitCode::SUCCESS
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
        let state = match std::fs::read(&path) {
            Err(_) => "installed",
            Ok(existing) if existing == wanted.as_bytes() => "unchanged",
            Ok(existing) if String::from_utf8_lossy(&existing).contains(MARKER) => "updated",
            Ok(_) if force => "replaced",
            Ok(_) => {
                eprintln!(
                    "sykli hook install: refusing: {} exists and sykli did not write it; pass --force to replace it",
                    path.display()
                );
                return ExitCode::from(REFUSED);
            }
        };
        plan.push((path, wanted, state));
    }
    for (path, wanted, state) in plan {
        let written = if state == "unchanged" {
            Ok(())
        } else {
            std::fs::create_dir_all(&directory).and_then(|_| std::fs::write(&path, wanted))
        };
        if let Err(error) = written.and_then(|_| executable(&path)) {
            eprintln!(
                "sykli hook install: cannot write {}: {error}",
                path.display()
            );
            return ExitCode::from(CANNOT_EVALUATE);
        }
        println!("{state}: {}", path.display());
    }
    ExitCode::SUCCESS
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
