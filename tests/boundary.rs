//! The Boundary: a receipt lets a commit through only when it is a clean,
//! passing run of exactly that commit's tree.

#[path = "support/stdio.rs"]
mod stdio;

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
fn verify_commit_accepts_a_dirty_run_of_the_same_tree() {
    let root = repo("dirty", PASSING);
    // Run with an uncommitted edit, then commit exactly that edit: the
    // receipt's tree is the new commit's tree, so the run checked exactly
    // what the commit carries, though HEAD had not moved yet.
    fs::write(root.join("file.txt"), "two\n").unwrap();
    let (path, value) = receipt(&root);
    assert_eq!(value["subject"]["dirty"], true);
    commit(&root, "commit what ran");
    let out = verify_commit(&root, &path, "HEAD");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));

    // The tree still binds it: the same dirty receipt is no evidence for
    // the commit before, whose tree differs.
    let out = verify_commit(&root, &path, "HEAD~1");
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stdout));
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
    assert!(
        message.contains("differs from what this commit carries"),
        "{message}"
    );
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
    let input: String = lines.iter().map(|line| format!("{line}\n")).collect();
    stdio::output(
        Command::new(env!("CARGO_BIN_EXE_sykli"))
            .args(["hook", "pre-push", remote, "/nowhere/origin.git"])
            .current_dir(root),
        input.as_bytes(),
    )
    .unwrap()
}

fn update(sha: &str, remote_ref: &str) -> String {
    format!("refs/heads/work {sha} {remote_ref} {ZEROS}")
}

#[test]
fn pre_push_malformed_object_ids_are_errors_not_panics() {
    let root = repo("push-malformed-oid", READS_FILE);
    for bad in ["aaaaaaaaaaa€", "abc", "HEAD"] {
        let out = pre_push(&root, "origin", &[update(bad, "refs/heads/main")]);
        assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
        assert!(text(&out.stderr).contains("unexpected pre-push input line"));
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pre_push_to_main_requires_a_passing_receipt_of_the_pushed_commit() {
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
fn pre_push_refuses_a_failing_commit_on_main() {
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

    // A passing tree that only ever ran uncommitted: the run checked exactly
    // what the commit carries, so it counts.
    fs::write(root.join("file.txt"), "one\n").unwrap();
    assert_eq!(sykli(&root, &["run", "sykli.json"]).status.code(), Some(0));
    let fixed = commit(&root, "fixed");
    let out = pre_push(&root, "origin", &[update(&fixed, "refs/heads/main")]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
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
    // The replaced hook is kept beside it, and the output says where.
    let kept = hooks_dir(&root).join("pre-push.pre-sykli");
    assert_eq!(fs::read_to_string(&kept).unwrap(), mine);
    assert!(
        text(&out.stdout).contains("pre-push.pre-sykli"),
        "{}",
        text(&out.stdout)
    );
    // A sykli hook someone has edited is not rewritten without a word.
    let edited = format!(
        "{}echo also mine\n",
        fs::read_to_string(hooks_dir(&root).join("pre-push")).unwrap()
    );
    fs::write(hooks_dir(&root).join("pre-push"), &edited).unwrap();
    let out = sykli(&root, &["hook", "install", "--pre-push"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("edited"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(hooks_dir(&root).join("pre-push")).unwrap(),
        edited
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn install_refuses_when_core_hooks_path_points_elsewhere() {
    let root = repo("hookspath", PASSING);
    git(&root, &["config", "core.hooksPath", "elsewhere"]);
    let out = sykli(&root, &["hook", "install"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("core.hooksPath"),
        "{}",
        text(&out.stderr)
    );
    assert!(!hooks_dir(&root).join("pre-push").exists());
    // Pointing at the directory install writes is fine.
    let default = fs::canonicalize(root.join(".git")).unwrap().join("hooks");
    git(
        &root,
        &["config", "core.hooksPath", default.to_str().unwrap()],
    );
    let out = sykli(&root, &["hook", "install"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
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
        text(&out.stderr).contains("differs from what this commit carries"),
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

    // Fixed through the pre-commit hook: its run is the commit's evidence,
    // so the push is accepted without a second run.
    fs::write(root.join("file.txt"), "one\n").unwrap();
    git(&root, &["add", "file.txt"]);
    let out = hooked(&["commit", "--quiet", "-m", "fixed"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let out = hooked(&["push", "--quiet", "origin", "main"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        git(&root, &["rev-parse", "HEAD"]),
        git(&origin, &["rev-parse", "main"])
    );
    fs::remove_dir_all(origin).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ci_checks_out_the_pull_request_head_not_the_merge_ref() {
    let ci =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/ci.yml"))
            .unwrap();
    let checkouts = ci.matches("uses: actions/checkout@").count();
    assert!(checkouts > 0);
    assert_eq!(
        ci.matches("ref: ${{ github.event.pull_request.head.sha || github.sha }}")
            .count(),
        checkouts,
        "every checkout names the pull request head"
    );
}

/// The Action's verify step, run as the Action runs it, with this build.
#[cfg(unix)]
#[test]
fn the_action_verifies_the_receipt_against_the_pull_request_head() {
    let root = repo("action", PASSING);
    let head = git(&root, &["rev-parse", "HEAD"]);
    let action_verify = |receipt: &Path, commit: &str| {
        let output = root.with_extension("github-output");
        let _ = fs::remove_file(&output);
        let out = Command::new("sh")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("action/verify.sh"))
            .current_dir(&root)
            .env("SYKLI_BINARY", env!("CARGO_BIN_EXE_sykli"))
            .env("SYKLI_CONTRACT", "sykli.json")
            .env("SYKLI_RECEIPT", receipt)
            .env("SYKLI_COMMIT", commit)
            .env("GITHUB_OUTPUT", &output)
            .env_remove("GITHUB_STEP_SUMMARY")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
        let code = fs::read_to_string(&output).unwrap();
        let _ = fs::remove_file(&output);
        (code, text(&out.stdout))
    };
    // Checked out at GitHub's merge commit, a clean, passing receipt of that
    // checkout is still not evidence for the pull request's head.
    fs::write(root.join("file.txt"), "merged\n").unwrap();
    commit(&root, "merge");
    let (merged, _) = receipt(&root);
    let (code, annotations) = action_verify(&merged, &head);
    assert_eq!(code.trim(), "code=3", "{annotations}");
    assert!(
        annotations.contains("is not a run of commit"),
        "{annotations}"
    );
    // Checked out at the head itself, it is.
    git(&root, &["checkout", "--quiet", &head]);
    let (at_head, _) = receipt(&root);
    let (code, annotations) = action_verify(&at_head, &head);
    assert_eq!(code.trim(), "code=0", "{annotations}");
    let _ = fs::remove_file(merged);
    let _ = fs::remove_file(at_head);
    fs::remove_dir_all(root).unwrap();
}

/// Review of the Boundary: `--commit` must judge the receipt against the
/// commit alone, whatever is checked out and whatever is uncommitted.
#[test]
fn verify_commit_judges_the_commit_not_the_checkout() {
    let root = repo("notcheckedout", PASSING);
    let first = git(&root, &["rev-parse", "HEAD"]);
    let (r1, _) = receipt(&root);
    // A second commit changes a file and the contract itself.
    fs::write(root.join("file.txt"), "two\n").unwrap();
    fs::write(
        root.join("sykli.json"),
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"true","inputs":["file.txt"]},{"name":"u","run":"true"}]}"#,
    )
    .unwrap();
    assert!(sykli(&root, &["lock", "sykli.json"]).status.success());
    commit(&root, "second");
    // Uncommitted noise on top: an unstaged edit and an untracked file.
    fs::write(root.join("file.txt"), "three\n").unwrap();
    fs::write(root.join("editor.swp"), "x").unwrap();

    let out = verify_commit(&root, &r1, &first);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    // Plain verify still judges the live checkout.
    let live = sykli(
        &root,
        &["verify", r1.to_str().unwrap(), "--contract", "sykli.json"],
    );
    assert_ne!(live.status.code(), Some(0));
    // A receipt of another tree is still refused, checked out or not.
    let out = verify_commit(&root, &r1, "HEAD");
    assert_eq!(
        out.status.code(),
        Some(4),
        "the commit's contract differs: {}",
        text(&out.stderr)
    );
    git(&root, &["checkout", "--quiet", "--", "file.txt"]);
    fs::remove_file(root.join("editor.swp")).unwrap();
    let (r2, _) = receipt(&root);
    let out = verify_commit(&root, &r2, &first);
    assert_eq!(out.status.code(), Some(4), "{}", text(&out.stderr));
    fs::write(root.join("editor.swp"), "x").unwrap();
    let out = verify_commit(&root, &r2, "HEAD");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let _ = fs::remove_file(r1);
    let _ = fs::remove_file(r2);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn verify_commit_reads_the_contract_from_the_commit_even_when_it_is_gone_from_disk() {
    let root = repo("contractgone", PASSING);
    let (r1, _) = receipt(&root);
    fs::remove_file(root.join("sykli.json")).unwrap();
    fs::remove_file(root.join("sykli.lock")).unwrap();
    let out = verify_commit(&root, &r1, "HEAD");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    // A commit without that contract cannot be verified.
    git(&root, &["add", "--all"]);
    git(
        &root,
        &[
            "commit",
            "--quiet",
            "--no-verify",
            "-m",
            "drop the contract",
        ],
    );
    let out = verify_commit(&root, &r1, "HEAD");
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("sykli.json"),
        "{}",
        text(&out.stderr)
    );
    let _ = fs::remove_file(r1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pre_push_judges_the_pushed_commit_not_the_checkout() {
    let root = repo("pushnotcheckedout", READS_FILE);
    let first = git(&root, &["rev-parse", "HEAD"]);
    assert_eq!(sykli(&root, &["run", "sykli.json"]).status.code(), Some(0));
    fs::write(root.join("other.txt"), "more\n").unwrap();
    let second = commit(&root, "second");
    assert_eq!(sykli(&root, &["run", "sykli.json"]).status.code(), Some(0));
    // `git push origin <first>:main`, not checked out.
    let out = pre_push(&root, "origin", &[update(&first, "refs/heads/main")]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    // A force-push of the older, verified commit over the newer one.
    let force = format!("refs/heads/work {first} refs/heads/main {second}");
    let out = pre_push(&root, "origin", &[force]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    // HEAD with an unrelated unstaged edit and an untracked file.
    fs::write(root.join("other.txt"), "edited\n").unwrap();
    fs::write(root.join("scratch.tmp"), "x").unwrap();
    let out = pre_push(&root, "origin", &[update(&second, "refs/heads/main")]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    fs::remove_dir_all(root).unwrap();
}

/// The same three pushes through installed hooks and real `git push`.
#[cfg(unix)]
#[test]
fn installed_pre_push_judges_the_pushed_commit_not_the_checkout() {
    let root = repo("e2enotcheckedout", READS_FILE);
    let origin = root.with_extension("origin.git");
    let bin = Path::new(env!("CARGO_BIN_EXE_sykli")).parent().unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin.to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
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
    assert!(
        sykli(&root, &["hook", "install", "--pre-push"])
            .status
            .success()
    );
    let first = git(&root, &["rev-parse", "HEAD"]);
    assert_eq!(sykli(&root, &["run", "sykli.json"]).status.code(), Some(0));
    fs::write(root.join("other.txt"), "more\n").unwrap();
    let second = commit(&root, "second");
    assert_eq!(sykli(&root, &["run", "sykli.json"]).status.code(), Some(0));

    let out = hooked(&[
        "push",
        "--quiet",
        "origin",
        &format!("{first}:refs/heads/main"),
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let out = hooked(&["push", "--quiet", "origin", "main"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let out = hooked(&[
        "push",
        "--quiet",
        "--force",
        "origin",
        &format!("{first}:refs/heads/main"),
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    fs::write(root.join("other.txt"), "edited\n").unwrap();
    fs::write(root.join("scratch.tmp"), "x").unwrap();
    let out = hooked(&["push", "--quiet", "origin", "main"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(git(&origin, &["rev-parse", "main"]), second);
    fs::remove_dir_all(origin).unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// `action/verify.sh`, as the Action runs it; returns (`code=` line, annotations).
#[cfg(unix)]
fn action_verify(root: &Path, receipt: &Path, commit: &str) -> (String, String) {
    let output = root.with_extension("github-output");
    let _ = fs::remove_file(&output);
    let out = Command::new("sh")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("action/verify.sh"))
        .current_dir(root)
        .env("SYKLI_BINARY", env!("CARGO_BIN_EXE_sykli"))
        .env("SYKLI_CONTRACT", "sykli.json")
        .env("SYKLI_RECEIPT", receipt)
        .env("SYKLI_COMMIT", commit)
        .env("GITHUB_OUTPUT", &output)
        .env_remove("GITHUB_STEP_SUMMARY")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    let code = fs::read_to_string(&output).unwrap().trim().to_string();
    let _ = fs::remove_file(&output);
    (code, text(&out.stdout))
}

/// `run` hashes the tree before tasks execute, so only the live check sees a
/// task that leaves a stray file behind; the Action keeps it beside --commit.
#[cfg(unix)]
#[test]
fn the_action_keeps_the_live_check_and_names_a_merge_checkout() {
    let root = repo(
        "actionlive",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"echo x > stray.txt","inputs":["file.txt"]}]}"#,
    );
    let (stray, _) = receipt(&root);
    assert_eq!(verify_commit(&root, &stray, "HEAD").status.code(), Some(0));
    let (code, annotations) = action_verify(&root, &stray, "HEAD");
    assert_eq!(code, "code=3", "{annotations}");
    fs::remove_file(root.join("stray.txt")).unwrap();
    fs::write(root.join(".gitignore"), "stray.txt\n").unwrap();
    commit(&root, "ignore the output");

    // GitHub's merge ref: a merge commit whose second parent is the head.
    let base = git(&root, &["rev-parse", "HEAD"]);
    git(&root, &["checkout", "--quiet", "-b", "topic"]);
    fs::write(root.join("file.txt"), "topic\n").unwrap();
    let head = commit(&root, "topic");
    git(&root, &["checkout", "--quiet", &base]);
    fs::write(root.join("other.txt"), "main moved\n").unwrap();
    commit(&root, "main moved");
    git(
        &root,
        &["merge", "--quiet", "--no-ff", "--no-edit", "topic"],
    );
    let (merged, _) = receipt(&root);
    let (code, annotations) = action_verify(&root, &merged, &head);
    assert_eq!(code, "code=3", "{annotations}");
    assert!(annotations.contains("merge commit"), "{annotations}");
    git(&root, &["checkout", "--quiet", &head]);
    let (at_head, _) = receipt(&root);
    let (code, annotations) = action_verify(&root, &at_head, &head);
    assert_eq!(code, "code=0", "{annotations}");
    for path in [stray, merged, at_head] {
        let _ = fs::remove_file(path);
    }
    fs::remove_dir_all(root).unwrap();
}

/// Review finding 2: whether a push is gated is decided from the commits,
/// not from the working tree.
#[test]
fn pre_push_refuses_a_commit_that_takes_the_gate_off_main() {
    let root = repo("pushdropgate", PASSING);
    let gated = git(&root, &["rev-parse", "HEAD"]);
    git(&root, &["rm", "--quiet", "sykli.json", "sykli.lock"]);
    let dropped = commit(&root, "drop the contract");
    // The remote's main, the line's fourth field, has the contract.
    let over = format!("refs/heads/main {dropped} refs/heads/main {gated}");
    let out = pre_push(&root, "origin", &[over]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("--no-verify"),
        "{}",
        text(&out.stderr)
    );
    // Unknown here, so the remote-tracking branch says what main has.
    git(&root, &["update-ref", "refs/remotes/origin/main", &gated]);
    let unknown = format!(
        "refs/heads/main {dropped} refs/heads/main 1111111111111111111111111111111111111111"
    );
    let out = pre_push(&root, "origin", std::slice::from_ref(&unknown));
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    // Putting the contract back on disk does not change the pushed commit.
    git(
        &root,
        &["checkout", &gated, "--", "sykli.json", "sykli.lock"],
    );
    let out = pre_push(&root, "origin", &[unknown]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));

    // And a pushed commit with a contract is gated with none on disk.
    git(&root, &["checkout", "--quiet", &gated]);
    fs::remove_file(root.join("sykli.json")).unwrap();
    let out = pre_push(&root, "origin", &[update(&gated, "refs/heads/main")]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pre_push_says_when_it_cannot_tell_the_default_branch() {
    let root = repo("pushtrunk", PASSING);
    let head = git(&root, &["rev-parse", "HEAD"]);
    let out = pre_push(&root, "origin", &[update(&head, "refs/heads/trunk")]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("git remote set-head origin --auto"),
        "{}",
        text(&out.stderr)
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_push_with_one_refused_ref_is_refused_whole() {
    let root = repo("pushmulti", PASSING);
    let head = git(&root, &["rev-parse", "HEAD"]);
    let out = pre_push(
        &root,
        "origin",
        &[
            update(&head, "refs/heads/feature"),
            update(&head, "refs/heads/main"),
        ],
    );
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    fs::remove_dir_all(root).unwrap();
}

/// Review finding 3: `git diff` is silent for files marked assume-unchanged
/// or skip-worktree, so the hook compares the run's tree with the index.
#[test]
fn pre_commit_compares_the_run_with_the_tree_it_commits() {
    for bit in ["--assume-unchanged", "--skip-worktree"] {
        let root = repo("indexbits", READS_FILE);
        fs::write(root.join("file.txt"), "broken\n").unwrap();
        git(&root, &["add", "file.txt"]);
        fs::write(root.join("file.txt"), "one\n").unwrap();
        git(&root, &["update-index", bit, "file.txt"]);
        let out = sykli(&root, &["hook", "pre-commit"]);
        assert_eq!(out.status.code(), Some(1), "{bit}: {}", text(&out.stderr));
        assert!(
            text(&out.stderr).contains("not the tree being committed"),
            "{bit}: {}",
            text(&out.stderr)
        );
        fs::remove_dir_all(root).unwrap();
    }
}

/// Review finding 4: a receipt records a submodule as its gitlink, so content
/// a submodule has not committed is invisible to the tree. Such a run is
/// dirty, and `--commit` refuses it on its own account.
#[test]
fn a_modified_submodule_makes_the_run_dirty_and_is_refused_for_a_commit() {
    let root = repo(
        "submodule",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"test \"$(cat sub/file.txt)\" = one","inputs":["sub/file.txt"]}]}"#,
    );
    let sub = root.with_extension("sub");
    fs::create_dir(&sub).unwrap();
    git(&sub, &["init", "--quiet"]);
    fs::write(sub.join("file.txt"), "broken\n").unwrap();
    commit(&sub, "broken");
    git(
        &root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--quiet",
            sub.to_str().unwrap(),
            "sub",
        ],
    );
    commit(&root, "add the submodule at a broken commit");
    assert_eq!(sykli(&root, &["run", "sykli.json"]).status.code(), Some(1));
    // Fix the file inside the submodule without committing it there.
    fs::write(root.join("sub/file.txt"), "one\n").unwrap();
    let (path, value) = receipt(&root);
    assert_eq!(value["outcome"], "passed");
    assert_eq!(value["subject"]["dirty"], true, "{}", value["subject"]);
    let out = verify_commit(&root, &path, "HEAD");
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stdout));
    assert!(
        text(&out.stderr).contains("submodule"),
        "{}",
        text(&out.stderr)
    );
    let _ = fs::remove_file(path);
    fs::remove_dir_all(sub).unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Where accepted pushes' receipts are kept: the common git directory.
fn kept_receipts(root: &Path) -> Vec<PathBuf> {
    match fs::read_dir(root.join(".git/sykli/receipts")) {
        Ok(entries) => entries.flatten().map(|entry| entry.path()).collect(),
        Err(_) => Vec::new(),
    }
}

/// The house flow: commit in a worktree, push main through the hook, remove
/// the worktree. The receipt that verified the push must outlive it.
#[cfg(unix)]
#[test]
fn an_accepted_push_keeps_its_receipt_after_the_worktree_is_gone() {
    let root = repo("keepevidence", READS_FILE);
    let origin = root.with_extension("origin.git");
    let worktree = root.with_extension("worktree");
    let bin = Path::new(env!("CARGO_BIN_EXE_sykli")).parent().unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin.to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    git(
        &root,
        &["init", "--quiet", "--bare", origin.to_str().unwrap()],
    );
    git(
        &root,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    assert!(
        sykli(&root, &["hook", "install", "--pre-push"])
            .status
            .success()
    );
    git(
        &root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "work",
            worktree.to_str().unwrap(),
        ],
    );
    fs::write(worktree.join("other.txt"), "work\n").unwrap();
    let pushed = commit(&worktree, "work");
    assert_eq!(
        sykli(&worktree, &["run", "sykli.json"]).status.code(),
        Some(0)
    );
    let out = Command::new("git")
        .args(["push", "--quiet", "origin", "work:main"])
        .current_dir(&worktree)
        .env("PATH", &path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    git(
        &root,
        &["worktree", "remove", "--force", worktree.to_str().unwrap()],
    );
    assert!(!worktree.exists());

    let kept = kept_receipts(&root);
    assert_eq!(kept.len(), 1, "{kept:?}");
    let name = kept[0].file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with("rcpt_") && name.ends_with(".json") && name.len() == 5 + 64 + 5,
        "{name}"
    );
    let out = verify_commit(&root, &kept[0], &pushed);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    fs::remove_dir_all(origin).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_refused_push_keeps_nothing() {
    let root = repo("keepnothing", READS_FILE);
    let head = git(&root, &["rev-parse", "HEAD"]);
    assert_eq!(sykli(&root, &["run", "sykli.json"]).status.code(), Some(0));
    // main is accepted, master (also a default branch here) has no receipt
    // for its commit, so the whole push is refused and nothing is kept.
    fs::write(root.join("other.txt"), "more\n").unwrap();
    let unverified = commit(&root, "unverified");
    let out = pre_push(
        &root,
        "origin",
        &[
            update(&head, "refs/heads/main"),
            update(&unverified, "refs/heads/master"),
        ],
    );
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(kept_receipts(&root).is_empty());
    // Runs are journaled under .git/sykli/runs; nothing is kept as evidence.
    assert!(!root.join(".git/sykli/receipts").exists());
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn a_symlinked_evidence_directory_is_refused_and_the_push_still_goes() {
    let root = repo("keeplink", READS_FILE);
    let head = git(&root, &["rev-parse", "HEAD"]);
    assert_eq!(sykli(&root, &["run", "sykli.json"]).status.code(), Some(0));
    let elsewhere = root.with_extension("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    // The run journaled itself under .git/sykli; the push needs only its
    // receipt, so the directory can be replaced by a link.
    fs::remove_dir_all(root.join(".git/sykli")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, root.join(".git/sykli")).unwrap();
    let out = pre_push(&root, "origin", &[update(&head, "refs/heads/main")]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("symbolic link"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    fs::remove_dir_all(elsewhere).unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// `sykli hook pre-push` with extra environment, for the receipt budget.
fn pre_push_with(root: &Path, lines: &[String], env: &[(&str, &str)]) -> Output {
    let input: String = lines.iter().map(|line| format!("{line}\n")).collect();
    stdio::output(
        Command::new(env!("CARGO_BIN_EXE_sykli"))
            .args(["hook", "pre-push", "origin", "/nowhere/origin.git"])
            .envs(env.iter().copied())
            .current_dir(root),
        input.as_bytes(),
    )
    .unwrap()
}

/// Review of the kept evidence, finding 1: two first pushes at once must both
/// keep their receipt; creating the directory races, and losing is fine.
#[test]
fn concurrent_first_pushes_all_keep_their_receipt() {
    use std::io::Write;
    let root = repo("keeprace", READS_FILE);
    let head = git(&root, &["rev-parse", "HEAD"]);
    assert_eq!(sykli(&root, &["run", "sykli.json"]).status.code(), Some(0));
    for round in 0..5 {
        let _ = fs::remove_dir_all(root.join(".git/sykli"));
        let mut children = (0..12)
            .map(|_| {
                Command::new(env!("CARGO_BIN_EXE_sykli"))
                    .args(["hook", "pre-push", "origin", "/nowhere/origin.git"])
                    .current_dir(&root)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        // Each child waits on its input, so feeding them all releases them
        // together.
        let line = update(&head, "refs/heads/main");
        for child in &mut children {
            writeln!(child.stdin.take().unwrap(), "{line}").unwrap();
        }
        for child in children {
            let out = child.wait_with_output().unwrap();
            assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
            assert!(
                !text(&out.stderr).contains("warning"),
                "round {round}: {}",
                text(&out.stderr)
            );
        }
        assert_eq!(kept_receipts(&root).len(), 1, "round {round}");
    }
    fs::remove_dir_all(root).unwrap();
}

/// Finding 2: keeping a receipt again makes it the newest, so eviction never
/// takes the only evidence for a commit just pushed again. Finding 5: stale
/// temporary files from an interrupted copy are pruned too.
#[test]
fn re_keeping_a_receipt_makes_it_the_newest() {
    let root = repo("keepagain", READS_FILE);
    let budget = [("SYKLI_RECEIPT_BUDGET", "2")];
    let a = git(&root, &["rev-parse", "HEAD"]);
    assert_eq!(sykli(&root, &["run", "sykli.json"]).status.code(), Some(0));
    let pushed = |sha: &str| pre_push_with(&root, &[update(sha, "refs/heads/main")], &budget);
    assert_eq!(pushed(&a).status.code(), Some(0));
    let kept_a = kept_receipts(&root);
    assert_eq!(kept_a.len(), 1);
    fs::write(root.join("other.txt"), "b\n").unwrap();
    let b = commit(&root, "b");
    assert_eq!(sykli(&root, &["run", "sykli.json"]).status.code(), Some(0));
    assert_eq!(pushed(&b).status.code(), Some(0));
    // A force-push back to A keeps A's receipt again.
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert_eq!(pushed(&a).status.code(), Some(0));
    // A temporary file an interrupted copy left behind, long ago.
    let stale = root.join(".git/sykli/receipts/.rcpt_0.json.tmp-1");
    fs::write(&stale, "{").unwrap();
    fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1))
        .unwrap();
    fs::write(root.join("other.txt"), "c\n").unwrap();
    let c = commit(&root, "c");
    assert_eq!(sykli(&root, &["run", "sykli.json"]).status.code(), Some(0));
    assert_eq!(pushed(&c).status.code(), Some(0));
    // Budget 2: B's receipt went, A's (kept again after B) stayed.
    let kept = kept_receipts(&root);
    assert_eq!(kept.len(), 2, "{kept:?}");
    assert!(kept.contains(&kept_a[0]), "{kept:?}");
    assert!(!stale.exists(), "a stale temporary file is pruned");
    let out = verify_commit(&root, &kept_a[0], &a);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    fs::remove_dir_all(root).unwrap();
}

/// Inside a hook git prepends its exec path to PATH, and PATH is part of every
/// task's runtime. A run in a hook and the same run by hand must share keys.
#[cfg(unix)]
#[test]
fn a_hooked_run_and_a_manual_run_of_the_same_tree_share_the_cache() {
    let root = repo("hookpath", READS_FILE);
    let bin = Path::new(env!("CARGO_BIN_EXE_sykli")).parent().unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin.to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    assert!(
        sykli(&root, &["hook", "install", "--pre-commit"])
            .status
            .success()
    );
    fs::write(root.join("other.txt"), "more\n").unwrap();
    git(&root, &["add", "other.txt"]);
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["commit", "--quiet", "-m", "through the hook"])
        .current_dir(&root)
        .env("PATH", &path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    // The same tree by hand, with the PATH the test gave git.
    let manual = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["run", "sykli.json", "--json"])
        .current_dir(&root)
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_eq!(manual.status.code(), Some(0), "{}", text(&manual.stderr));
    let receipt: serde_json::Value = serde_json::from_slice(&manual.stdout).unwrap();
    for task in receipt["tasks"].as_array().unwrap() {
        assert_eq!(task["outcome"], "cached", "{}", task["name"]);
    }
    fs::remove_dir_all(root).unwrap();
}

/// `sykli hook pre-push FLAGS... origin URL`, fed git's pre-push `lines`.
fn pre_push_flags(root: &Path, flags: &[&str], lines: &[String]) -> Output {
    let input: String = lines.iter().map(|line| format!("{line}\n")).collect();
    stdio::output(
        Command::new(env!("CARGO_BIN_EXE_sykli"))
            .args(["hook", "pre-push"])
            .args(flags)
            .args(["origin", "/nowhere/origin.git"])
            .current_dir(root),
        input.as_bytes(),
    )
    .unwrap()
}

/// The caller knows which branch it protects; `refs/remotes/origin/HEAD` is
/// only a guess, and it can name another branch than the caller's.
#[test]
fn pre_push_branch_gates_exactly_the_named_branches() {
    let root = repo("pushnamed", READS_FILE);
    // A commit with no receipt.
    fs::write(root.join("file.txt"), "one\n\n").unwrap();
    let head = commit(&root, "no receipt");
    // origin/HEAD names develop, but main is the branch to protect.
    git(&root, &["update-ref", "refs/remotes/origin/develop", &head]);
    git(
        &root,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/develop",
        ],
    );
    let push = |flags: &[&str], remote_ref: &str| {
        pre_push_flags(&root, flags, &[update(&head, remote_ref)])
    };
    // The guess gates develop and lets main through.
    assert_eq!(push(&[], "refs/heads/main").status.code(), Some(0));
    // Named, main is gated whatever origin/HEAD says ...
    let out = push(&["--branch", "main"], "refs/heads/main");
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("refusing to push"));
    // ... and nothing else is, with no guess and no note about one.
    for other in ["refs/heads/develop", "refs/heads/feature"] {
        let out = push(&["--branch", "main"], other);
        assert_eq!(out.status.code(), Some(0), "{other}: {}", text(&out.stderr));
        assert!(
            !text(&out.stderr).contains("set-head"),
            "{}",
            text(&out.stderr)
        );
    }
    // Two names gate both.
    let both = ["--branch", "main", "--branch", "trunk2"];
    for gated in ["refs/heads/main", "refs/heads/trunk2"] {
        let out = push(&both, gated);
        assert_eq!(out.status.code(), Some(1), "{gated}: {}", text(&out.stderr));
    }
    assert_eq!(push(&both, "refs/heads/feature").status.code(), Some(0));
    // A deletion and a tag pass, as they always have.
    let delete = format!("(delete) {ZEROS} refs/heads/main {head}");
    let tag = format!("refs/tags/v1 {head} refs/tags/v1 {ZEROS}");
    for line in [delete, tag.clone()] {
        let out = pre_push_flags(&root, &both, std::slice::from_ref(&line));
        assert_eq!(out.status.code(), Some(0), "{line}: {}", text(&out.stderr));
    }
    let out = pre_push_flags(&root, &["--branch", "v1"], &[tag]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    // With a receipt of the commit, the named branch takes it.
    assert_eq!(sykli(&root, &["run", "sykli.json"]).status.code(), Some(0));
    let out = push(&both, "refs/heads/main");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    // A value that is not a branch name cannot gate anything: fail closed.
    // `HEAD` and a leading `-` pass `git check-ref-format` as refs, yet no
    // branch can have such a name; `git branch` refuses both too.
    for bad in [
        "refs/heads/main",
        "",
        "two words",
        "main..x",
        "HEAD",
        "-dash",
    ] {
        let out = push(&[&format!("--branch={bad}")], "refs/heads/main");
        assert_eq!(out.status.code(), Some(2), "{bad:?}: {}", text(&out.stderr));
        assert!(
            text(&out.stderr).contains("is not a branch name"),
            "{bad:?}: {}",
            text(&out.stderr)
        );
    }
    fs::remove_dir_all(root).unwrap();
}

/// After the host renames the default branch and a fetch prunes the old
/// one, origin/HEAD still names it. Gating a branch that is gone would gate
/// nothing, so the hook takes the default as unknown and says so.
#[test]
fn pre_push_takes_an_origin_head_naming_a_missing_ref_as_unknown() {
    let root = repo("pushstale", READS_FILE);
    fs::write(root.join("file.txt"), "one\n\n").unwrap();
    let head = commit(&root, "no receipt");
    git(
        &root,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/master",
        ],
    );
    let out = pre_push(&root, "origin", &[update(&head, "refs/heads/main")]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let message = text(&out.stderr);
    for said in [
        "refs/remotes/origin/master, which does not exist",
        "git remote set-head origin --auto",
        "--branch",
    ] {
        assert!(message.contains(said), "{said}: {message}");
    }
    // Naming a ref that exists, it is the default branch again.
    git(&root, &["update-ref", "refs/remotes/origin/master", &head]);
    let out = pre_push(&root, "origin", &[update(&head, "refs/heads/main")]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(!text(&out.stderr).contains("does not exist"));
    fs::remove_dir_all(root).unwrap();
}

/// A commit whose contract differs from its own lock cannot pass any run;
/// the refusal says to re-lock, not to run.
#[test]
fn pre_push_names_sykli_lock_when_the_commit_contract_drifted() {
    let root = repo("pushdrift", READS_FILE);
    fs::write(
        root.join("sykli.json"),
        READS_FILE.replace("= one", "= one && true"),
    )
    .unwrap();
    let head = commit(&root, "edited, not locked");
    let out = pre_push(&root, "origin", &[update(&head, "refs/heads/main")]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let message = text(&out.stderr);
    assert!(message.contains("sykli lock sykli.json"), "{message}");
    fs::remove_dir_all(root).unwrap();
}

/// `refs/remotes/REMOTE/HEAD` is read as git reads it. Without --branch, each
/// of these named the default branch to git, and the hook must gate it:
/// `ref:` with no space or with a tab before the target, and remotes named
/// `HEAD` and `@`, which git allows.
#[test]
fn pre_push_reads_the_remote_head_as_git_does() {
    let root = repo("pushrefs", READS_FILE);
    fs::write(root.join("file.txt"), "one\n\n").unwrap();
    let head = commit(&root, "no receipt");
    let to_trunk = || [update(&head, "refs/heads/trunk")];
    git(&root, &["update-ref", "refs/remotes/origin/trunk", &head]);
    for spelling in [
        "ref:refs/remotes/origin/trunk\n",
        "ref:\trefs/remotes/origin/trunk\n",
    ] {
        fs::write(root.join(".git/refs/remotes/origin/HEAD"), spelling).unwrap();
        assert_eq!(
            git(&root, &["symbolic-ref", "refs/remotes/origin/HEAD"]),
            "refs/remotes/origin/trunk",
            "git reads {spelling:?}"
        );
        let out = pre_push(&root, "origin", &to_trunk());
        assert_eq!(
            out.status.code(),
            Some(1),
            "{spelling:?}: {}",
            text(&out.stderr)
        );
    }
    for remote in ["HEAD", "@"] {
        git(
            &root,
            &["update-ref", &format!("refs/remotes/{remote}/trunk"), &head],
        );
        git(
            &root,
            &[
                "symbolic-ref",
                &format!("refs/remotes/{remote}/HEAD"),
                &format!("refs/remotes/{remote}/trunk"),
            ],
        );
        let out = pre_push(&root, remote, &to_trunk());
        assert_eq!(
            out.status.code(),
            Some(1),
            "remote {remote}: {}",
            text(&out.stderr)
        );
    }
    fs::remove_dir_all(root).unwrap();
}
