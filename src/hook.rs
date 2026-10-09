//! Git hook entrypoints. They check the tree a commit actually carries, never
//! merely the files on disk, and they only refuse: nothing is staged, stashed
//! or rewritten on the operator's behalf.

use crate::{git, load, refusal, resolve_contract, run, verify};
use std::path::PathBuf;
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
        Ok(checks) => match checks.iter().find(|check| !check.ok) {
            Some(failed) => refuse(format!(
                "the run's receipt does not verify: {}",
                refusal(failed)
            )),
            None => {
                eprintln!("sykli hook {HOOK}: the staged tree passed");
                ExitCode::SUCCESS
            }
        },
        Err(error) => cannot(error),
    }
}
