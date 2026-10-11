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

/// A repository and, beside it rather than inside it, a directory holding a
/// contract: `<base>/repo` and `<base>/config`. Removed when dropped.
struct Scratch {
    base: PathBuf,
    repo: PathBuf,
    config: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("sykli-external-{name}-{nonce}"));
        let repo = base.join("repo");
        let config = base.join("config");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&config).unwrap();
        fs::write(repo.join("file.txt"), "one\n").unwrap();
        git(&repo, &["init", "--quiet", "--initial-branch=main"]);
        // What a person's global ignore does for `.sykli/`, kept local here.
        fs::write(repo.join(".git/info/exclude"), ".sykli/\n").unwrap();
        commit(&repo, "fixture");
        Scratch { base, repo, config }
    }

    /// The contract in the configuration directory, written and locked
    /// from inside the repository.
    fn contract(&self, contract: &str) -> PathBuf {
        let path = self.config.join("sykli.json");
        fs::write(&path, contract).unwrap();
        let out = sykli(&self.repo, &["lock", path.to_str().unwrap()]);
        assert!(out.status.success(), "{}", text(&out.stderr));
        path
    }

    /// `git status --porcelain`: empty when the repository is untouched.
    fn status(&self) -> String {
        git(
            &self.repo,
            &["status", "--porcelain", "--untracked-files=all"],
        )
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

/// `sykli run CONTRACT --json` from `cwd`, the receipt kept beside the
/// scratch directories so it is in neither.
fn receipt(scratch: &Scratch, cwd: &Path, contract: &Path) -> (PathBuf, serde_json::Value) {
    let out = sykli(cwd, &["run", contract.to_str().unwrap(), "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("no receipt: {}", text(&out.stderr)));
    let path = scratch
        .base
        .join(format!("receipt-{}.json", value["started_at_ms"]));
    fs::write(&path, &out.stdout).unwrap();
    (path, value)
}

fn names(directory: &Path) -> Vec<String> {
    let mut names = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
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

#[test]
fn run_lock_plan_and_verify_take_a_contract_outside_the_repository() {
    let scratch = Scratch::new("outside");
    let repo = &scratch.repo;
    // Work from a subdirectory: the repository is the toplevel around the
    // current directory, never the current directory or the contract's.
    let sub = repo.join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("note.txt"), "note\n").unwrap();
    commit(repo, "sub");
    let contract = scratch.contract(READS_FILE);
    let path = contract.to_str().unwrap();
    assert_eq!(names(&scratch.config), ["sykli.json", "sykli.lock"]);

    let out = sykli(&sub, &["validate", path]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let out = sykli(&sub, &["plan", path, "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let plan: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(plan["tasks"], serde_json::json!(["t"]));
    // `--changed` means what it says where it was typed.
    let out = sykli(
        &sub,
        &[
            "plan",
            path,
            "--explain",
            "--json",
            "--changed",
            "../file.txt",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let plan: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let explanation = &plan["explanations"][0];
    assert_eq!(explanation["selection"][0]["code"], "changed_input");
    assert_eq!(explanation["cache"]["status"], "missing");

    let (first, value) = receipt(&scratch, &sub, &contract);
    assert_eq!(value["outcome"], "passed", "{value}");
    assert_eq!(value["subject"]["dirty"], false);
    assert_eq!(
        value["lock_digest"].as_str(),
        Some(sha256(&scratch.config.join("sykli.lock")).as_str())
    );
    assert_eq!(names(&repo.join(".sykli/receipts")).len(), 1);
    // A relative path to the contract works too, and reuses the pass.
    let relative = Path::new("..").join("..").join("config").join("sykli.json");
    let (_, value) = receipt(&scratch, &sub, &relative);
    assert_eq!(value["outcome"], "cached", "{value}");

    let verify = |cwd: &Path| {
        sykli(
            cwd,
            &["verify", first.to_str().unwrap(), "--contract", path],
        )
    };
    for cwd in [repo, &sub] {
        let out = verify(cwd);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));
    }
    // Nothing was written into the repository, and nothing beside the
    // contract but its lock.
    assert_eq!(scratch.status(), "");
    assert_eq!(names(&scratch.config), ["sykli.json", "sykli.lock"]);

    fs::write(repo.join("file.txt"), "two\n").unwrap();
    let out = verify(&sub);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stdout));
}

#[test]
fn the_folder_rule_holds_for_a_contract_outside_the_repository() {
    let scratch = Scratch::new("folder");
    let repo = &scratch.repo;
    fs::create_dir(repo.join("fixtures")).unwrap();
    fs::write(repo.join("fixtures/a.txt"), "a\n").unwrap();
    fs::write(repo.join("fixtures/b.txt"), "b\n").unwrap();
    commit(repo, "fixtures");
    let contract = scratch.contract(
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"cat fixtures/*.txt","inputs":["fixtures/a.txt"]}]}"#,
    );
    let path = contract.to_str().unwrap();
    let cwd = repo.join("fixtures");
    let out = sykli(&cwd, &["run", path]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("fixtures/b.txt (task t)"),
        "{}",
        text(&out.stderr)
    );
    let out = sykli(&cwd, &["plan", path, "--explain", "--json"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let plan: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        plan["input_coverage"]["folder_missing_inputs"],
        serde_json::json!([{"path": "fixtures/b.txt", "task": "t"}])
    );
}

#[test]
fn the_cargo_gate_holds_for_a_contract_outside_the_repository() {
    let scratch = Scratch::new("cargo");
    let repo = &scratch.repo;
    fs::create_dir(repo.join("src")).unwrap();
    fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(repo.join("src/main.rs"), "mod extra;\nfn main() {}\n").unwrap();
    fs::write(repo.join("src/extra.rs"), "").unwrap();
    commit(repo, "crate");
    let contract = scratch.contract(
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"build","run":"cargo build","inputs":["Cargo.toml","src/main.rs"]}]}"#,
    );
    // From `src`, where there is no Cargo.toml: the repository root has one.
    let out = sykli(&repo.join("src"), &["run", contract.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let message = text(&out.stderr);
    assert!(message.contains("Cargo inputs are undeclared"), "{message}");
    assert!(message.contains("src/extra.rs"), "{message}");
}

fn verify(cwd: &Path, receipt: &Path, contract: &Path, commit: Option<&str>) -> Output {
    let mut args = vec![
        "verify",
        receipt.to_str().unwrap(),
        "--contract",
        contract.to_str().unwrap(),
    ];
    if let Some(commit) = commit {
        args.extend(["--commit", commit]);
    }
    sykli(cwd, &args)
}

/// The one line verify prints on stderr when it refuses.
fn refusal(out: &Output) -> String {
    let reason = text(&out.stderr);
    assert_eq!(
        reason.trim().lines().count(),
        1,
        "one-line reason: {reason}"
    );
    reason
}

#[test]
fn verify_commit_reads_a_contract_outside_the_repository_from_its_file() {
    let scratch = Scratch::new("verifycommit");
    let repo = &scratch.repo;
    // The repository's own contract, if it has one, is not the one judged.
    fs::write(
        repo.join("sykli.json"),
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"other","run":"false"}]}"#,
    )
    .unwrap();
    fs::create_dir(repo.join("sub")).unwrap();
    fs::write(repo.join("sub/note.txt"), "note\n").unwrap();
    let head = commit(repo, "a contract of its own");
    let contract = scratch.contract(READS_FILE);
    let (receipt, value) = receipt(&scratch, repo, &contract);
    assert_eq!(value["outcome"], "passed", "{value}");
    for cwd in [repo.clone(), repo.join("sub")] {
        for rev in ["HEAD", head.as_str()] {
            let out = verify(&cwd, &receipt, &contract, Some(rev));
            assert_eq!(
                out.status.code(),
                Some(0),
                "{rev}: {}{}",
                text(&out.stdout),
                text(&out.stderr)
            );
        }
    }
    // The tree still binds the receipt to its commit.
    fs::write(repo.join("file.txt"), "one\n\n").unwrap();
    commit(repo, "another tree");
    let out = verify(repo, &receipt, &contract, Some("HEAD"));
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stdout));
    assert!(refusal(&out).contains("commit"));
    let out = verify(repo, &receipt, &contract, Some("HEAD~1"));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));
}

#[test]
fn verify_commit_refuses_a_receipt_of_another_contract_version() {
    let scratch = Scratch::new("version");
    let repo = &scratch.repo;
    let contract = scratch.contract(READS_FILE);
    let (old, _) = receipt(&scratch, repo, &contract);
    assert_eq!(
        verify(repo, &old, &contract, Some("HEAD")).status.code(),
        Some(0)
    );

    // The person edits their contract; until it is locked, it cannot judge.
    let edited = READS_FILE.replace("= one", "= one && true");
    fs::write(&contract, &edited).unwrap();
    let out = verify(repo, &old, &contract, Some("HEAD"));
    assert_eq!(out.status.code(), Some(4), "{}", text(&out.stdout));
    assert!(refusal(&out).contains("contract lock"));

    // Locked, it is the contract now, and the old receipt is not a run of it.
    assert!(
        sykli(repo, &["lock", contract.to_str().unwrap()])
            .status
            .success()
    );
    for commit in [Some("HEAD"), None] {
        let out = verify(repo, &old, &contract, commit);
        assert_eq!(out.status.code(), Some(4), "{}", text(&out.stdout));
        let reason = refusal(&out);
        assert!(reason.starts_with("refused: contract expected"), "{reason}");
        assert!(reason.contains(contract.to_str().unwrap()), "{reason}");
    }

    // A run of the new version is.
    let (new, _) = receipt(&scratch, repo, &contract);
    let out = verify(repo, &new, &contract, Some("HEAD"));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));
}

#[test]
fn a_receipt_for_a_contract_outside_the_repository_must_name_its_lock() {
    let scratch = Scratch::new("lockcheck");
    let repo = &scratch.repo;
    let contract = scratch.contract(READS_FILE);
    let (path, value) = receipt(&scratch, repo, &contract);
    assert_eq!(verify(repo, &path, &contract, None).status.code(), Some(0));

    // The lock changed though the contract did not: the receipt did not run
    // with this lock, so it is refused as another version would be.
    let other = scratch.config.join("other.json");
    fs::write(&other, READS_FILE).unwrap();
    assert!(
        sykli(repo, &["lock", other.to_str().unwrap()])
            .status
            .success()
    );
    for commit in [Some("HEAD"), None] {
        let out = verify(repo, &path, &contract, commit);
        assert_eq!(out.status.code(), Some(4), "{}", text(&out.stdout));
        let reason = refusal(&out);
        assert!(reason.starts_with("refused: lock expected"), "{reason}");
        assert!(reason.contains("sykli.lock"), "{reason}");
    }

    // A receipt from before receipts named their lock cannot show it ran
    // with this one.
    let (path, mut value) = (path, value);
    value.as_object_mut().unwrap().remove("lock_digest");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let out = verify(repo, &path, &contract, Some("HEAD"));
    assert_eq!(out.status.code(), Some(4), "{}", text(&out.stdout));

    // Without a lock, a receipt that ran without one verifies.
    fs::remove_file(scratch.config.join("sykli.lock")).unwrap();
    let (path, value) = receipt(&scratch, repo, &contract);
    assert!(value["lock_digest"].is_null(), "{value}");
    let out = verify(repo, &path, &contract, Some("HEAD"));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));
}

#[test]
fn a_contract_inside_the_repository_is_read_from_the_commit_however_it_is_named() {
    let scratch = Scratch::new("inside");
    let repo = &scratch.repo;
    fs::write(repo.join("sykli.json"), READS_FILE).unwrap();
    assert!(sykli(repo, &["lock", "sykli.json"]).status.success());
    commit(repo, "contract");
    let (path, _) = receipt(&scratch, repo, Path::new("sykli.json"));
    // An uncommitted, locked edit on disk is not what the commit carries.
    fs::write(
        repo.join("sykli.json"),
        READS_FILE.replace("= one", "= one && true"),
    )
    .unwrap();
    assert!(sykli(repo, &["lock", "sykli.json"]).status.success());
    // Named by the path the test was given (on macOS, through the /var
    // symbolic link), it is still the repository's own contract.
    let named = repo.join("sykli.json");
    let out = verify(repo, &path, &named, Some("HEAD"));
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
}
