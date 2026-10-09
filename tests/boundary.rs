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
