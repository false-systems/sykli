//! Git hook entrypoints. They check the tree a commit actually carries, never
//! merely the files on disk, and they only refuse: nothing is staged, stashed
//! or rewritten on the operator's behalf.

use crate::{
    commit_tree_oid, git, judge, load_pinned, refusal, repo, resolve_contract, run, verify,
};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const REFUSED: u8 = 1;
const CANNOT_EVALUATE: u8 = 2;

/// Git runs hooks from the repository root; make sure of it, since the
/// contract and receipts are looked for there.
fn enter_repository(hook: &str) -> Result<&'static repo::Repository, ExitCode> {
    let repository = repo::repository().map_err(|_| {
        eprintln!("sykli hook {hook}: not inside a git repository");
        ExitCode::from(CANNOT_EVALUATE)
    })?;
    std::env::set_current_dir(&repository.root).map_err(|error| {
        eprintln!(
            "sykli hook {hook}: cannot enter {}: {error}",
            repository.root.display()
        );
        ExitCode::from(CANNOT_EVALUATE)
    })?;
    Ok(repository)
}

/// The root's contract, as `run` would pick it: `sykli.rs`, else `sykli.json`.
fn root_contract() -> Option<PathBuf> {
    let contract = resolve_contract(PathBuf::from("sykli.rs"));
    contract.exists().then_some(contract)
}

/// Paths a git listing prints with `-z`, for messages.
fn git_names(args: &[&str]) -> Result<Vec<String>, String> {
    let output = repo::git_command()
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
                    root.root.display()
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

/// What `refs/remotes/REMOTE/HEAD` names. It is read where git's files
/// backend keeps it, a loose file in the common directory (a symbolic ref is
/// never packed), and parsed as git parses it: `ref:`, any whitespace, the
/// target. No git runs for that. Git is asked whenever the file cannot be
/// read that way: refs kept another way (reftable), a symbolic link, a file
/// that does not parse, or a remote name that is not one by git's rules (so
/// no path is ever built from it).
fn remote_head(repository: &repo::Repository, remote: &str) -> Option<String> {
    let name = format!("refs/remotes/{remote}/HEAD");
    let ask_git = || git(&["symbolic-ref", "--quiet", &name]).ok();
    if !repo::remote_name(remote) || repository.common_dir.join("reftable").is_dir() {
        return ask_git();
    }
    let file = repository.common_dir.join(&name);
    match std::fs::symlink_metadata(&file) {
        Err(_) => return None,
        Ok(meta) if meta.file_type().is_symlink() => return ask_git(),
        Ok(_) => {}
    }
    let parsed = std::fs::read_to_string(&file).ok().and_then(|text| {
        let target = text.strip_prefix("ref:")?.trim();
        (!target.is_empty()).then(|| target.to_string())
    });
    parsed.or_else(ask_git)
}

/// The remote's default branch as `refs/remotes/REMOTE/HEAD` names it, when
/// the ref it names exists (`target` is the ref, and `exists` says whether
/// it does, as one batch answered); else `main` and `master`.
fn default_branches(
    remote: Option<&str>,
    target: Option<&str>,
    exists: bool,
) -> (Vec<String>, Chosen) {
    let fallback = || vec!["refs/heads/main".into(), "refs/heads/master".into()];
    let (Some(remote), Some(target)) = (remote, target) else {
        return (fallback(), Chosen::Unset);
    };
    let Some(branch) = target.strip_prefix(&format!("refs/remotes/{remote}/")) else {
        return (fallback(), Chosen::Unset);
    };
    if !exists {
        return (fallback(), Chosen::Stale(target.to_string()));
    }
    (vec![format!("refs/heads/{branch}")], Chosen::Known)
}

/// The refs `--branch` names: exactly `refs/heads/BRANCH` for each. A value
/// that is not a branch name would gate nothing, so it is an error instead.
/// Checked by git's own rules for ref names, in this process
/// (`repo::branch_name`), including the two `git branch` refuses: `HEAD` and
/// a leading `-`.
fn named_branches(branches: &[String]) -> Result<Vec<String>, String> {
    branches
        .iter()
        .map(|branch| {
            if !repo::branch_name(branch) {
                return Err(format!(
                    "--branch {branch:?} is not a branch name; name it as `git push REMOTE NAME` \
                     would (main, not refs/heads/main)"
                ));
            }
            Ok(format!("refs/heads/{branch}"))
        })
        .collect()
}

/// The root contracts a commit may carry, as the hook gates on them:
/// `sykli.json`, else `sykli.rs` (which `verify --commit` cannot read, so it
/// refuses).
const ROOT_CONTRACTS: [&str; 2] = ["sykli.json", "sykli.rs"];

/// The root contract `commit` carries, from the batch's answers.
fn contract_in(answers: &crate::Answers, commit: &str) -> Option<PathBuf> {
    ROOT_CONTRACTS
        .into_iter()
        .find(|name| crate::answer(answers, &format!("{commit}:{name}")).is_some())
        .map(PathBuf::from)
}

/// The remote-tracking branch a pushed ref replaces, `refs/remotes/R/B`.
fn tracking(update: &Update, remote: Option<&str>) -> Option<String> {
    let branch = update.remote_ref.strip_prefix("refs/heads/")?;
    Some(format!("refs/remotes/{}/{branch}", remote?))
}

/// What the remote's branch points at before this push, as far as this
/// repository knows: the line's remote sha when its commit is here, else the
/// remote-tracking branch. Its id, and the name the batch asked about it by.
fn remote_tip(
    answers: &crate::Answers,
    update: &Update,
    remote: Option<&str>,
) -> Option<(String, String)> {
    if !absent(&update.remote_sha)
        && crate::answer(answers, &format!("{}^{{commit}}", update.remote_sha)).is_some()
    {
        return Some((update.remote_sha.clone(), update.remote_sha.clone()));
    }
    let tracking = tracking(update, remote)?;
    crate::answer(answers, &tracking).map(|object| (object.id.clone(), tracking))
}

/// Every name the push's checks need from git, asked at once: for each
/// pushed branch, its commit and tree; the contract and lock it carries (or
/// the named contract, when that is inside the worktree); and, when no
/// contract is named, whether the branch it replaces had one.
fn questions(
    pushed: &[&Update],
    remote: Option<&str>,
    gate: &Option<(PathBuf, bool)>,
    head_target: Option<&str>,
) -> Vec<String> {
    let mut names = Vec::new();
    names.extend(head_target.map(str::to_string));
    for update in pushed
        .iter()
        .filter(|update| update.remote_ref.starts_with("refs/heads/"))
    {
        let sha = &update.local_sha;
        names.extend(crate::commit_names(sha));
        match gate {
            Some((_, true)) => {}
            Some((contract, false)) => {
                if let Ok(inside) = crate::ContractNames::new(sha, contract) {
                    names.extend(inside.names());
                }
            }
            None => {
                for root in ROOT_CONTRACTS {
                    if let Ok(inside) = crate::ContractNames::new(sha, Path::new(root)) {
                        names.extend(inside.names());
                    }
                }
                let mut tips = Vec::new();
                if !absent(&update.remote_sha) {
                    names.push(format!("{}^{{commit}}", update.remote_sha));
                    tips.push(update.remote_sha.clone());
                }
                if let Some(tracking) = tracking(update, remote) {
                    names.push(tracking.clone());
                    tips.push(tracking);
                }
                for tip in tips {
                    names.extend(ROOT_CONTRACTS.map(|name| format!("{tip}:{name}")));
                }
            }
        }
    }
    names
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

/// A receipt in this worktree that passes `verify --commit` for a commit,
/// judged against facts already read, with the bytes that verified (read
/// before and after, and the same), or why there is none. Kept evidence is
/// those bytes, never a second read of the path.
fn receipt_for(
    at: Result<&crate::CommitAt, &str>,
    judged: Result<&crate::Judged, &str>,
) -> Result<(PathBuf, Vec<u8>), String> {
    let at = at.map_err(str::to_string)?;
    let candidates = receipts_of(&at.tree);
    let mut newest_refusal = None;
    for receipt in candidates {
        let before = std::fs::read(&receipt).ok();
        let refused = match judge(&receipt, judged.map(|judged| (judged, at))) {
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

/// What the pre-push hook does with one gated ref, decided before any
/// verdict is printed.
enum Step<'a> {
    /// The pushed commit carries no contract: `tip` is the commit the branch
    /// had when it carried one, so the push would take the gate off.
    Ungated {
        update: &'a Update,
        tip: Option<String>,
    },
    /// A named contract inside the worktree that the commit does not carry.
    NotCarried {
        update: &'a Update,
        contract: PathBuf,
    },
    /// A receipt must verify for the commit: its facts, and the contract
    /// judged from the commit (`owned`) unless a named outside contract is.
    Judge {
        update: &'a Update,
        contract: PathBuf,
        outside: bool,
        at: Box<Result<crate::CommitAt, String>>,
        owned: Box<Option<Result<crate::Judged, String>>>,
    },
}

/// What would let a refused push through. While the contract differs from
/// its own lock no run can pass, so that says to re-lock; otherwise, to run.
fn next_step(contract: &Path, drifted: bool, outside: bool, short: &str) -> String {
    let contract_path = contract.display();
    if !drifted {
        return format!(
            "to produce a receipt, check out {short} with nothing uncommitted and run: sykli run {contract_path}"
        );
    }
    if outside {
        format!(
            "the contract differs from its lock; to accept the edit, run: sykli lock {contract_path}, \
             then check out {short} with nothing uncommitted and run: sykli run {contract_path}"
        )
    } else {
        format!(
            "the contract in {short} differs from its lock; run: sykli lock {contract_path}, \
             commit the lock, and run: sykli run {contract_path} on that commit"
        )
    }
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
///
/// The hook runs on every push, so it asks git once: one `git cat-file
/// --batch` answers everything about every pushed commit, receipts are read
/// as files, and nothing rebuilds a snapshot of the working tree, since the
/// pushed commits are what is judged. With the repository given
/// (`--repo-root`, `--git-common-dir`) that is the only git process.
pub(crate) fn pre_push(
    remote: Option<&str>,
    contract: Option<&Path>,
    branches: &[String],
) -> ExitCode {
    const HOOK: &str = "pre-push";
    let repository = match enter_repository(HOOK) {
        Ok(repository) => repository,
        Err(code) => return code,
    };
    let gate = match contract.map(|contract| named_contract(HOOK, contract)) {
        Some(Ok(contract)) => {
            let outside = crate::outside_repository(&contract).is_some();
            Some((contract, outside))
        }
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
    let pushed = updates
        .iter()
        .filter(|update| !absent(&update.local_sha))
        .collect::<Vec<_>>();
    let head_target = if named.is_empty() {
        remote.and_then(|remote| remote_head(repository, remote))
    } else {
        None
    };
    let mut session = repository.session(Vec::new());
    let mut answers = match crate::ask_in(
        &mut session,
        questions(&pushed, remote, &gate, head_target.as_deref()),
    ) {
        Ok(answers) => answers,
        Err(error) => {
            eprintln!("sykli hook {HOOK}: cannot read what is pushed: {error}");
            return ExitCode::from(CANNOT_EVALUATE);
        }
    };
    let (defaults, chosen) = if named.is_empty() {
        let exists = head_target
            .as_deref()
            .is_some_and(|target| crate::answer(&answers, target).is_some());
        default_branches(remote, head_target.as_deref(), exists)
    } else {
        (named, Chosen::Known)
    };
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
    // A named contract outside the repository is the same for every commit:
    // read once, from its file.
    let outside_judged = match &gate {
        Some((contract, true)) => Some(crate::Judged::from_file(contract, true)),
        _ => None,
    };
    // First, what each gated ref needs, printing nothing; then one more
    // round of the same git process for the inputs each judged contract
    // declares; then the verdicts, in order, as the hook always gave them.
    let mut steps = Vec::new();
    let mut input_names = Vec::new();
    for update in pushed
        .into_iter()
        .filter(|update| defaults.contains(&update.remote_ref))
    {
        let sha = &update.local_sha;
        let named_gate = gate.as_ref().map(|(contract, _)| contract.clone());
        let Some(contract) = named_gate.or_else(|| contract_in(&answers, sha)) else {
            let before = remote_tip(&answers, update, remote);
            let tip = before
                .filter(|(_, asked)| contract_in(&answers, asked).is_some())
                .map(|(tip, _)| tip);
            steps.push(Step::Ungated { update, tip });
            continue;
        };
        let outside = matches!(gate, Some((_, true)));
        let inside = (!outside).then(|| crate::ContractNames::new(sha, &contract));
        // A named contract inside the worktree is read from the commit, as a
        // committed one is; one the commit does not carry can never verify.
        if gate.is_some()
            && !outside
            && !inside
                .as_ref()
                .and_then(|inside| inside.as_ref().ok())
                .is_some_and(|inside| inside.carried(&answers))
        {
            steps.push(Step::NotCarried { update, contract });
            continue;
        }
        let at = crate::commit_from(sha, &answers);
        let owned = inside.map(|inside| {
            let id = at.as_ref().map_or(sha.as_str(), |at| at.id.as_str());
            inside.and_then(|inside| inside.judged(&answers, id))
        });
        let judged = match (&outside_judged, &owned) {
            (Some(judged), _) => judged.as_ref().ok(),
            (None, Some(owned)) => owned.as_ref().ok(),
            (None, None) => None,
        };
        if let (Some(judged), Ok(_)) = (judged, &at) {
            match crate::input_names(sha, &judged.contract) {
                Ok(names) => input_names.extend(names),
                Err(error) => {
                    eprintln!("sykli hook {HOOK}: cannot read what is pushed: {error}");
                    return ExitCode::from(CANNOT_EVALUATE);
                }
            }
        }
        steps.push(Step::Judge {
            update,
            contract,
            outside,
            at: Box::new(at),
            owned: Box::new(owned),
        });
    }
    let read = crate::ask_in(&mut session, input_names).and_then(|more| {
        answers.extend(more);
        session.finish()
    });
    if let Err(error) = read {
        eprintln!("sykli hook {HOOK}: cannot read what is pushed: {error}");
        return ExitCode::from(CANNOT_EVALUATE);
    }
    let mut accepted = Vec::new();
    for step in steps {
        let (update, contract, outside, mut at, owned) = match step {
            Step::Ungated { update, tip } => {
                let short = &update.local_sha[..update.local_sha.len().min(12)];
                if let Some(tip) = tip {
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
            }
            Step::NotCarried { update, contract } => {
                let short = &update.local_sha[..update.local_sha.len().min(12)];
                eprintln!(
                    "sykli hook {HOOK}: refusing to push {short} to {}: the contract {} is inside this \
                     worktree, but {short} does not carry it, so `verify --commit` cannot read it and no \
                     receipt can pass; keep the contract outside the worktree, or commit it",
                    update.remote_ref,
                    contract.display()
                );
                return ExitCode::from(REFUSED);
            }
            Step::Judge {
                update,
                contract,
                outside,
                at,
                owned,
            } => (update, contract, outside, *at, *owned),
        };
        let sha = &update.local_sha;
        let short = &sha[..sha.len().min(12)];
        let judged = match (&outside_judged, &owned) {
            (Some(judged), _) => judged.as_ref().map_err(String::as_str),
            (None, Some(owned)) => owned.as_ref().map_err(String::as_str),
            (None, None) => Err("no contract to judge by"),
        };
        if let (Ok(judged), Ok(at)) = (judged, at.as_mut()) {
            match crate::inputs_from(sha, &judged.contract, &answers) {
                Ok(inputs) => at.inputs = Some(inputs),
                Err(error) => {
                    eprintln!("sykli hook {HOOK}: cannot read what is pushed: {error}");
                    return ExitCode::from(CANNOT_EVALUATE);
                }
            }
        }
        match receipt_for(at.as_ref().map_err(String::as_str), judged) {
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
                let drifted = judged.as_ref().is_ok_and(|judged| judged.drifted());
                eprintln!(
                    "sykli hook {HOOK}: {}",
                    next_step(&contract, drifted, outside, short)
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
        match keep_evidence(&repository.common_dir, &bytes) {
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
fn keep_evidence(common_dir: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    let error = |what: &Path, error: std::io::Error| format!("{}: {error}", what.display());
    let mut directory = common_dir.to_path_buf();
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
