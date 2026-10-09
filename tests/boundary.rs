//! The Boundary: a receipt lets a commit through only when it is a clean,
//! passing run of exactly that commit's tree.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

/// A git repository with `sykli.json` committed (and locked) as its only
/// content besides `file.txt`.
fn repo(name: &str, contract: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("sykli-boundary-{name}-{nonce}"));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("sykli.json"), contract).unwrap();
    fs::write(root.join("file.txt"), "one\n").unwrap();
    git(&root, &["init", "--quiet", "--initial-branch=main"]);
    assert!(sykli(&root, &["lock", "sykli.json"]).status.success());
    commit(&root, "fixture");
    root
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

fn commit(root: &Path, message: &str) -> String {
    git(root, &["add", "--all"]);
    git(root, &["commit", "--quiet", "--no-verify", "-m", message]);
    git(root, &["rev-parse", "HEAD"])
}

fn sykli(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(args)
        .current_dir(root)
        .output()
        .unwrap()
}

/// Run the graph and keep the receipt outside the repository.
fn receipt(root: &Path) -> (PathBuf, serde_json::Value) {
    let out = sykli(root, &["run", "sykli.json", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let path = root.with_extension(format!("receipt-{}.json", value["started_at_ms"]));
    fs::write(&path, &out.stdout).unwrap();
    (path, value)
}

fn verify_commit(root: &Path, receipt: &Path, rev: &str) -> Output {
    sykli(
        root,
        &[
            "verify",
            receipt.to_str().unwrap(),
            "--contract",
            "sykli.json",
            "--commit",
            rev,
        ],
    )
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

const PASSING: &str =
    r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"true","inputs":["file.txt"]}]}"#;

#[test]
fn verify_commit_accepts_a_clean_passing_run_of_that_commit() {
    let root = repo("accept", PASSING);
    let head = git(&root, &["rev-parse", "HEAD"]);
    let (path, value) = receipt(&root);
    assert_eq!(value["subject"]["dirty"], false);
    for rev in [head.as_str(), "HEAD", "main"] {
        let out = verify_commit(&root, &path, rev);
        assert_eq!(out.status.code(), Some(0), "{rev}: {}", text(&out.stderr));
    }
    // A cached re-run is a pass too.
    let (cached, value) = receipt(&root);
    assert_eq!(value["outcome"], "cached");
    assert_eq!(verify_commit(&root, &cached, "HEAD").status.code(), Some(0));
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(cached);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn verify_commit_refuses_a_receipt_of_another_commit() {
    let root = repo("other", PASSING);
    fs::write(root.join("file.txt"), "two\n").unwrap();
    commit(&root, "second");
    let (path, _) = receipt(&root);
    // The receipt matches the tree on disk, so plain verify passes ...
    assert_eq!(
        sykli(
            &root,
            &["verify", path.to_str().unwrap(), "--contract", "sykli.json"]
        )
        .status
        .code(),
        Some(0)
    );
    // ... but it is not a run of the parent commit.
    let out = verify_commit(&root, &path, "HEAD~1");
    assert_ne!(out.status.code(), Some(0));
    let reason = text(&out.stderr);
    assert_eq!(
        reason.trim().lines().count(),
        1,
        "one-line reason: {reason}"
    );
    assert!(reason.contains("commit"), "{reason}");
    let _ = fs::remove_file(path);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn verify_commit_refuses_a_dirty_run_even_of_the_same_tree() {
    let root = repo("dirty", PASSING);
    // Run with an uncommitted edit, then commit exactly that edit: the
    // receipt's tree is the new commit's tree, but the run was not clean.
    fs::write(root.join("file.txt"), "two\n").unwrap();
    let (path, value) = receipt(&root);
    assert_eq!(value["subject"]["dirty"], true);
    commit(&root, "commit what ran");
    assert_eq!(
        sykli(
            &root,
            &["verify", path.to_str().unwrap(), "--contract", "sykli.json"]
        )
        .status
        .code(),
        Some(0),
        "plain verify still accepts a dirty receipt for the tree on disk"
    );
    let out = verify_commit(&root, &path, "HEAD");
    assert_ne!(out.status.code(), Some(0));
    let reason = text(&out.stderr);
    assert_eq!(
        reason.trim().lines().count(),
        1,
        "one-line reason: {reason}"
    );
    assert!(reason.contains("dirty"), "{reason}");

    // Relabelling it clean is not enough: HEAD's tree at the run disagrees.
    let mut forged = value.clone();
    forged["subject"]["dirty"] = false.into();
    fs::write(&path, forged.to_string()).unwrap();
    let out = verify_commit(&root, &path, "HEAD");
    assert_ne!(out.status.code(), Some(0), "{}", text(&out.stdout));
    let _ = fs::remove_file(path);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn verify_commit_refuses_a_failed_run() {
    let root = repo(
        "failed",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"false","inputs":["file.txt"]}]}"#,
    );
    let (path, value) = receipt(&root);
    assert_eq!(value["outcome"], "failed");
    assert_eq!(value["subject"]["dirty"], false);
    let out = verify_commit(&root, &path, "HEAD");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("outcome"),
        "{}",
        text(&out.stderr)
    );
    let _ = fs::remove_file(path);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn verify_commit_cannot_verify_an_unknown_revision() {
    let root = repo("unknown", PASSING);
    let (path, _) = receipt(&root);
    let out = verify_commit(&root, &path, "no-such-branch");
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("no-such-branch"),
        "{}",
        text(&out.stderr)
    );
    let _ = fs::remove_file(path);
    fs::remove_dir_all(root).unwrap();
}

/// The task reads `file.txt`, so it passes only while that file says `one`.
const READS_FILE: &str = r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"test \"$(cat file.txt)\" = one","inputs":["file.txt"]}]}"#;

#[test]
fn pre_commit_refuses_when_the_disk_is_not_what_is_staged() {
    let root = repo("staged", READS_FILE);
    // Stage a broken line, then put the disk back as HEAD has it: the commit
    // would carry the broken line, while the files on disk still pass.
    fs::write(root.join("file.txt"), "broken\n").unwrap();
    git(&root, &["add", "file.txt"]);
    fs::write(root.join("file.txt"), "one\n").unwrap();
    assert_eq!(
        sykli(&root, &["run", "sykli.json"]).status.code(),
        Some(0),
        "a run checks the disk, which passes"
    );
    let out = sykli(&root, &["hook", "pre-commit"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let message = text(&out.stderr);
    assert!(message.contains("file.txt"), "{message}");
    assert!(message.contains("not staged"), "{message}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pre_commit_refuses_untracked_files_the_commit_would_not_carry() {
    let root = repo("untracked", READS_FILE);
    fs::write(root.join("new.txt"), "scratch\n").unwrap();
    let out = sykli(&root, &["hook", "pre-commit"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("new.txt"),
        "{}",
        text(&out.stderr)
    );
    // Ignored files are not part of any tree, and `.sykli/` is sykli's own.
    fs::write(root.join(".gitignore"), "new.txt\n").unwrap();
    git(&root, &["add", ".gitignore"]);
    let out = sykli(&root, &["hook", "pre-commit"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pre_commit_runs_the_graph_on_exactly_what_is_staged() {
    let root = repo("gate", READS_FILE);
    // Staged and on disk alike: the commit's tree is what runs.
    fs::write(root.join("file.txt"), "broken\n").unwrap();
    git(&root, &["add", "file.txt"]);
    let out = sykli(&root, &["hook", "pre-commit"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("refusing"),
        "{}",
        text(&out.stderr)
    );

    fs::write(root.join("file.txt"), "one\n").unwrap();
    fs::write(root.join("other.txt"), "more\n").unwrap();
    git(&root, &["add", "--all"]);
    let out = sykli(&root, &["hook", "pre-commit"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pre_commit_passes_where_there_is_no_contract() {
    let root = repo("nocontract", PASSING);
    git(&root, &["rm", "--quiet", "sykli.json", "sykli.lock"]);
    let out = sykli(&root, &["hook", "pre-commit"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("no sykli.json"),
        "{}",
        text(&out.stderr)
    );
    fs::remove_dir_all(root).unwrap();
}

const ZEROS: &str = "0000000000000000000000000000000000000000";

/// Feed git's pre-push lines (`<local ref> <local sha> <remote ref> <remote sha>`).
fn pre_push(root: &Path, remote: &str, lines: &[String]) -> Output {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["hook", "pre-push", remote, "/nowhere/origin.git"])
        .current_dir(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for line in lines {
        writeln!(stdin, "{line}").unwrap();
    }
    drop(stdin);
    child.wait_with_output().unwrap()
}

fn update(sha: &str, remote_ref: &str) -> String {
    format!("refs/heads/work {sha} {remote_ref} {ZEROS}")
}

#[test]
fn pre_push_to_main_requires_a_clean_passing_receipt_of_the_pushed_commit() {
    let root = repo("push", READS_FILE);
    let head = git(&root, &["rev-parse", "HEAD"]);
    let out = pre_push(&root, "origin", &[update(&head, "refs/heads/main")]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("sykli run sykli.json"),
        "the refusal names the command that produces a receipt: {}",
        text(&out.stderr)
    );

    assert_eq!(sykli(&root, &["run", "sykli.json"]).status.code(), Some(0));
    let out = pre_push(&root, "origin", &[update(&head, "refs/heads/main")]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));

    // `.sykli/receipt.json`, where the documented gate writes, counts too.
    let receipt = sykli(&root, &["run", "sykli.json", "--json"]);
    fs::remove_dir_all(root.join(".sykli/receipts")).unwrap();
    assert_eq!(
        pre_push(&root, "origin", &[update(&head, "refs/heads/main")])
            .status
            .code(),
        Some(1)
    );
    fs::write(root.join(".sykli/receipt.json"), receipt.stdout).unwrap();
    let out = pre_push(&root, "origin", &[update(&head, "refs/heads/main")]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pre_push_refuses_a_failing_or_dirty_commit_on_main() {
    let root = repo("pushfail", READS_FILE);
    // A failing commit, with its honest receipt.
    fs::write(root.join("file.txt"), "broken\n").unwrap();
    let broken = commit(&root, "broken");
    assert_eq!(sykli(&root, &["run", "sykli.json"]).status.code(), Some(1));
    let out = pre_push(&root, "origin", &[update(&broken, "refs/heads/main")]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("outcome"),
        "{}",
        text(&out.stderr)
    );

    // A passing tree that only ever ran uncommitted.
    fs::write(root.join("file.txt"), "one\n").unwrap();
    assert_eq!(sykli(&root, &["run", "sykli.json"]).status.code(), Some(0));
    let fixed = commit(&root, "fixed");
    let out = pre_push(&root, "origin", &[update(&fixed, "refs/heads/main")]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("dirty"), "{}", text(&out.stderr));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pre_push_gates_only_the_default_branch() {
    let root = repo("pushbranch", READS_FILE);
    let head = git(&root, &["rev-parse", "HEAD"]);
    let feature = update(&head, "refs/heads/feature");
    let delete = format!("(delete) {ZEROS} refs/heads/main {head}");
    let out = pre_push(&root, "origin", &[feature, delete]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    // Without the remote's HEAD, main and master are the default branch.
    let master = pre_push(&root, "origin", &[update(&head, "refs/heads/master")]);
    assert_eq!(master.status.code(), Some(1));
    // With it, the remote's HEAD names the one default branch.
    git(&root, &["update-ref", "refs/remotes/origin/trunk", &head]);
    git(
        &root,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ],
    );
    let trunk = pre_push(&root, "origin", &[update(&head, "refs/heads/trunk")]);
    assert_eq!(trunk.status.code(), Some(1), "{}", text(&trunk.stderr));
    let main = pre_push(&root, "origin", &[update(&head, "refs/heads/main")]);
    assert_eq!(main.status.code(), Some(0), "{}", text(&main.stderr));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pre_push_passes_where_there_is_no_contract() {
    let root = repo("pushnocontract", PASSING);
    git(&root, &["rm", "--quiet", "sykli.json", "sykli.lock"]);
    let head = commit(&root, "no contract");
    let out = pre_push(&root, "origin", &[update(&head, "refs/heads/main")]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let note = text(&out.stderr);
    assert_eq!(note.trim().lines().count(), 1, "{note}");
    assert!(note.contains("no sykli.json"), "{note}");
    fs::remove_dir_all(root).unwrap();
}

fn hooks_dir(root: &Path) -> PathBuf {
    root.join(".git/hooks")
}

#[test]
fn install_writes_thin_hooks_every_worktree_shares() {
    let root = repo("install", PASSING);
    let worktree = root.with_extension("worktree");
    git(
        &root,
        &["worktree", "add", "--quiet", worktree.to_str().unwrap()],
    );
    // Installed from a linked worktree, the hooks land in the common dir.
    let out = sykli(&worktree, &["hook", "install"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    for name in ["pre-commit", "pre-push"] {
        let path = hooks_dir(&root).join(name);
        let body = fs::read_to_string(&path).unwrap();
        assert!(body.starts_with("#!/bin/sh\n"), "{body}");
        assert!(
            body.contains(&format!("exec sykli hook {name} \"$@\"")),
            "{body}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "{name} is executable");
        }
    }
    // Idempotent.
    let before = fs::read(hooks_dir(&root).join("pre-push")).unwrap();
    let again = sykli(&root, &["hook", "install"]);
    assert_eq!(again.status.code(), Some(0), "{}", text(&again.stderr));
    assert_eq!(fs::read(hooks_dir(&root).join("pre-push")).unwrap(), before);
    git(
        &root,
        &["worktree", "remove", "--force", worktree.to_str().unwrap()],
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn install_replaces_a_foreign_hook_only_with_force() {
    let root = repo("foreign", PASSING);
    fs::create_dir_all(hooks_dir(&root)).unwrap();
    let mine = "#!/bin/sh\necho mine\n";
    fs::write(hooks_dir(&root).join("pre-push"), mine).unwrap();
    let out = sykli(&root, &["hook", "install", "--pre-push"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("--force"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(hooks_dir(&root).join("pre-push")).unwrap(),
        mine
    );
    // Only the hook asked for is written.
    assert!(!hooks_dir(&root).join("pre-commit").exists());
    let out = sykli(&root, &["hook", "install", "--pre-push", "--force"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        fs::read_to_string(hooks_dir(&root).join("pre-push"))
            .unwrap()
            .contains("exec sykli hook pre-push")
    );
    assert!(!hooks_dir(&root).join("pre-commit").exists());
    fs::remove_dir_all(root).unwrap();
}

/// Installed hooks, real `git commit` and `git push`, a bare origin, and the
/// binary under test as the `sykli` on PATH.
#[cfg(unix)]
#[test]
fn installed_hooks_gate_real_commits_and_pushes() {
    let root = repo("e2e", READS_FILE);
    let origin = root.with_extension("origin.git");
    let bin = Path::new(env!("CARGO_BIN_EXE_sykli")).parent().unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin.to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    // Hooks run: no --no-verify, and no global hooksPath can redirect them.
    let hooked = |args: &[&str]| {
        Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(&root)
            .env("PATH", &path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
    };
    git(
        &root,
        &["init", "--quiet", "--bare", origin.to_str().unwrap()],
    );
    git(
        &root,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    assert!(sykli(&root, &["hook", "install"]).status.success());

    // A broken change staged while the disk still passes: refused.
    fs::write(root.join("file.txt"), "broken\n").unwrap();
    git(&root, &["add", "file.txt"]);
    fs::write(root.join("file.txt"), "one\n").unwrap();
    let out = hooked(&["commit", "--quiet", "-m", "sneak"]);
    assert!(!out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("not staged"),
        "{}",
        text(&out.stderr)
    );
    git(&root, &["reset", "--quiet"]);

    // A failing commit (made without hooks) cannot reach main.
    fs::write(root.join("file.txt"), "broken\n").unwrap();
    commit(&root, "broken");
    assert_eq!(sykli(&root, &["run", "sykli.json"]).status.code(), Some(1));
    let out = hooked(&["push", "--quiet", "origin", "main"]);
    assert!(!out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("refusing to push"),
        "{}",
        text(&out.stderr)
    );
    // A feature branch is not gated.
    let out = hooked(&["push", "--quiet", "origin", "main:feature"]);
    assert!(out.status.success(), "{}", text(&out.stderr));

    // Fixed through the pre-commit hook, run clean once committed: accepted.
    fs::write(root.join("file.txt"), "one\n").unwrap();
    git(&root, &["add", "file.txt"]);
    let out = hooked(&["commit", "--quiet", "-m", "fixed"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let out = hooked(&["push", "--quiet", "origin", "main"]);
    assert!(!out.status.success(), "the pre-commit run was dirty");
    assert_eq!(sykli(&root, &["run", "sykli.json"]).status.code(), Some(0));
    let out = hooked(&["push", "--quiet", "origin", "main"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        git(&root, &["rev-parse", "HEAD"]),
        git(&origin, &["rev-parse", "main"])
    );
    fs::remove_dir_all(origin).unwrap();
    fs::remove_dir_all(root).unwrap();
}
