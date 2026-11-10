//! `sykli deploy` and `sykli deployed`: run a deploy contract for a proven,
//! landed commit and keep a record that it went out; then say what went out.
//!
//! `deploy` refuses unless nothing is uncommitted, the commit's tree has a
//! passing receipt of the gate contract that verifies for it (as `land`
//! judges it), and the commit is in the remote's default branch as this
//! clone last fetched it (it fetches nothing itself; `--allow-unlanded`
//! overrides, and the record says so). Then it runs the deploy contract as
//! `run` does, except that no task's pass is reused from the cache: a
//! deployment that did not run did not happen. Only a run that passed is
//! recorded.
//!
//! The record is one JSON line (`sykli-deployment.v1`) appended to a git
//! note on the commit under `refs/notes/sykli/deploy`: one line per
//! deployment, so the notes grow with the deployments made, as a log does.
//! `deployed` reads those notes in this clone and nothing else: what it has
//! no record of, it does not know.
//!
//! Exit codes of `deploy`: 0 deployed and recorded; 1 refused, or the deploy
//! run failed (nothing recorded); 2 could not tell, or deployed but the
//! record could not be kept or shared; 128+N stopped by signal N.

use crate::land::{self, Proof, Prover};
use crate::review::{self, git, read_note, write_note};
use crate::{Ran, repo, sha256};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

/// Where deployments are recorded: one note per commit, one line each.
pub(crate) const NOTES: &str = "refs/notes/sykli/deploy";
const SCHEMA: &str = "sykli-deployment.v1";
/// How notes of two clones on one commit are merged: every line of both.
const MERGE: &str = "cat_sort_uniq";

const REFUSED: u8 = 1;
const CANNOT_TELL: u8 = 2;

/// `sykli-deployment.v1`: one deployment of one commit to one target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Deployment {
    pub(crate) schema: String,
    pub(crate) commit: String,
    pub(crate) tree: String,
    pub(crate) target: String,
    /// The deploy contract, as a path from the repository's top level (or
    /// its own path when it is kept outside the repository), and its hash.
    pub(crate) contract: String,
    pub(crate) contract_digest: String,
    /// The SHA-256 of the receipt the deploy run wrote.
    pub(crate) receipt_digest: String,
    pub(crate) at: u64,
    /// True when the commit was not shown to be in the remote's default
    /// branch and `--allow-unlanded` let it out anyway.
    pub(crate) unlanded: bool,
}

/// What `deploy` checked before it ran anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ready {
    pub(crate) head: String,
    pub(crate) tree: String,
    pub(crate) unlanded: bool,
}

/// Whether the commit checked out is in the remote's default branch, as
/// this clone last fetched it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Landed {
    Yes,
    /// It is not in the branch, shown as `REMOTE/BRANCH`.
    No(String),
    /// The branch, or this clone's copy of it, is not known here.
    Unknown(String),
}

fn landed(root: &Path, remote: &str, branch: Option<&str>, head: &str) -> Result<Landed, String> {
    let branch = match review::default_branch(root, remote, branch) {
        Ok(branch) => branch,
        Err(why) => return Ok(Landed::Unknown(why)),
    };
    let tracking = format!("refs/remotes/{remote}/{branch}");
    if !review::commit_exists(root, &tracking) {
        return Ok(Landed::Unknown(format!(
            "this clone has no {tracking}: fetch it"
        )));
    }
    Ok(if land::is_ancestor(root, head, &tracking)? {
        Landed::Yes
    } else {
        Landed::No(format!("{remote}/{branch}"))
    })
}

/// The checks before anything runs. `Ok(Err(why))` is a refusal; `Err` is
/// what could not be told.
pub(crate) fn preflight(
    root: &Path,
    remote: &str,
    branch: Option<&str>,
    allow_unlanded: bool,
    prover: Prover<'_>,
) -> Result<Result<Ready, String>, String> {
    if review::uncommitted(root)? {
        return Ok(Err(
            "uncommitted or untracked files here: commit first".into()
        ));
    }
    let head = git(root, &["rev-parse", "HEAD"])?;
    let tree = git(root, &["rev-parse", "HEAD^{tree}"])?;
    if let Proof::Not(why) = prover(root, &head, &tree)? {
        return Ok(Err(format!(
            "not proven ({why}): run `sykli run` on this commit"
        )));
    }
    let unlanded = match landed(root, remote, branch, &head)? {
        Landed::Yes => false,
        Landed::No(_) | Landed::Unknown(_) if allow_unlanded => true,
        Landed::No(shown) => {
            return Ok(Err(format!(
                "this commit is not in {shown} as last fetched: land it (and fetch), or pass \
                 --allow-unlanded, which the record will say"
            )));
        }
        Landed::Unknown(why) => {
            return Err(format!(
                "whether this commit is landed cannot be told ({why}); --allow-unlanded \
                 deploys it anyway, and the record will say so"
            ));
        }
    };
    Ok(Ok(Ready {
        head,
        tree,
        unlanded,
    }))
}

/// Append a deployment to the note on its commit: the lines already there,
/// then this one.
pub(crate) fn record(root: &Path, deployment: &Deployment) -> Result<(), String> {
    let mut note = read_note(root, NOTES, &deployment.commit)?.unwrap_or_default();
    if !note.is_empty() && !note.ends_with(b"\n") {
        note.push(b'\n');
    }
    note.extend(serde_json::to_vec(deployment).map_err(|error| error.to_string())?);
    note.push(b'\n');
    write_note(root, NOTES, &deployment.commit, &note)
}

/// Take in the remote's records and push this clone's. Where both hold a
/// note for a commit, every line of both is kept.
pub(crate) fn share(root: &Path, remote: &str) -> Result<(), String> {
    land::fetch_notes(root, remote, &[NOTES], MERGE)?;
    land::push_notes(root, remote, &[NOTES], MERGE)
}

/// How to share the record by hand, for a deployment made without `--remote`
/// or whose sharing failed: running `deploy` again would deploy again.
fn how_to_share(remote: &str) -> String {
    let fetched = NOTES.replacen("refs/notes/", "refs/notes-fetched/", 1);
    format!(
        "to share the record (when {remote} already has records, the first two take them in):\n  \
         git fetch {remote} +{NOTES}:{fetched}\n  \
         git notes --ref {NOTES} merge -s {MERGE} {fetched}\n  \
         git push {remote} {NOTES}"
    )
}

/// Every deployment recorded in this clone, and how many recorded lines are
/// not deployments of the commit they are on. None when this clone has no
/// record at all, which says nothing about what was deployed.
pub(crate) fn recorded(root: &Path) -> Result<Option<(Vec<Deployment>, usize)>, String> {
    if git(root, &["rev-parse", "--verify", "--quiet", NOTES]).is_err() {
        return Ok(None);
    }
    let mut deployments = Vec::new();
    let mut unreadable = 0;
    for line in git(root, &["notes", "--ref", NOTES, "list"])?.lines() {
        let Some((_, commit)) = line.split_once(' ') else {
            return Err(format!("unexpected line from git notes list: {line:?}"));
        };
        let note = read_note(root, NOTES, commit)?.unwrap_or_default();
        for line in note
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
        {
            match serde_json::from_slice::<Deployment>(line) {
                Ok(deployment) if deployment.schema == SCHEMA && deployment.commit == commit => {
                    deployments.push(deployment);
                }
                _ => unreadable += 1,
            }
        }
    }
    Ok(Some((deployments, unreadable)))
}

/// The latest deployment of each target, by target name.
pub(crate) fn latest(deployments: Vec<Deployment>) -> BTreeMap<String, Deployment> {
    let mut latest = BTreeMap::<String, Deployment>::new();
    for deployment in deployments {
        match latest.get(&deployment.target) {
            Some(kept) if kept.at > deployment.at => {}
            _ => {
                latest.insert(deployment.target.clone(), deployment);
            }
        }
    }
    latest
}

/// Seconds since 1970 as `YYYY-MM-DD HH:MM:SS UTC`.
pub(crate) fn utc(at: u64) -> String {
    let (days, seconds) = (at / 86_400, at % 86_400);
    // Days to a civil date, in the proleptic Gregorian calendar: years are
    // counted from 1 March, so a leap day is the last day of its year.
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        seconds / 3_600,
        seconds % 3_600 / 60,
        seconds % 60
    )
}

/// A target's name: something to print on a line of its own.
fn target_name(target: &str) -> Result<(), String> {
    if target.is_empty() || target.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(format!(
            "--target {target:?} is not a target name: one word, no spaces"
        ));
    }
    Ok(())
}

/// The receipt a deploy run wrote, as proof that the deployment ran: its
/// digest, once every task in it is `passed`. A task reused from the cache
/// did not run, so it deployed nothing.
fn ran_receipt(receipt: &Path) -> Result<String, String> {
    let bytes = fs::read(receipt).map_err(|error| format!("{}: {error}", receipt.display()))?;
    let parsed: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("{}: {error}", receipt.display()))?;
    let tasks = parsed["tasks"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if tasks.is_empty() {
        return Err("the deploy contract ran no task".into());
    }
    if let Some(task) = tasks.iter().find(|task| task["outcome"] != "passed") {
        return Err(format!(
            "task {} of the deploy contract is {} in its receipt, not passed: it did not run now",
            task["name"].as_str().unwrap_or("?"),
            task["outcome"].as_str().unwrap_or("?")
        ));
    }
    Ok(sha256(&bytes))
}

/// `sykli deploy [CONTRACT] [--target NAME] [--remote NAME] [--branch NAME]
/// [--allow-unlanded]`.
pub(crate) fn command(
    contract: &Path,
    target: &str,
    remote: Option<&str>,
    branch: Option<&str>,
    allow_unlanded: bool,
) -> ExitCode {
    let cannot = |error: String| {
        eprintln!("deploy: {error}");
        ExitCode::from(CANNOT_TELL)
    };
    let repository = match repo::repository() {
        Ok(repository) => repository,
        Err(_) => return cannot("not inside a git repository".into()),
    };
    let root = repository.root.as_path();
    if let Err(error) = target_name(target) {
        return cannot(error);
    }
    // Read before anything is checked: a deploy contract that cannot be
    // read, or differs from its lock, is the first thing to say.
    let ((loaded, levels, hash), lock) = match crate::load_pinned(contract) {
        Ok(loaded) => loaded,
        Err(error) => return cannot(format!("{}: {error}", contract.display())),
    };
    let shown_remote = remote.unwrap_or("origin");
    let ready = match preflight(
        root,
        shown_remote,
        branch,
        allow_unlanded,
        &land::sykli_proof,
    ) {
        Ok(Ok(ready)) => ready,
        Ok(Err(why)) => {
            eprintln!("deploy: refused: {why}");
            return ExitCode::from(REFUSED);
        }
        Err(error) => return cannot(error),
    };
    let short = &ready.head[..12.min(ready.head.len())];
    let ran = match crate::run_reusing(
        contract,
        &loaded,
        &levels,
        hash.clone(),
        lock,
        false,
        false,
        false,
    ) {
        Ok(ran) => ran,
        Err(error) => {
            return cannot(format!(
                "the deploy contract could not be run, so nothing is recorded as deployed: {error}"
            ));
        }
    };
    let receipt = match ran {
        Ran::Concluded {
            passed: true,
            receipt,
        } => receipt,
        Ran::Concluded { passed: false, .. } => {
            eprintln!(
                "deploy: the deploy run failed: nothing is recorded as deployed. What its tasks \
                 did before failing is not known to sykli"
            );
            return ExitCode::from(REFUSED);
        }
        other => {
            eprintln!("deploy: the deploy run was stopped: nothing is recorded as deployed");
            return crate::exit_for(other, false);
        }
    };
    let kept = ran_receipt(&receipt).and_then(|receipt_digest| {
        if git(root, &["rev-parse", "HEAD"])? != ready.head {
            return Err("HEAD moved while the deploy contract ran".into());
        }
        let deployment = Deployment {
            schema: SCHEMA.into(),
            commit: ready.head.clone(),
            tree: ready.tree.clone(),
            target: target.into(),
            contract: crate::path_in_repository(repository, contract)
                .unwrap_or_else(|_| contract.display().to_string()),
            contract_digest: hash,
            receipt_digest,
            at: review::now(),
            unlanded: ready.unlanded,
        };
        record(root, &deployment)
    });
    if let Err(error) = kept {
        return cannot(format!(
            "the deploy run passed, but no deployment is recorded: {error}"
        ));
    }
    let unlanded = if ready.unlanded { ", unlanded" } else { "" };
    let said = format!("deploy: {short} deployed to {target}{unlanded}; recorded on the commit");
    match remote {
        None => {
            println!("{said} in this clone only");
            println!("deploy: {}", how_to_share(shown_remote));
            ExitCode::SUCCESS
        }
        Some(remote) => match share(root, remote) {
            Ok(()) => {
                println!("{said} and shared with {remote}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!(
                    "deploy: {short} is deployed to {target} and recorded in this clone, but not \
                     shared with {remote}: {error}"
                );
                eprintln!("deploy: {}", how_to_share(remote));
                ExitCode::from(CANNOT_TELL)
            }
        },
    }
}

/// `sykli deployed [--target NAME] [--json]`.
pub(crate) fn deployed(target: Option<&str>, json: bool) -> ExitCode {
    let read = repo::repository()
        .map_err(|_| "not inside a git repository".to_string())
        .and_then(|repository| {
            let root = repository.root.as_path();
            let recorded = recorded(root)?;
            Ok((root, recorded))
        });
    let (root, recorded) = match read {
        Ok(read) => read,
        Err(error) => {
            eprintln!("deployed: {error}");
            return ExitCode::from(CANNOT_TELL);
        }
    };
    let known = recorded.is_some();
    let (deployments, unreadable) = recorded.unwrap_or_default();
    let targets = latest(deployments)
        .into_values()
        .filter(|deployment| target.is_none_or(|target| deployment.target == target))
        .map(|deployment| {
            // None when this clone does not have the commit.
            let subject = git(root, &["log", "-1", "--format=%s", &deployment.commit]).ok();
            (deployment, subject)
        })
        .collect::<Vec<_>>();
    if json {
        println!(
            "{}",
            serde_json::json!({
                "schema": "sykli-deployed.v1",
                "recorded": known,
                "unreadable": unreadable,
                "targets": targets.iter().map(|(deployment, subject)| serde_json::json!({
                    "target": deployment.target,
                    "commit": deployment.commit,
                    "tree": deployment.tree,
                    "subject": subject,
                    "at": deployment.at,
                    "contract": deployment.contract,
                    "contract_digest": deployment.contract_digest,
                    "receipt_digest": deployment.receipt_digest,
                    "unlanded": deployment.unlanded,
                })).collect::<Vec<_>>(),
            })
        );
        return ExitCode::SUCCESS;
    }
    if unreadable > 0 {
        eprintln!(
            "deployed: {unreadable} recorded line{} could not be read as a deployment of the \
             commit {} on, and {} not counted",
            if unreadable == 1 { "" } else { "s" },
            if unreadable == 1 { "it is" } else { "they are" },
            if unreadable == 1 { "is" } else { "are" },
        );
    }
    if targets.is_empty() {
        let of = target
            .map(|target| format!(" for {target}"))
            .unwrap_or_default();
        println!(
            "deployed: nothing has been recorded in this clone{of} ({NOTES}{}). That is not \
             \"nothing was deployed\": another clone may hold records; fetch them with \
             git fetch REMOTE '{NOTES}:{NOTES}'",
            if known { "" } else { " is not here" }
        );
        return ExitCode::SUCCESS;
    }
    for (deployment, subject) in targets {
        println!(
            "{}: {}  {}  {}{}",
            deployment.target,
            &deployment.commit[..12.min(deployment.commit.len())],
            subject.as_deref().unwrap_or("(commit not in this clone)"),
            utc(deployment.at),
            if deployment.unlanded {
                "  (unlanded)"
            } else {
                ""
            }
        );
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::tests::{Fixture, branch_repo, run_git};

    fn proven(_: &Path, _: &str, _: &str) -> Result<Proof, String> {
        Ok(Proof::Passed { receipt: None })
    }

    fn deployment(commit: &str, target: &str, at: u64) -> Deployment {
        Deployment {
            schema: SCHEMA.into(),
            commit: commit.into(),
            tree: "tree".into(),
            target: target.into(),
            contract: "deploy.json".into(),
            contract_digest: "c".into(),
            receipt_digest: "r".into(),
            at,
            unlanded: false,
        }
    }

    fn head(root: &Fixture) -> String {
        git(root, &["rev-parse", "HEAD"]).unwrap()
    }

    #[test]
    fn a_commit_outside_the_default_branch_is_refused_unless_allowed() {
        let root = branch_repo("deploy-unlanded");
        let why = preflight(&root, "origin", None, false, &proven)
            .unwrap()
            .unwrap_err();
        assert!(why.contains("is not in origin/main"), "{why}");
        let ready = preflight(&root, "origin", None, true, &proven)
            .unwrap()
            .unwrap();
        assert!(ready.unlanded);
        assert_eq!(ready.head, head(&root));

        // Landed, as this clone last fetched: not unlanded, allowed or not.
        run_git(&root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        for allow in [false, true] {
            let ready = preflight(&root, "origin", None, allow, &proven)
                .unwrap()
                .unwrap();
            assert!(!ready.unlanded);
        }
        // An ancestor of the branch is in it too.
        run_git(&root, &["checkout", "-q", "HEAD~1"]);
        assert!(
            !preflight(&root, "origin", None, false, &proven)
                .unwrap()
                .unwrap()
                .unlanded
        );
    }

    #[test]
    fn a_default_branch_this_clone_does_not_know_is_unknown_not_landed() {
        let root = branch_repo("deploy-unknown");
        let error = preflight(&root, "elsewhere", None, false, &proven).unwrap_err();
        assert!(error.contains("cannot be told"), "{error}");
        let error = preflight(&root, "origin", Some("release"), false, &proven).unwrap_err();
        assert!(error.contains("refs/remotes/origin/release"), "{error}");
        // Allowed, it goes out, recorded as unlanded: it was not shown landed.
        assert!(
            preflight(&root, "elsewhere", None, true, &proven)
                .unwrap()
                .unwrap()
                .unlanded
        );
    }

    #[test]
    fn an_unproven_or_uncommitted_tree_is_refused_whatever_is_allowed() {
        let root = branch_repo("deploy-unproven");
        let unproven = |_: &Path, _: &str, _: &str| Ok(Proof::Not("sykli says failed".into()));
        let why = preflight(&root, "origin", None, true, &unproven)
            .unwrap()
            .unwrap_err();
        assert!(why.contains("not proven (sykli says failed)"), "{why}");
        let cannot = |_: &Path, _: &str, _: &str| Err("the journal cannot be read".to_string());
        assert!(preflight(&root, "origin", None, true, &cannot).is_err());
        fs::write(root.join("stray.txt"), "x\n").unwrap();
        let why = preflight(&root, "origin", None, true, &proven)
            .unwrap()
            .unwrap_err();
        assert!(why.contains("commit first"), "{why}");
    }

    #[test]
    fn each_deployment_is_a_line_appended_to_the_commits_note() {
        let root = branch_repo("deploy-record");
        let commit = head(&root);
        assert_eq!(recorded(&root).unwrap(), None);
        let first = deployment(&commit, "default", 10);
        let second = Deployment {
            unlanded: true,
            ..deployment(&commit, "staging", 20)
        };
        record(&root, &first).unwrap();
        record(&root, &second).unwrap();
        let note = String::from_utf8(read_note(&root, NOTES, &commit).unwrap().unwrap()).unwrap();
        let lines = note.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2, "{note}");
        assert!(note.ends_with('\n'));
        assert_eq!(
            lines[0],
            format!(
                "{{\"schema\":\"sykli-deployment.v1\",\"commit\":\"{commit}\",\"tree\":\"tree\",\
                 \"target\":\"default\",\"contract\":\"deploy.json\",\"contract_digest\":\"c\",\
                 \"receipt_digest\":\"r\",\"at\":10,\"unlanded\":false}}"
            )
        );
        assert_eq!(recorded(&root).unwrap(), Some((vec![first, second], 0)));
    }

    #[test]
    fn a_line_that_is_not_a_deployment_of_its_commit_is_counted() {
        let root = branch_repo("deploy-unreadable");
        let commit = head(&root);
        let other = git(&root, &["rev-parse", "HEAD~1"]).unwrap();
        let good = deployment(&commit, "default", 10);
        record(&root, &good).unwrap();
        // Copied to another commit, of another schema, and not JSON at all.
        record(&root, &deployment(&commit, "default", 11)).unwrap();
        let copied = read_note(&root, NOTES, &commit).unwrap().unwrap();
        write_note(&root, NOTES, &other, &copied).unwrap();
        let mut note = serde_json::to_vec(&good).unwrap();
        note.extend(b"\n\n");
        note.extend(
            serde_json::to_vec(&Deployment {
                schema: "sykli-deployment.v2".into(),
                ..good.clone()
            })
            .unwrap(),
        );
        note.extend(b"\nnot json\n");
        write_note(&root, NOTES, &commit, &note).unwrap();
        assert_eq!(recorded(&root).unwrap(), Some((vec![good], 4)));
    }

    #[test]
    fn the_latest_deployment_of_each_target_is_what_went_out() {
        let latest = latest(vec![
            deployment("a", "default", 10),
            deployment("b", "default", 30),
            deployment("c", "default", 20),
            deployment("d", "staging", 5),
        ]);
        assert_eq!(
            latest
                .iter()
                .map(|(target, deployment)| (target.as_str(), deployment.commit.as_str()))
                .collect::<Vec<_>>(),
            [("default", "b"), ("staging", "d")]
        );
        assert!(super::latest(Vec::new()).is_empty());
    }

    #[test]
    fn a_receipt_with_a_task_reused_from_the_cache_is_no_deployment() {
        let root = Fixture::new("deploy-receipt");
        fs::create_dir_all(&root).unwrap();
        let receipt = root.join("receipt.json");
        let write = |outcomes: &[&str]| {
            let tasks = outcomes
                .iter()
                .map(|outcome| serde_json::json!({"name": "ship", "outcome": outcome}))
                .collect::<Vec<_>>();
            let bytes = serde_json::to_vec(&serde_json::json!({"tasks": tasks})).unwrap();
            fs::write(&receipt, &bytes).unwrap();
            sha256(&bytes)
        };
        let digest = write(&["passed", "passed"]);
        assert_eq!(ran_receipt(&receipt).unwrap(), digest);
        write(&["passed", "cached"]);
        let error = ran_receipt(&receipt).unwrap_err();
        assert!(
            error.contains("ship") && error.contains("cached"),
            "{error}"
        );
        write(&[]);
        assert!(ran_receipt(&receipt).is_err(), "no task is no deployment");
        fs::write(&receipt, "not json").unwrap();
        assert!(ran_receipt(&receipt).is_err());
        assert!(ran_receipt(&root.join("gone.json")).is_err());
    }

    #[test]
    fn a_time_is_shown_as_a_utc_date() {
        assert_eq!(utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc(951_782_399), "2000-02-28 23:59:59 UTC");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(utc(951_868_800), "2000-03-01 00:00:00 UTC");
        assert_eq!(utc(1_794_335_676), "2026-11-10 18:34:36 UTC");
        assert_eq!(utc(4_107_542_399), "2100-02-28 23:59:59 UTC");
        assert_eq!(utc(4_107_542_400), "2100-03-01 00:00:00 UTC");
    }

    #[test]
    fn a_target_is_one_word() {
        assert!(target_name("default").is_ok());
        assert!(target_name("eu-west.1").is_ok());
        for bad in ["", "two words", "line\nbreak", "tab\tbed"] {
            assert!(target_name(bad).is_err(), "{bad:?}");
        }
    }
}
