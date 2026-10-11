//! A contract kept outside the repository it judges: a person's own gate,
//! held in their configuration instead of being committed. The repository is
//! always the git toplevel of the current directory, the contract's inputs
//! stay paths in that repository, and the contract and its lock are read from
//! where they are.

use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

/// A repository in a scratch directory: `<base>/repo`. Removed when dropped.
struct Scratch {
    base: PathBuf,
    repo: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("sykli-external-{name}-{nonce}"));
        let repo = base.join("repo");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("file.txt"), "one\n").unwrap();
        git(&repo, &["init", "--quiet", "--initial-branch=main"]);
        // What a person's global ignore does for `.sykli/`, kept local here.
        fs::write(repo.join(".git/info/exclude"), ".sykli/\n").unwrap();
        commit(&repo, "fixture");
        Scratch { base, repo }
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

fn commit(root: &Path, message: &str) -> String {
    git(root, &["add", "--all"]);
    git(root, &["commit", "--quiet", "--no-verify", "-m", message]);
    git(root, &["rev-parse", "HEAD"])
}

fn sykli(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn sha256(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

/// The task passes only while `file.txt` says `one`.
const READS_FILE: &str = r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"test \"$(cat file.txt)\" = one","inputs":["file.txt"]}]}"#;

#[test]
fn a_receipt_records_the_digest_of_the_lock_it_ran_with() {
    let scratch = Scratch::new("lockdigest");
    let repo = &scratch.repo;
    fs::write(repo.join("sykli.json"), READS_FILE).unwrap();
    assert!(sykli(repo, &["lock", "sykli.json"]).status.success());
    commit(repo, "contract");
    let out = sykli(repo, &["run", "sykli.json", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let mut value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        value["lock_digest"].as_str(),
        Some(sha256(&repo.join("sykli.lock")).as_str())
    );

    // A receipt written before the field existed is still read, and judged
    // as it always was against a contract in the repository.
    value.as_object_mut().unwrap().remove("lock_digest");
    let old = scratch.base.join("old-receipt.json");
    fs::write(&old, serde_json::to_vec(&value).unwrap()).unwrap();
    for extra in [&[][..], &["--commit", "HEAD"][..]] {
        let mut args = vec!["verify", old.to_str().unwrap(), "--contract", "sykli.json"];
        args.extend(extra);
        let out = sykli(repo, &args);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{extra:?}: {}",
            text(&out.stdout)
        );
    }

    // Without a lock, the receipt says there was none.
    fs::remove_file(repo.join("sykli.lock")).unwrap();
    let out = sykli(repo, &["run", "sykli.json", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(value["lock_digest"].is_null(), "{value}");
    assert!(value.as_object().unwrap().contains_key("lock_digest"));
}
