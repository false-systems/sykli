//! `sykli land`: put the commit checked out on the remote's default branch
//! without a pull request, when it is proven and reviewed.
//!
//! It refuses unless all of these hold for the commit checked out:
//!
//! - nothing is uncommitted, and the remote's default branch, fetched now, is
//!   an ancestor of it, so the push is a fast-forward and the tree that lands
//!   is the tree that was checked;
//! - its tree has a passing receipt that verifies for the commit, as
//!   `status --commit` judges what this clone keeps, or else a receipt another
//!   clone shared verifies here, as `verify --commit` judges it;
//! - a review of this tree against exactly that branch tip is kept, and it
//!   is not blocking, or a person overruled it.
//!
//! Then it shares the receipt and the review as git notes on the tree
//! (`refs/notes/sykli/receipt`, `refs/notes/sykli/review`), and only then
//! pushes the commit, so another clone can check the commit without running
//! anything again. One note per tree in each, so they grow with the trees
//! landed.
//!
//! Exit codes: 0 landed, 1 refused (it says what is missing), 2 it could not
//! tell or the push failed.

use crate::review::{self, Level, git, read_note, write_note};
use crate::{refusal, repo, sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Where receipts travel: one note per tree, the receipt as sykli wrote it.
pub(crate) const RECEIPTS: &str = "refs/notes/sykli/receipt";

const REFUSED: u8 = 1;
const CANNOT_TELL: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Proof {
    /// The tree passed. `receipt` is the receipt to share, unless the proof
    /// is itself a shared receipt or the file is gone.
    Passed {
        receipt: Option<Vec<u8>>,
    },
    Not(String),
}

/// Says whether a commit's tree is proven: `(root, commit, tree)`.
pub(crate) type Prover<'a> = &'a dyn Fn(&Path, &str, &str) -> Result<Proof, String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Landed {
        head: String,
        branch: String,
        review: Level,
    },
    Refused(String),
}

pub(crate) fn land(
    root: &Path,
    remote: &str,
    branch: Option<&str>,
    prover: Prover<'_>,
) -> Result<Outcome, String> {
    if review::uncommitted(root)? {
        return refused("uncommitted or untracked files here: commit first");
    }
    let branch = review::default_branch(root, remote, branch)?;
    let tracking = format!("refs/remotes/{remote}/{branch}");
    git(
        root,
        &[
            "fetch",
            "--quiet",
            remote,
            &format!("+refs/heads/{branch}:{tracking}"),
        ],
    )?;
    fetch_notes(root, remote, &[review::NOTES, RECEIPTS], "ours")?;
    let shown = format!("{remote}/{branch}");
    let target = git(root, &["rev-parse", &tracking])?;
    let head = git(root, &["rev-parse", "HEAD"])?;
    let tree = git(root, &["rev-parse", "HEAD^{tree}"])?;
    if head == target {
        return refused(&format!("nothing to land: {shown} is already this commit"));
    }
    if !is_ancestor(root, &target, &head)? {
        return refused(&format!(
            "{shown} has commits this branch lacks: merge it in, then prove and review the result"
        ));
    }
    let receipt = match prover(root, &head, &tree)? {
        Proof::Passed { receipt } => receipt,
        Proof::Not(why) => {
            return refused(&format!(
                "not proven ({why}): run `sykli run` on this commit"
            ));
        }
    };
    let Some(record) = review::read_record(root, &tree)?.filter(|kept| kept.base == target) else {
        return refused(&format!(
            "not reviewed against this {shown}: run `sykli review`"
        ));
    };
    if review::exit_code(&record) != 0 {
        return refused(&format!(
            "the review is blocking ({} finding(s)): fix it, or a person overrules it",
            record.findings.len()
        ));
    }
    let receipt = match receipt {
        Some(receipt) => {
            write_note(root, RECEIPTS, &tree, &receipt)
                .map_err(|error| format!("could not attach the receipt to the tree: {error}"))?;
            receipt
        }
        // Proven here, but the receipt file is gone: what another clone
        // shared stands in. With neither, the branch does not move without
        // the proof that travels with it.
        None => match read_note(root, RECEIPTS, &tree)? {
            Some(shared) => shared,
            None => {
                return refused(
                    "proven, but there is no receipt left to share: run `sykli run` again",
                );
            }
        },
    };
    keep_for_hook(root, &receipt)?;
    // The evidence goes first: a commit never reaches the branch without it,
    // and a landing that fails after this step can simply be run again.
    push_notes(root, remote, &[review::NOTES, RECEIPTS], "ours").map_err(|error| {
        format!("the review and receipt could not be shared; nothing landed: {error}")
    })?;
    // The lease makes the remote refuse unless its branch is still the commit
    // that was checked: the review is of this change against that tip only.
    // It overwrites nothing, since that tip is an ancestor of the commit.
    git(
        root,
        &[
            "push",
            "--quiet",
            &format!("--force-with-lease=refs/heads/{branch}:{target}"),
            remote,
            &format!("{head}:refs/heads/{branch}"),
        ],
    )
    .map_err(|error| format!("the push to {shown} was refused: {error}"))?;
    Ok(Outcome::Landed {
        head,
        branch,
        review: record.level,
    })
}

fn refused(why: &str) -> Result<Outcome, String> {
    Ok(Outcome::Refused(why.to_string()))
}

pub(crate) fn is_ancestor(root: &Path, ancestor: &str, descendant: &str) -> Result<bool, String> {
    let status = repo::git_command()
        .current_dir(root)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|error| format!("could not start git: {error}"))?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!(
            "git merge-base --is-ancestor {ancestor} {descendant} failed"
        )),
    }
}

/// The receipt the landing rests on, where the pre-push hook looks for one:
/// this worktree's `.sykli/receipts/rcpt_<sha256>.json`, the name and place
/// `run` gives it. The hook reads receipts from the worktree only, and judges
/// whatever it finds there itself; without this a commit proven in another
/// worktree of the clone, or by a shared receipt, would be refused by it.
/// Bounded as every receipt there is: the next run prunes the directory to
/// the receipt budget (`SYKLI_RECEIPT_BUDGET`).
fn keep_for_hook(root: &Path, receipt: &[u8]) -> Result<(), String> {
    let directory = root.join(".sykli/receipts");
    let hash = sha256(receipt);
    let path = directory.join(format!("rcpt_{hash}.json"));
    if path.exists() {
        return Ok(());
    }
    let error = |error: std::io::Error| format!("{}: {error}", path.display());
    fs::create_dir_all(&directory).map_err(error)?;
    let temporary = directory.join(format!(".{hash}.tmp-{}", std::process::id()));
    fs::write(&temporary, receipt)
        .and_then(|()| fs::rename(&temporary, &path))
        .map_err(|failed| {
            let _ = fs::remove_file(&temporary);
            error(failed)
        })
}

/// Where a remote's notes are fetched to before they are merged.
fn fetched(notes: &str) -> String {
    notes.replacen("refs/notes/", "refs/notes-fetched/", 1)
}

/// Brings the remote's notes into this clone's. Where both hold a note for
/// an object, `strategy` decides, as `git notes merge -s` names it: `ours`
/// keeps this clone's.
pub(crate) fn fetch_notes(
    root: &Path,
    remote: &str,
    notes: &[&str],
    strategy: &str,
) -> Result<(), String> {
    for notes in notes {
        if git(root, &["ls-remote", remote, notes])?.is_empty() {
            continue;
        }
        let fetched = fetched(notes);
        git(
            root,
            &["fetch", "--quiet", remote, &format!("+{notes}:{fetched}")],
        )?;
        if git(root, &["rev-parse", "--quiet", "--verify", notes]).is_err() {
            git(root, &["update-ref", notes, &fetched])?;
        } else {
            git(
                root,
                &[
                    "notes", "--ref", notes, "merge", "--quiet", "-s", strategy, &fetched,
                ],
            )?;
        }
    }
    Ok(())
}

/// Pushes the notes this clone has. When the remote's moved meanwhile, they
/// are taken in and the push is tried once more.
pub(crate) fn push_notes(
    root: &Path,
    remote: &str,
    notes: &[&str],
    strategy: &str,
) -> Result<(), String> {
    let kept: Vec<&str> = notes
        .iter()
        .copied()
        .filter(|notes| git(root, &["rev-parse", "--quiet", "--verify", notes]).is_ok())
        .collect();
    if kept.is_empty() {
        return Ok(());
    }
    let mut push = vec!["push", "--quiet", remote];
    push.extend(&kept);
    if git(root, &push).is_ok() {
        return Ok(());
    }
    // Another clone shared notes meanwhile: take them in and push once more.
    fetch_notes(root, remote, &kept, strategy)?;
    git(root, &push).map(|_| ())
}

/// What sykli itself knows of the commit: first what this clone's receipts
/// say (`status --commit`), then whether a receipt another clone shared for
/// its tree verifies here (`verify --commit`). Asked in this process, by the
/// repository's own contract: the one at its top level, wherever the
/// command was typed.
pub(crate) fn sykli_proof(root: &Path, commit: &str, tree: &str) -> Result<Proof, String> {
    let contract = gate_contract(root);
    let status = crate::commit_status(commit, &contract)?;
    let verdict = status["verdict"].as_str().unwrap_or("unknown");
    if verdict == "passed" {
        let receipt = kept_receipt(&status, &|path| fs::read(path).ok());
        return Ok(Proof::Passed { receipt });
    }
    // Only a clone that knows nothing of this tree takes another clone's
    // word: a failure or a conflict recorded here stands.
    if !knows_nothing(verdict) {
        return Ok(Proof::Not(format!("sykli says {verdict}")));
    }
    let Some(shared) = read_note(root, RECEIPTS, tree)? else {
        return Ok(Proof::Not(
            "this clone keeps no receipt of its tree, and none was shared".into(),
        ));
    };
    let scratch = review::Scratch::new("sykli-shared-receipt")?;
    let copy = scratch.path().join("receipt.json");
    fs::write(&copy, &shared).map_err(|error| format!("{}: {error}", copy.display()))?;
    let unverified = match crate::verify(&copy, &contract, Some(commit)) {
        Ok(checks) => checks.iter().find(|check| !check.ok).map(refusal),
        Err(error) => Some(error),
    };
    if let Some(why) = unverified {
        return Ok(Proof::Not(format!(
            "this clone keeps no receipt of its tree, and the shared receipt does not verify \
             here: {why}"
        )));
    }
    let passed = serde_json::from_slice::<serde_json::Value>(&shared)
        .is_ok_and(|receipt| receipt_passed(&receipt));
    Ok(if passed {
        Proof::Passed { receipt: None }
    } else {
        Proof::Not("the shared receipt verifies, but not every task in it passed".into())
    })
}

/// The repository's gate contract, as `run` picks it at the top level:
/// `sykli.rs`, or `sykli.json` when only that exists.
fn gate_contract(root: &Path) -> PathBuf {
    let emitter = root.join("sykli.rs");
    if !emitter.exists() && root.join("sykli.json").exists() {
        root.join("sykli.json")
    } else {
        emitter
    }
}

/// The bytes of a verified passing receipt, as `status --commit` judged
/// them: from where it was written, or else from the copy sykli kept with
/// its run, which outlives the worktree. Bytes that are no longer the ones
/// judged (by their digest) are not taken.
fn kept_receipt(
    status: &serde_json::Value,
    read: &dyn Fn(&Path) -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    let list = |key: &str| status[key].as_array().into_iter().flatten();
    list("current")
        .filter(|entry| {
            entry["judgement"] == "verified"
                && matches!(entry["outcome"].as_str(), Some("passed" | "cached"))
        })
        .find_map(|entry| {
            let kept = list("runs")
                .filter(|run| run["run"] == entry["run"] && !entry["run"].is_null())
                .filter_map(|run| run["receipt"]["kept"]["path"].as_str());
            entry["receipt"]
                .as_str()
                .into_iter()
                .chain(kept)
                .filter_map(|path| read(Path::new(path)))
                .find(|bytes| entry["digest"] == sha256(bytes).as_str())
        })
}

/// sykli's verdict for a tree it holds no receipt of.
fn knows_nothing(verdict: &str) -> bool {
    verdict == "none"
}

/// Whether every task of a receipt passed, run or reused from sykli's cache.
fn receipt_passed(receipt: &serde_json::Value) -> bool {
    receipt["tasks"].as_array().is_some_and(|tasks| {
        !tasks.is_empty()
            && tasks
                .iter()
                .all(|task| matches!(task["outcome"].as_str(), Some("passed" | "cached")))
    })
}

/// `sykli land [--remote NAME] [--branch NAME]`.
pub(crate) fn command(remote: &str, branch: Option<&str>) -> ExitCode {
    let outcome = repo::repository()
        .map_err(|_| "not inside a git repository".to_string())
        .and_then(|repository| {
            // The contract and the receipts are looked for from the top level.
            std::env::set_current_dir(&repository.root)
                .map_err(|error| format!("cannot enter {}: {error}", repository.root.display()))?;
            land(&repository.root, remote, branch, &sykli_proof)
        });
    ExitCode::from(match outcome {
        Ok(Outcome::Landed {
            head,
            branch,
            review,
        }) => {
            println!(
                "land: {} is on {remote}'s {branch}: proven, review {review}; the receipt and \
                 the review are shared as notes on its tree",
                &head[..12.min(head.len())]
            );
            0
        }
        Ok(Outcome::Refused(why)) => {
            eprintln!("land: refused: {why}");
            REFUSED
        }
        Err(error) => {
            eprintln!("land: {error}");
            CANNOT_TELL
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::tests::{Fixture, identify, run_git};
    use crate::review::{Agent, overrule};

    const BASE: &str = "refs/remotes/origin/main";

    /// A prover whose proof comes with a receipt to share.
    fn passed(_: &Path, _: &str, _: &str) -> Result<Proof, String> {
        Ok(Proof::Passed {
            receipt: Some(b"{\"tasks\":[]}\n".to_vec()),
        })
    }

    fn refusal_of(outcome: Outcome) -> String {
        match outcome {
            Outcome::Refused(why) => why,
            Outcome::Landed { .. } => panic!("it landed"),
        }
    }

    #[test]
    fn a_proof_with_no_receipt_to_share_does_not_land() {
        let world = World::new("land-no-receipt");
        world.review("REVIEW: fine\n");
        let before = world.remote_main();
        let bare = |_: &Path, _: &str, _: &str| Ok(Proof::Passed { receipt: None });
        let why = refusal_of(land(&world.work, "origin", None, &bare).unwrap());
        assert!(why.contains("no receipt"), "{why}");
        assert_eq!(world.remote_main(), before);
        // Nor did its review reach the remote ahead of it.
        assert!(git(&world.origin(), &["rev-parse", review::NOTES]).is_err());
    }

    #[test]
    fn a_shared_receipt_counts_only_where_this_clone_knows_nothing() {
        assert!(knows_nothing("none"));
        for recorded in ["failed", "conflict", "passed", "pending", "unknown"] {
            assert!(!knows_nothing(recorded), "{recorded}");
        }
    }

    #[test]
    fn the_receipt_to_share_is_the_written_file_or_the_copy_its_run_kept() {
        let bytes = b"the receipt".to_vec();
        let digest = sha256(&bytes);
        let status = serde_json::json!({
            "current": [
                {"outcome": "failed", "judgement": "failed", "run": "r0",
                 "receipt": "/w/failed.json", "digest": digest},
                {"outcome": "passed", "judgement": "not-applicable", "run": "r0",
                 "receipt": "/w/other.json", "digest": digest},
                {"outcome": "cached", "judgement": "verified", "run": "r1",
                 "receipt": "/w/rcpt.json", "digest": digest}
            ],
            "runs": [
                {"run": "r0", "receipt": {"kept": {"path": "/git/r0.json"}}},
                {"run": "r1", "receipt": {"kept": {"path": "/git/r1.json"}}}
            ]
        });
        let asked = std::cell::RefCell::new(Vec::new());
        let read_where = |exists: fn(&Path) -> bool| {
            asked.borrow_mut().clear();
            kept_receipt(&status, &|path| {
                asked.borrow_mut().push(path.to_path_buf());
                exists(path).then(|| bytes.clone())
            })
        };
        assert_eq!(read_where(|_| true), Some(bytes.clone()));
        assert_eq!(*asked.borrow(), [PathBuf::from("/w/rcpt.json")]);
        // The worktree is gone: the copy kept with the passing run is taken,
        // never one of a failed run or of a receipt that did not verify.
        assert_eq!(
            read_where(|path| path.starts_with("/git")),
            Some(bytes.clone())
        );
        assert_eq!(
            *asked.borrow(),
            [PathBuf::from("/w/rcpt.json"), PathBuf::from("/git/r1.json")]
        );
        assert_eq!(read_where(|_| false), None);
        // Bytes that are not the ones judged are not the receipt.
        assert_eq!(
            kept_receipt(&status, &|_| Some(b"rewritten since".to_vec())),
            None
        );
    }

    #[test]
    fn a_receipt_passed_when_every_task_passed_or_was_reused() {
        let receipt = |outcomes: &[&str]| {
            serde_json::json!({"tasks": outcomes
                .iter()
                .map(|outcome| serde_json::json!({"outcome": outcome}))
                .collect::<Vec<_>>()})
        };
        assert!(receipt_passed(&receipt(&["passed", "cached"])));
        assert!(!receipt_passed(&receipt(&["passed", "failed"])));
        assert!(!receipt_passed(&receipt(&["errored"])));
        assert!(!receipt_passed(&receipt(&[])), "no task is no proof");
        assert!(!receipt_passed(&serde_json::json!({})));
    }

    #[test]
    fn a_proven_reviewed_branch_lands_and_its_review_reaches_another_clone() {
        let world = World::new("land-ok");
        world.review("REVIEW: look\n");

        let outcome = land(&world.work, "origin", None, &passed).unwrap();
        let head = world.git(&["rev-parse", "HEAD"]);
        assert_eq!(
            outcome,
            Outcome::Landed {
                head: head.clone(),
                branch: "main".into(),
                review: Level::Look
            }
        );
        assert_eq!(world.remote_main(), head);

        // Another clone has run nothing: it reads both from the remote.
        let other = world.dir.join("other");
        run_git(&world.dir, &["clone", "-q", "origin.git", "other"]);
        fetch_notes(&other, "origin", &[review::NOTES, RECEIPTS], "ours").unwrap();
        let tree = world.git(&["rev-parse", "HEAD^{tree}"]);
        let shared = review::read_record(&other, &tree).unwrap().unwrap();
        assert_eq!(shared.level, Level::Look);
        // The receipt arrives byte for byte, its last newline included.
        assert_eq!(
            read_note(&other, RECEIPTS, &tree).unwrap().unwrap(),
            b"{\"tasks\":[]}\n"
        );
    }

    #[test]
    fn an_unproven_unreviewed_or_blocked_branch_does_not_land() {
        let world = World::new("land-refused");
        let before = world.remote_main();
        let unproven = |_: &Path, _: &str, _: &str| Ok(Proof::Not("sykli says none".to_string()));

        let why = refusal_of(land(&world.work, "origin", None, &unproven).unwrap());
        assert!(why.contains("not proven"), "{why}");
        let why = refusal_of(land(&world.work, "origin", None, &passed).unwrap());
        assert!(why.contains("not reviewed"), "{why}");
        world.review("- [blocking] new.txt:1: wrong.\nREVIEW: blocking\n");
        let why = refusal_of(land(&world.work, "origin", None, &passed).unwrap());
        assert!(why.contains("blocking"), "{why}");
        fs::write(world.work.join("stray.txt"), "x\n").unwrap();
        let why = refusal_of(land(&world.work, "origin", None, &passed).unwrap());
        assert!(why.contains("commit first"), "{why}");
        fs::remove_file(world.work.join("stray.txt")).unwrap();
        assert_eq!(world.remote_main(), before, "nothing reached main");
        assert!(
            git(&world.origin(), &["rev-parse", RECEIPTS]).is_err(),
            "no refusal shared anything"
        );

        overrule(&world.work, BASE, "accepted by a person").unwrap();
        assert!(matches!(
            land(&world.work, "origin", None, &passed).unwrap(),
            Outcome::Landed { .. }
        ));
    }

    #[test]
    fn a_prover_that_cannot_tell_is_an_error_not_a_refusal_and_nothing_lands() {
        let world = World::new("land-cannot-tell");
        world.review("REVIEW: fine\n");
        let before = world.remote_main();
        let broken = |_: &Path, _: &str, _: &str| Err("the journal cannot be read".to_string());
        let error = land(&world.work, "origin", None, &broken).unwrap_err();
        assert!(error.contains("journal"), "{error}");
        assert_eq!(world.remote_main(), before);
    }

    #[test]
    fn a_branch_behind_main_does_not_land_and_an_old_review_does_not_count() {
        let world = World::new("land-behind");
        world.review("REVIEW: fine\n");
        // Someone else lands first.
        world.git(&["checkout", "-q", "-b", "other", "origin/main"]);
        fs::write(world.work.join("theirs.txt"), "theirs\n").unwrap();
        world.git(&["add", "-A"]);
        world.git(&["commit", "-q", "-m", "theirs"]);
        world.git(&["push", "-q", "origin", "HEAD:refs/heads/main"]);
        world.git(&["checkout", "-q", "work"]);

        let why = refusal_of(land(&world.work, "origin", None, &passed).unwrap());
        assert!(why.contains("merge it in"), "{why}");

        // Merged up, the tree is new: the old review is not of this change.
        world.git(&["merge", "-q", "--no-edit", "origin/main"]);
        let why = refusal_of(land(&world.work, "origin", None, &passed).unwrap());
        assert!(why.contains("not reviewed"), "{why}");
    }

    #[test]
    fn a_review_against_an_older_main_does_not_count_for_the_same_tree() {
        let world = World::new("land-old-base");
        fs::write(world.work.join("more.txt"), "more\n").unwrap();
        world.git(&["add", "-A"]);
        world.git(&["commit", "-q", "-m", "more"]);
        world.review("REVIEW: fine\n");
        // The branch's first commit lands meanwhile: same tree, another diff.
        world.git(&["push", "-q", "origin", "HEAD~1:refs/heads/main"]);
        let why = refusal_of(land(&world.work, "origin", None, &passed).unwrap());
        assert!(why.contains("not reviewed against this"), "{why}");
    }

    #[test]
    fn main_moving_between_the_check_and_the_push_stops_the_landing() {
        let world = World::new("land-race");
        fs::write(world.work.join("more.txt"), "more\n").unwrap();
        world.git(&["add", "-A"]);
        world.git(&["commit", "-q", "-m", "more"]);
        world.review("REVIEW: fine\n");
        // While the checks run, someone lands the branch's first commit: the
        // push would still be a fast-forward, of a change nobody reviewed
        // against that main.
        let racing = |root: &Path, commit: &str, tree: &str| {
            git(root, &["push", "-q", "origin", "HEAD~1:refs/heads/main"])?;
            passed(root, commit, tree)
        };
        let moved_to = world.git(&["rev-parse", "HEAD~1"]);

        let error = land(&world.work, "origin", None, &racing).unwrap_err();
        assert!(error.contains("refused"), "{error}");
        assert_eq!(
            world.remote_main(),
            moved_to,
            "main is where the other left it"
        );
    }

    #[test]
    fn notes_another_clone_shared_meanwhile_are_merged_and_this_clones_win() {
        let world = World::new("land-notes-merge");
        world.review("REVIEW: fine\n");
        let tree = world.git(&["rev-parse", "HEAD^{tree}"]);
        // Another clone shared a receipt for this tree, and one for another.
        let other = world.dir.join("other");
        run_git(&world.dir, &["clone", "-q", "origin.git", "other"]);
        identify(&other);
        let elsewhere = git(&other, &["rev-parse", "HEAD^{tree}"]).unwrap();
        write_note(&other, RECEIPTS, &tree, b"theirs\n").unwrap();
        write_note(&other, RECEIPTS, &elsewhere, b"of another tree\n").unwrap();
        // And something in place of this tree's review: this clone's own
        // stays, or the landing would rest on a review it never read.
        write_note(&other, review::NOTES, &tree, b"theirs\n").unwrap();
        run_git(&other, &["push", "-q", "origin", RECEIPTS, review::NOTES]);

        assert!(matches!(
            land(&world.work, "origin", None, &passed).unwrap(),
            Outcome::Landed { .. }
        ));
        let origin = world.origin();
        assert_eq!(
            read_note(&origin, RECEIPTS, &tree).unwrap().unwrap(),
            b"{\"tasks\":[]}\n",
            "this clone's note stands on the tree both hold one for"
        );
        assert_eq!(
            read_note(&origin, RECEIPTS, &elsewhere).unwrap().unwrap(),
            b"of another tree\n",
            "the other clone's notes are kept"
        );
    }

    #[test]
    fn a_notes_push_the_remote_refuses_is_merged_and_pushed_once_more() {
        let world = World::new("land-notes-retry");
        let tree = world.git(&["rev-parse", "HEAD^{tree}"]);
        let other = world.dir.join("other");
        run_git(&world.dir, &["clone", "-q", "origin.git", "other"]);
        identify(&other);
        let elsewhere = git(&other, &["rev-parse", "HEAD^{tree}"]).unwrap();
        write_note(&other, RECEIPTS, &tree, b"theirs\n").unwrap();
        write_note(&other, RECEIPTS, &elsewhere, b"of another tree\n").unwrap();
        run_git(&other, &["push", "-q", "origin", RECEIPTS]);
        // This clone has not fetched them: its push is not a fast-forward.
        write_note(&world.work, RECEIPTS, &tree, b"ours\n").unwrap();
        assert!(git(&world.work, &["push", "-q", "origin", RECEIPTS]).is_err());

        push_notes(&world.work, "origin", &[review::NOTES, RECEIPTS], "ours").unwrap();
        let origin = world.origin();
        assert_eq!(
            read_note(&origin, RECEIPTS, &tree).unwrap().unwrap(),
            b"ours\n"
        );
        assert_eq!(
            read_note(&origin, RECEIPTS, &elsewhere).unwrap().unwrap(),
            b"of another tree\n"
        );
    }

    #[test]
    fn the_receipt_is_put_where_the_hook_reads_it() {
        let world = World::new("land-hook-copy");
        world.review("REVIEW: fine\n");
        land(&world.work, "origin", None, &passed).unwrap();
        let kept = world.work.join(format!(
            ".sykli/receipts/rcpt_{}.json",
            sha256(b"{\"tasks\":[]}\n")
        ));
        assert_eq!(fs::read(kept).unwrap(), b"{\"tasks\":[]}\n");
    }

    /// A bare `origin.git` whose `main` holds one commit, and a clone of it
    /// on branch `work` with one commit more.
    struct World {
        dir: Fixture,
        work: PathBuf,
    }

    impl World {
        fn new(name: &str) -> Self {
            let dir = Fixture::new(name);
            let work = dir.join("work");
            fs::create_dir_all(&work).unwrap();
            run_git(&dir, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
            run_git(&work, &["init", "-q", "-b", "main"]);
            identify(&work);
            run_git(&work, &["remote", "add", "origin", "../origin.git"]);
            fs::write(work.join("README.md"), "# Demo\n").unwrap();
            run_git(&work, &["add", "-A"]);
            run_git(&work, &["commit", "-q", "-m", "initial"]);
            run_git(&work, &["push", "-q", "origin", "main"]);
            run_git(&work, &["fetch", "-q", "origin"]);
            run_git(&work, &["checkout", "-q", "-b", "work"]);
            fs::write(work.join("new.txt"), "new\n").unwrap();
            run_git(&work, &["add", "-A"]);
            run_git(&work, &["commit", "-q", "-m", "change"]);
            Self { dir, work }
        }

        fn git(&self, args: &[&str]) -> String {
            git(&self.work, args).unwrap()
        }

        fn origin(&self) -> PathBuf {
            self.dir.join("origin.git")
        }

        fn remote_main(&self) -> String {
            git(&self.origin(), &["rev-parse", "main"]).unwrap()
        }

        fn review(&self, reply: &'static str) {
            let reviewer = move |_: Agent, _: &str, _: &Path| Ok(reply.to_string());
            review::review(&self.work, BASE, &[Agent::Codex], &reviewer)
                .unwrap()
                .unwrap();
        }
    }
}
