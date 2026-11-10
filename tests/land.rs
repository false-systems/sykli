//! `sykli review` and `sykli land`, as a person runs them: the real binary,
//! real git with a bare remote, the pre-push hook installed, and a reviewer
//! on PATH that is a script, so what it is handed and what it answers are the
//! test's to decide.
#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

/// Passes while `file.txt` says `ok`, unless `.sykli/fail` exists: a file no
/// tree carries, so a test can make a run of an unchanged tree fail.
const CONTRACT: &str = r#"{"schema":"sykli-contract.v1","tasks":[{"name":"check","run":"grep -q ok file.txt && test ! -e .sykli/fail","inputs":["file.txt"]}]}"#;

/// A reviewer as `sykli review` starts `codex`: it takes its whole prompt,
/// writes its reply to the file `-o` names, and exits as told.
const REVIEWER: &str = r#"#!/bin/sh
cat > "${FAKE_PROMPT:-/dev/null}"
echo called >> "$FAKE_CALLS"
echo "the reviewer's own chatter" >&2
while [ $# -gt 0 ]; do
  if [ "$1" = "-o" ]; then out="$2"; fi
  shift
done
printf '%b\n' "${FAKE_REPLY-REVIEW: fine}" > "$out"
exit "${FAKE_EXIT:-0}"
"#;

/// A bare `origin.git` whose `main` holds the contract and a passing
/// `file.txt`, and a clone of it on branch `work` with one commit more, which
/// names the agent that wrote it.
struct World {
    dir: PathBuf,
    work: PathBuf,
    path: std::ffi::OsString,
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", text(&out.stderr));
    text(&out.stdout).trim().to_owned()
}

fn identify(root: &Path) {
    git(root, &["config", "user.name", "Widget Test"]);
    git(root, &["config", "user.email", "widget@example.invalid"]);
}

fn commit(root: &Path, message: &str) -> String {
    git(root, &["add", "--all"]);
    git(root, &["commit", "--quiet", "--no-verify", "-m", message]);
    git(root, &["rev-parse", "HEAD"])
}

impl World {
    fn new(name: &str, change: &str) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("sykli-land-{name}-{nonce}"));
        let work = dir.join("work");
        let bin = dir.join("bin");
        fs::create_dir_all(&work).unwrap();
        fs::create_dir(&bin).unwrap();
        fs::write(bin.join("codex"), REVIEWER).unwrap();
        fs::set_permissions(bin.join("codex"), fs::Permissions::from_mode(0o755)).unwrap();
        // The reviewer script, the binary under test (the hook calls
        // `sykli`), and the system's git and sh: no agent a machine happens
        // to have installed is ever started.
        let sykli = Path::new(env!("CARGO_BIN_EXE_sykli")).parent().unwrap();
        let path = std::env::join_paths([
            bin.as_path(),
            sykli,
            Path::new("/usr/bin"),
            Path::new("/bin"),
        ])
        .unwrap();
        git(
            &dir,
            &["init", "--quiet", "--bare", "-b", "main", "origin.git"],
        );
        git(&work, &["init", "--quiet", "-b", "main"]);
        identify(&work);
        git(&work, &["remote", "add", "origin", "../origin.git"]);
        fs::write(work.join("sykli.json"), CONTRACT).unwrap();
        fs::write(work.join("file.txt"), "ok one\n").unwrap();
        let world = World { dir, work, path };
        assert!(world.sykli(&["lock", "sykli.json"], &[]).status.success());
        commit(&world.work, "fixture");
        git(&world.work, &["push", "--quiet", "origin", "main"]);
        git(&world.work, &["fetch", "--quiet", "origin"]);
        git(&world.work, &["checkout", "--quiet", "-b", "work"]);
        fs::write(world.work.join("file.txt"), change).unwrap();
        commit(
            &world.work,
            "change\n\nCo-Authored-By: Claude <noreply@anthropic.com>",
        );
        world
    }

    fn sykli_in(&self, root: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_sykli"))
            .args(args)
            .current_dir(root)
            .env("PATH", &self.path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("FAKE_CALLS", self.dir.join("calls"))
            .envs(env.iter().copied())
            .output()
            .unwrap()
    }

    fn sykli(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.sykli_in(&self.work, args, env)
    }

    fn head(&self) -> String {
        git(&self.work, &["rev-parse", "HEAD"])
    }

    fn remote_main(&self) -> String {
        git(&self.dir.join("origin.git"), &["rev-parse", "main"])
    }

    /// How many times a reviewer was started.
    fn reviews(&self) -> usize {
        fs::read_to_string(self.dir.join("calls")).map_or(0, |calls| calls.lines().count())
    }

    fn done(self) {
        fs::remove_dir_all(self.dir).unwrap();
    }
}

fn exit(out: &Output, code: i32) {
    assert_eq!(
        out.status.code(),
        Some(code),
        "stdout: {}\nstderr: {}",
        text(&out.stdout),
        text(&out.stderr)
    );
}

#[test]
fn a_proven_reviewed_change_lands_and_another_clone_verifies_what_was_shared() {
    let world = World::new("ok", "ok two\n");
    exit(&world.sykli(&["hook", "install", "--pre-push"], &[]), 0);
    let before = world.remote_main();

    // Neither proven nor reviewed: each refusal says what is missing.
    let out = world.sykli(&["land"], &[]);
    exit(&out, 1);
    assert!(text(&out.stderr).contains("not proven"), "{out:?}");
    exit(&world.sykli(&["run", "sykli.json"], &[]), 0);
    let out = world.sykli(&["land"], &[]);
    exit(&out, 1);
    assert!(text(&out.stderr).contains("not reviewed"), "{out:?}");
    assert_eq!(world.remote_main(), before);

    let prompt = world.dir.join("prompt");
    let out = world.sykli(&["review"], &[("FAKE_PROMPT", prompt.to_str().unwrap())]);
    exit(&out, 0);
    assert_eq!(
        text(&out.stdout).trim(),
        "review: fine (0 blocking, 0 to look at) by codex, another agent than the author"
    );
    assert!(
        !text(&out.stderr).contains("chatter"),
        "a reviewer that gave a review is not echoed: {out:?}"
    );
    let prompt = fs::read_to_string(prompt).unwrap();
    assert!(prompt.contains("+ok two"), "the diff: {prompt}");
    assert!(prompt.contains(" change\n"), "the commit: {prompt}");
    // The same change is not reviewed twice.
    let again = world.sykli(&["review"], &[]);
    exit(&again, 0);
    assert_eq!(text(&again.stdout), text(&out.stdout));
    assert_eq!(world.reviews(), 1);

    let out = world.sykli(&["land"], &[]);
    exit(&out, 0);
    let head = world.head();
    assert!(
        text(&out.stdout).contains(&format!("land: {} is on origin's main", &head[..12])),
        "{out:?}"
    );
    assert_eq!(world.remote_main(), head);
    // Landed, there is nothing left to land or to review.
    git(&world.work, &["fetch", "--quiet", "origin"]);
    let out = world.sykli(&["land"], &[]);
    exit(&out, 1);
    assert!(text(&out.stderr).contains("nothing to land"), "{out:?}");
    let out = world.sykli(&["review"], &[]);
    exit(&out, 0);
    assert!(text(&out.stdout).contains("nothing to review"), "{out:?}");

    // Another clone has run nothing: it reads both from the remote, and the
    // receipt verifies there for the commit that landed.
    git(&world.dir, &["clone", "--quiet", "origin.git", "other"]);
    let other = world.dir.join("other");
    git(
        &other,
        &[
            "fetch",
            "--quiet",
            "origin",
            "refs/notes/sykli/*:refs/notes/sykli/*",
        ],
    );
    let tree = git(&other, &["rev-parse", "HEAD^{tree}"]);
    let review = git(
        &other,
        &["notes", "--ref", "refs/notes/sykli/review", "show", &tree],
    );
    let review: serde_json::Value = serde_json::from_str(&review).unwrap();
    assert_eq!(review["schema"], "sykli-review.v1");
    assert_eq!(review["level"], "fine");
    assert_eq!(review["head"], head.as_str());
    assert_eq!(review["base"], before.as_str());
    let receipt = git(
        &other,
        &["notes", "--ref", "refs/notes/sykli/receipt", "show", &tree],
    );
    let shared = world.dir.join("shared-receipt.json");
    fs::write(&shared, receipt).unwrap();
    let out = world.sykli_in(
        &other,
        &[
            "verify",
            shared.to_str().unwrap(),
            "--contract",
            "sykli.json",
            "--commit",
            &head,
        ],
        &[],
    );
    exit(&out, 0);
    world.done();
}

/// A worktree is removed and the commit checked out in another: the receipt
/// file is gone, the copy its run kept is not. It is what gets shared, and
/// the hook, which reads receipts from the worktree, still lets the push by.
#[test]
fn the_receipt_a_run_kept_lands_once_the_worktrees_own_is_gone() {
    let world = World::new("kept", "ok two\n");
    exit(&world.sykli(&["hook", "install", "--pre-push"], &[]), 0);
    exit(&world.sykli(&["run", "sykli.json"], &[]), 0);
    exit(&world.sykli(&["review"], &[]), 0);
    fs::remove_dir_all(world.work.join(".sykli")).unwrap();

    let out = world.sykli(&["land"], &[]);
    exit(&out, 0);
    assert_eq!(world.remote_main(), world.head());
    let tree = git(&world.work, &["rev-parse", "HEAD^{tree}"]);
    let receipt = git(
        &world.dir.join("origin.git"),
        &["notes", "--ref", "refs/notes/sykli/receipt", "show", &tree],
    );
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    assert_eq!(receipt["subject"]["tree_oid"], tree.as_str());
    world.done();
}

#[test]
fn a_blocking_review_stops_the_landing_and_no_pipe_overrules_it() {
    let world = World::new("blocking", "ok two\n");
    exit(&world.sykli(&["run", "sykli.json"], &[]), 0);
    let before = world.remote_main();
    let reply = [(
        "FAKE_REPLY",
        "- [blocking] file.txt:1: wrong.\\nREVIEW: fine",
    )];

    // Raised to its worst finding, whatever the verdict line said.
    let out = world.sykli(&["review"], &reply);
    exit(&out, 1);
    let said = text(&out.stdout);
    assert!(
        said.contains("review: blocking (1 blocking, 0 to look at) by codex"),
        "{said}"
    );
    assert!(said.contains("  [blocking] file.txt:1: wrong."), "{said}");
    let out = world.sykli(&["land"], &[]);
    exit(&out, 1);
    assert!(text(&out.stderr).contains("blocking"), "{out:?}");

    // This test has no terminal: whoever asks is not a person at one.
    let out = world.sykli(&["review", "--overrule", "accepted"], &[]);
    exit(&out, 2);
    assert!(
        text(&out.stderr).contains("only a person overrules"),
        "{out:?}"
    );
    exit(&world.sykli(&["land"], &[]), 1);
    assert_eq!(world.remote_main(), before);
    world.done();
}

#[test]
fn a_reviewer_that_fails_or_gives_no_verdict_is_no_review() {
    let world = World::new("none", "ok two\n");
    exit(&world.sykli(&["run", "sykli.json"], &[]), 0);

    let out = world.sykli(&["review"], &[("FAKE_EXIT", "3")]);
    exit(&out, 2);
    let said = text(&out.stderr);
    assert!(said.contains("no review happened"), "{said}");
    assert!(
        said.contains("the reviewer's own chatter"),
        "a reviewer that failed is shown: {said}"
    );
    let out = world.sykli(&["review"], &[("FAKE_REPLY", "Looks good to me.")]);
    exit(&out, 2);
    assert!(text(&out.stderr).contains("no `REVIEW:`"), "{out:?}");
    let out = world.sykli(&["review"], &[("FAKE_REPLY", "REVIEW: approved")]);
    exit(&out, 2);

    // None of them is kept as a review: nothing lands on it.
    let out = world.sykli(&["land"], &[]);
    exit(&out, 1);
    assert!(text(&out.stderr).contains("not reviewed"), "{out:?}");

    // Uncommitted work is not what a commit's review or landing is of.
    fs::write(world.work.join("stray.txt"), "x\n").unwrap();
    let out = world.sykli(&["review"], &[]);
    exit(&out, 2);
    assert!(text(&out.stderr).contains("commit first"), "{out:?}");
    let out = world.sykli(&["land"], &[]);
    exit(&out, 1);
    assert!(text(&out.stderr).contains("commit first"), "{out:?}");
    assert_eq!(world.reviews(), 3);
    world.done();
}

#[test]
fn a_failed_run_is_not_proven() {
    let world = World::new("failed", "bad\n");
    exit(&world.sykli(&["run", "sykli.json"], &[]), 1);
    exit(&world.sykli(&["review"], &[]), 0);
    let before = world.remote_main();
    let out = world.sykli(&["land"], &[]);
    exit(&out, 1);
    assert!(text(&out.stderr).contains("not proven"), "{out:?}");
    assert_eq!(world.remote_main(), before);
    world.done();
}

/// A clone that never ran a tree lands it on the receipt another clone
/// shared, once that receipt verifies there: the tree comes back as a new
/// commit after `main` moved on, and the hook lets the push by.
#[test]
fn a_shared_receipt_lands_a_tree_this_clone_never_ran() {
    let world = World::new("shared", "ok two\n");
    exit(&world.sykli(&["run", "sykli.json"], &[]), 0);
    exit(&world.sykli(&["review"], &[]), 0);
    exit(&world.sykli(&["land"], &[]), 0);
    let tree = git(&world.work, &["rev-parse", "HEAD^{tree}"]);

    git(&world.dir, &["clone", "--quiet", "origin.git", "other"]);
    let other = world.dir.join("other");
    identify(&other);
    fs::write(other.join("file.txt"), "ok three\n").unwrap();
    commit(&other, "moved on");
    git(&other, &["push", "--quiet", "origin", "HEAD:main"]);
    git(&other, &["fetch", "--quiet", "origin"]);

    // A clone where a run of that tree failed keeps its own word: what
    // another clone shared does not outrank a failure recorded here.
    git(&world.dir, &["clone", "--quiet", "origin.git", "third"]);
    let third = world.dir.join("third");
    identify(&third);
    git(&third, &["checkout", "--quiet", "-b", "again"]);
    fs::write(third.join("file.txt"), "ok two\n").unwrap();
    commit(&third, "two again");
    assert_eq!(git(&third, &["rev-parse", "HEAD^{tree}"]), tree);
    fs::create_dir_all(third.join(".sykli")).unwrap();
    fs::write(third.join(".sykli/fail"), "").unwrap();
    exit(&world.sykli_in(&third, &["run", "sykli.json"], &[]), 1);
    fs::remove_file(third.join(".sykli/fail")).unwrap();
    exit(&world.sykli_in(&third, &["review"], &[]), 0);
    let moved_on = world.remote_main();
    let out = world.sykli_in(&third, &["land"], &[]);
    exit(&out, 1);
    assert!(text(&out.stderr).contains("sykli says failed"), "{out:?}");
    assert_eq!(world.remote_main(), moved_on);

    git(&other, &["checkout", "--quiet", "-b", "back"]);
    fs::write(other.join("file.txt"), "ok two\n").unwrap();
    let back = commit(&other, "back to two");
    assert_eq!(git(&other, &["rev-parse", "HEAD^{tree}"]), tree);
    exit(
        &world.sykli_in(&other, &["hook", "install", "--pre-push"], &[]),
        0,
    );
    exit(&world.sykli_in(&other, &["review"], &[]), 0);

    let out = world.sykli_in(&other, &["land"], &[]);
    exit(&out, 0);
    assert_eq!(world.remote_main(), back);

    // A receipt that does not verify here is nobody's proof: the same tree
    // again, with a shared receipt that is not a receipt.
    fs::write(other.join("file.txt"), "ok four\n").unwrap();
    commit(&other, "four");
    git(
        &other,
        &["push", "--quiet", "--no-verify", "origin", "HEAD:main"],
    );
    git(&other, &["fetch", "--quiet", "origin"]);
    fs::write(other.join("file.txt"), "ok five\n").unwrap();
    commit(&other, "five");
    let five = git(&other, &["rev-parse", "HEAD^{tree}"]);
    let forged = git(&other, &["hash-object", "-w", "sykli.json"]);
    git(
        &other,
        &[
            "notes",
            "--ref",
            "refs/notes/sykli/receipt",
            "add",
            "-f",
            "-C",
            &forged,
            &five,
        ],
    );
    exit(&world.sykli_in(&other, &["review"], &[]), 0);
    let before = world.remote_main();
    let out = world.sykli_in(&other, &["land"], &[]);
    exit(&out, 1);
    assert!(text(&out.stderr).contains("does not verify"), "{out:?}");
    assert_eq!(world.remote_main(), before);
    world.done();
}

#[test]
fn review_and_land_say_what_they_take() {
    for (command, flags) in [
        ("review", vec!["--remote", "--branch", "--overrule"]),
        ("land", vec!["--remote", "--branch"]),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
            .args([command, "--help"])
            .output()
            .unwrap();
        exit(&out, 0);
        let help = text(&out.stdout);
        for flag in flags {
            assert!(help.contains(flag), "{command} --help: {help}");
        }
        assert!(help.contains("Exit codes:"), "{command} --help: {help}");
    }
}
