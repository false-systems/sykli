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

/// Run sykli with extra environment variables.
fn sykli_env(cwd: &Path, args: &[&str], stdin: Option<&str>, env: &[(&str, &Path)]) -> Output {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(args)
        .envs(env.iter().map(|(name, value)| (*name, *value)))
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

/// The review's reproduction: with the facts given and checked, an exported
/// GIT_WORK_TREE (alone, or with GIT_DIR) made sykli snapshot another
/// directory, a clean copy of commit X, while the tasks ran in the given
/// root, where an uncommitted edit made them pass. The receipt said X's
/// tree, clean, and the hook let X through. The given repository is what
/// sykli's git commands look at, whatever the environment names.
#[test]
fn exported_git_locations_do_not_steer_a_given_repository() {
    let scratch = Scratch::new("envgit");
    let contract = scratch.contract(READS_FILE);
    let contract = contract.to_str().unwrap();
    let repo = scratch.base.join("m");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(repo.join("app.txt"), "bad\n").unwrap();
    fs::write(repo.join(".gitignore"), ".sykli/\n").unwrap();
    git(&repo, &["add", "--all"]);
    git(&repo, &["commit", "--quiet", "-m", "X: bad"]);
    let x = git(&repo, &["rev-parse", "HEAD"]);
    let x_tree = git(&repo, &["rev-parse", "HEAD^{tree}"]);
    // A clean copy of X elsewhere, and a passing edit in the checkout.
    let copy = scratch.base.join("copy");
    fs::create_dir_all(&copy).unwrap();
    fs::write(copy.join("app.txt"), "bad\n").unwrap();
    fs::write(copy.join(".gitignore"), ".sykli/\n").unwrap();
    fs::write(repo.join("app.txt"), "ok\n").unwrap();
    let (root, common) = facts(&repo);
    let given = [
        "--repo-root",
        root.as_str(),
        "--git-common-dir",
        common.as_str(),
    ];
    let dot_git = repo.join(".git");
    let mut run = vec!["run", contract, "--json"];
    run.extend(given);
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
    for env in [
        vec![("GIT_WORK_TREE", copy.as_path())],
        vec![
            ("GIT_DIR", dot_git.as_path()),
            ("GIT_WORK_TREE", copy.as_path()),
        ],
    ] {
        let _ = fs::remove_dir_all(repo.join(".sykli"));
        let out = sykli_env(&repo, &run, None, &env);
        let receipt: serde_json::Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|_| panic!("{env:?}: {}", text_of(&out.stderr)));
        // The snapshot is of the checkout that ran: not X's tree, and dirty.
        assert_ne!(receipt["subject"]["tree_oid"], x_tree.as_str(), "{env:?}");
        assert_eq!(receipt["subject"]["dirty"], true, "{env:?}");
        let out = sykli(&repo, &push, Some(&line));
        assert_eq!(
            out.status.code(),
            Some(1),
            "{env:?}: {}",
            text_of(&out.stderr)
        );
    }
}

/// What git itself exports still works with the facts given: GIT_DIR naming
/// the given repository, in a main checkout.
#[test]
fn a_git_dir_naming_the_given_repository_still_works() {
    let scratch = Scratch::new("gitdir");
    let contract = scratch.contract(READS_FILE);
    let contract = contract.to_str().unwrap();
    let repo = scratch.base.join("r");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(repo.join("app.txt"), "ok\n").unwrap();
    fs::write(repo.join(".gitignore"), ".sykli/\n").unwrap();
    git(&repo, &["add", "--all"]);
    git(&repo, &["commit", "--quiet", "-m", "ok"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]);
    let (root, common) = facts(&repo);
    let given = [
        "--repo-root",
        root.as_str(),
        "--git-common-dir",
        common.as_str(),
    ];
    let dot_git = repo.join(".git");
    let env = [("GIT_DIR", dot_git.as_path())];
    let mut run = vec!["run", contract, "--json"];
    run.extend(given);
    let out = sykli_env(&repo, &run, None, &env);
    assert_eq!(out.status.code(), Some(0), "{}", text_of(&out.stderr));
    let receipt: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(receipt["subject"]["tree_oid"], tree.as_str());
    assert_eq!(receipt["subject"]["dirty"], false);
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
    let line = format!("refs/heads/main {head} refs/heads/main {ZEROS}\n");
    let out = sykli_env(&repo, &push, Some(&line), &env);
    assert_eq!(out.status.code(), Some(0), "{}", text_of(&out.stderr));
}

/// Real hooks with the facts given: a push from a linked worktree (git
/// exports GIT_DIR to its hooks) is refused without a receipt and accepted
/// with one, and `git commit -a` (git exports GIT_INDEX_FILE) passes the
/// pre-commit hook or refuses a failing commit.
#[cfg(unix)]
#[test]
fn hooks_git_runs_in_a_linked_worktree_keep_working_with_the_facts_given() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new("hooks");
    let contract = scratch.contract(READS_FILE);
    let repo = scratch.base.join("r");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(repo.join("app.txt"), "ok\n").unwrap();
    fs::write(repo.join(".gitignore"), ".sykli/\n").unwrap();
    git(&repo, &["add", "--all"]);
    git(&repo, &["commit", "--quiet", "-m", "ok"]);
    let origin = scratch.base.join("origin.git");
    git(
        &repo,
        &["init", "--quiet", "--bare", origin.to_str().unwrap()],
    );
    git(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
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
    let (root, common) = facts(&worktree);
    let hooks = scratch.base.join("hooks");
    fs::create_dir_all(&hooks).unwrap();
    for (hook, args) in [
        (
            "pre-push",
            format!("--contract '{}' --branch main", contract.display()),
        ),
        ("pre-commit", format!("--contract '{}'", contract.display())),
    ] {
        let path = hooks.join(hook);
        fs::write(
            &path,
            format!(
                "#!/bin/sh\n[ -n \"$GIT_DIR\" ] || {{ echo 'no GIT_DIR exported' >&2; exit 3; }}\n\
                 exec '{}' hook {hook} {args} --repo-root '{root}' --git-common-dir '{common}' \"$@\"\n",
                env!("CARGO_BIN_EXE_sykli"),
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(
        &repo,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    let hooked = |args: &[&str]| {
        Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(&worktree)
            .env("GIT_CONFIG_GLOBAL", scratch.base.join("no-gitconfig"))
            .output()
            .unwrap()
    };

    // A commit made with `commit -a`: the hook runs on git's own index.
    fs::write(worktree.join("app.txt"), "ok, still\n").unwrap();
    let out = hooked(&["commit", "--quiet", "-a", "-m", "passing"]);
    assert!(out.status.success(), "{}", text_of(&out.stderr));
    fs::write(worktree.join("app.txt"), "bad\n").unwrap();
    let out = hooked(&["commit", "--quiet", "-a", "-m", "failing"]);
    assert!(!out.status.success(), "{}", text_of(&out.stderr));
    git(&worktree, &["checkout", "--quiet", "app.txt"]);

    // The pre-commit run is the passing commit's evidence for the push.
    let out = hooked(&["push", "--quiet", "origin", "work:main"]);
    assert!(out.status.success(), "{}", text_of(&out.stderr));
    // A commit with no receipt is refused.
    fs::write(worktree.join("other.txt"), "x\n").unwrap();
    git(&worktree, &["add", "other.txt"]);
    git(
        &worktree,
        &["commit", "--quiet", "--no-verify", "-m", "no receipt"],
    );
    let out = hooked(&["push", "--quiet", "origin", "work:main"]);
    assert!(!out.status.success(), "{}", text_of(&out.stderr));
    assert!(
        text_of(&out.stderr).contains("refusing to push"),
        "{}",
        text_of(&out.stderr)
    );
}
