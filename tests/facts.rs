//! What a caller may state about the repository instead of sykli asking
//! git: `--repo-root` and `--git-common-dir`. They are trusted only as far
//! as the files on disk bear them out: the root's `.git` must lead to the
//! common git directory. A root that is a subdirectory would leave changes
//! outside it out of the snapshot, so a receipt could pass for a commit
//! whose content never ran.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

const ZEROS: &str = "0000000000000000000000000000000000000000";

/// A task that reads a file above its folder, as `cargo test` reads a
/// workspace's files from a member directory.
const READS_ABOVE: &str = r#"{"schema":"sykli-contract.v1","tasks":[{"name":"check","run":"grep -q s s.txt && grep -q ok ../app.txt","inputs":["s.txt"]}]}"#;

const READS_FILE: &str = r#"{"schema":"sykli-contract.v1","tasks":[{"name":"check","run":"grep -q ok app.txt","inputs":["app.txt"]}]}"#;

struct Scratch {
    base: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("sykli-facts-{name}-{nonce}"));
        fs::create_dir_all(base.join("cfg")).unwrap();
        Scratch { base }
    }

    /// The contract, written and locked, beside the repositories.
    fn contract(&self, text: &str) -> PathBuf {
        let path = self.base.join("cfg/sykli.json");
        fs::write(&path, text).unwrap();
        let out = sykli(&self.base, &["lock", path.to_str().unwrap()], None);
        assert!(out.status.success(), "{}", text_of(&out.stderr));
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn sykli(cwd: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    if let Some(stdin) = stdin {
        input.write_all(stdin.as_bytes()).unwrap();
    }
    drop(input);
    child.wait_with_output().unwrap()
}

fn text_of(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The repository's own answers, as a caller would pass them.
fn facts(root: &Path) -> (String, String) {
    let answer = git(
        root,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-common-dir",
        ],
    );
    let mut lines = answer.lines();
    (
        lines.next().unwrap().to_string(),
        lines.next().unwrap().to_string(),
    )
}

/// The review's reproduction: commit X carries a failing `app.txt`; with
/// `--repo-root` naming a subdirectory, an uncommitted fix outside it made a
/// passing receipt with X's tree and `dirty: false`, and the hook let X
/// through. A root whose `.git` does not lead to the common directory is
/// refused before anything runs.
#[test]
fn a_subdirectory_given_as_the_repository_root_is_refused() {
    let scratch = Scratch::new("subdir");
    let contract = scratch.contract(READS_ABOVE);
    let contract = contract.to_str().unwrap();
    let repo = scratch.base.join("r");
    fs::create_dir_all(repo.join("sub")).unwrap();
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(repo.join("app.txt"), "bad\n").unwrap();
    fs::write(repo.join("sub/s.txt"), "s\n").unwrap();
    fs::write(repo.join(".gitignore"), ".sykli/\n").unwrap();
    git(&repo, &["add", "--all"]);
    git(&repo, &["commit", "--quiet", "-m", "X: app.txt is bad"]);
    let x = git(&repo, &["rev-parse", "HEAD"]);
    let (_, common) = facts(&repo);
    let sub = repo.join("sub");
    let given = [
        "--repo-root",
        sub.to_str().unwrap(),
        "--git-common-dir",
        &common,
    ];

    fs::write(repo.join("app.txt"), "ok\n").unwrap();
    let mut run = vec!["run", contract, "--json"];
    run.extend(given);
    let out = sykli(&sub, &run, None);
    assert_eq!(out.status.code(), Some(2), "{}", text_of(&out.stdout));
    let message = text_of(&out.stderr);
    assert!(message.contains("--repo-root"), "{message}");
    assert!(message.contains("--git-common-dir"), "{message}");
    assert!(
        !repo.join("sub/.sykli").exists(),
        "no receipt was written for the subdirectory"
    );
    git(&repo, &["checkout", "--quiet", "app.txt"]);

    // Nor can the hook be told that root.
    let mut push = vec![
        "hook",
        "pre-push",
        "--contract",
        contract,
        "--branch",
        "main",
    ];
    push.extend(given);
    push.extend(["origin", "/nowhere/origin.git"]);
    let line = format!("refs/heads/main {x} refs/heads/main {ZEROS}\n");
    let out = sykli(&repo, &push, Some(&line));
    assert_eq!(out.status.code(), Some(2), "{}", text_of(&out.stderr));

    // Another repository's common directory is refused the same way.
    let other = scratch.base.join("other");
    fs::create_dir_all(&other).unwrap();
    git(&other, &["init", "--quiet"]);
    let (_, other_common) = facts(&other);
    let root = repo.to_str().unwrap();
    let out = sykli(
        &repo,
        &[
            "run",
            contract,
            "--repo-root",
            root,
            "--git-common-dir",
            &other_common,
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(2), "{}", text_of(&out.stderr));
}

/// The facts as git gives them pass, in a main checkout (its `.git` is the
/// common directory, also spelled with a trailing slash) and in a linked
/// worktree (its `.git` is a `gitdir:` file, followed through `commondir`).
#[test]
fn the_repositorys_own_root_and_a_linked_worktrees_root_are_accepted() {
    let scratch = Scratch::new("worktree");
    let contract = scratch.contract(READS_FILE);
    let contract = contract.to_str().unwrap();
    let repo = scratch.base.join("r");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(repo.join("app.txt"), "ok\n").unwrap();
    fs::write(repo.join(".gitignore"), ".sykli/\n").unwrap();
    git(&repo, &["add", "--all"]);
    git(&repo, &["commit", "--quiet", "-m", "ok"]);
    let worktree = scratch.base.join("wt");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "work",
            worktree.to_str().unwrap(),
        ],
    );
    let (root, common) = facts(&repo);
    let (wt_root, wt_common) = facts(&worktree);
    assert_eq!(common, wt_common, "a linked worktree shares the common dir");
    assert!(worktree.join(".git").is_file());

    for (cwd, root, common) in [
        (&repo, root.clone(), common.clone()),
        (&repo, format!("{root}/"), format!("{common}/")),
        (&worktree, wt_root, wt_common),
    ] {
        let given = [
            "--repo-root",
            root.as_str(),
            "--git-common-dir",
            common.as_str(),
        ];
        let mut run = vec!["run", contract, "--json"];
        run.extend(given);
        let out = sykli(cwd, &run, None);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{root}: {}",
            text_of(&out.stderr)
        );
        let receipt = scratch.base.join("receipt.json");
        fs::write(&receipt, &out.stdout).unwrap();
        let head = git(cwd, &["rev-parse", "HEAD"]);
        let mut verify = vec![
            "verify",
            receipt.to_str().unwrap(),
            "--contract",
            contract,
            "--commit",
            &head,
        ];
        verify.extend(given);
        let out = sykli(cwd, &verify, None);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{root}: {}",
            text_of(&out.stdout)
        );
        let mut push = vec![
            "hook",
            "pre-push",
            "--contract",
            contract,
            "--branch",
            "main",
        ];
        push.extend(given);
        push.extend(["origin", "/nowhere/origin.git"]);
        let line = format!("refs/heads/work {head} refs/heads/main {ZEROS}\n");
        let out = sykli(cwd, &push, Some(&line));
        assert_eq!(
            out.status.code(),
            Some(0),
            "{root}: {}",
            text_of(&out.stderr)
        );
    }
}
