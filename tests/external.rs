//! A contract kept outside the repository it judges: a person's own gate,
//! held in their configuration instead of being committed. The repository is
//! always the git toplevel of the current directory, the contract's inputs
//! stay paths in that repository, and the contract and its lock are read from
//! where they are.

#[path = "support/stdio.rs"]
mod stdio;

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
    // Listed with the platform's separator, escaped as Rust's Debug does.
    assert!(
        message.replace("\\\\", "/").contains("src/extra.rs"),
        "{message}"
    );
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

/// A contract of one task, `check`, that reads `file.txt` from the
/// repository and declares `contract_inputs` beside the contract (none: no
/// field, as a contract written before the field was).
fn helper_contract(run: &str, contract_inputs: &[&str]) -> String {
    let mut task = serde_json::json!({"name": "check", "run": run, "inputs": ["file.txt"]});
    if !contract_inputs.is_empty() {
        task["contract_inputs"] = contract_inputs.into();
    }
    serde_json::json!({"schema": "sykli-contract.v1", "tasks": [task]}).to_string()
}

/// How a task runs a helper it declares beside its contract.
const RUNS_HELPER: &str = r#"sh "$SYKLI_CONTRACT_DIR/check.sh""#;

/// sykli#75: a helper script beside the contract, run by its path. It is
/// not a declared input, so nothing about it reached the task's key, and
/// after it changed from `exit 0` to `exit 1` the next run reported the old
/// pass as `cached` and exited 0. A task may not name the contract's
/// directory: sykli cannot hash what it reads there by that name.
#[test]
fn a_helper_named_beside_an_outside_contract_never_reuses_a_stale_pass() {
    let scratch = Scratch::new("helper");
    let repo = &scratch.repo;
    let script = scratch.config.join("check.sh");
    fs::write(&script, "exit 0\n").unwrap();
    let contract = scratch.config.join("sykli.json");
    let path = contract.to_str().unwrap();
    let by_path = format!("sh '{}'", script.display());
    fs::write(&contract, helper_contract(&by_path, &[])).unwrap();
    let _ = sykli(repo, &["lock", path]);
    let _ = sykli(repo, &["run", path]);
    fs::write(&script, "exit 1\n").unwrap();
    let again = sykli(repo, &["run", path]);
    assert_ne!(
        again.status.code(),
        Some(0),
        "a changed helper passed: {}{}",
        text(&again.stdout),
        text(&again.stderr)
    );
    assert!(!text(&again.stdout).contains("cached"));

    // Refused wherever a contract is written or run, before anything runs,
    // and declaring the file does not make its path a way to reach it.
    for declared in [&[][..], &["check.sh"][..]] {
        fs::write(&contract, helper_contract(&by_path, declared)).unwrap();
        let out = sykli(repo, &["validate", path]);
        assert_eq!(out.status.code(), Some(1), "{}", text(&out.stdout));
        let message = text(&out.stderr);
        assert!(message.contains("names the directory"), "{message}");
        assert!(message.contains("$SYKLI_CONTRACT_DIR"), "{message}");
        for command in [&["run", path][..], &["lock", path], &["plan", path]] {
            let out = sykli(repo, command);
            assert_eq!(
                out.status.code(),
                Some(2),
                "{command:?}: {}",
                text(&out.stderr)
            );
            assert!(text(&out.stderr).contains("names the directory"));
        }
    }
    // Named through an environment value, the same.
    let in_env = serde_json::json!({
        "schema": "sykli-contract.v1",
        "tasks": [{"name": "check", "run": "sh \"$HELPERS/check.sh\"",
                   "env": {"HELPERS": scratch.config.to_str().unwrap()}}],
    });
    fs::write(&contract, in_env.to_string()).unwrap();
    assert_eq!(sykli(repo, &["validate", path]).status.code(), Some(1));
    // A directory whose name only starts with the contract's is another one.
    let sibling = format!("{}-other", scratch.config.display());
    fs::write(
        &contract,
        helper_contract(&format!("test ! -e '{sibling}'"), &[]),
    )
    .unwrap();
    let out = sykli(repo, &["validate", path]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}

/// `validate` of a contract whose task runs `run`, with `env`, from the
/// repository, with HOME set to `home` for sykli.
fn validate_with_home(
    scratch: &Scratch,
    contract: &Path,
    task: serde_json::Value,
    home: &Path,
) -> Output {
    let contract_text = serde_json::json!({"schema": "sykli-contract.v1", "tasks": [task]});
    fs::write(contract, contract_text.to_string()).unwrap();
    Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["validate", contract.to_str().unwrap()])
        .current_dir(&scratch.repo)
        .env("HOME", home)
        .output()
        .unwrap()
}

/// The contract's directory spelled through the HOME the task gets (its
/// declared one, else sykli's: `$HOME`, `${HOME}` or a leading `~`), or with
/// `//` or `/./` in its path, is refused as its plain path is.
#[test]
fn the_contract_directory_is_refused_through_home_and_extra_slashes() {
    let scratch = Scratch::new("spellings");
    let contract = scratch.config.join("sykli.json");
    let base = scratch.base.display().to_string();
    let task = |run: &str| serde_json::json!({"name": "check", "run": run});
    for run in [
        "sh $HOME/config/check.sh".to_string(),
        "sh \"${HOME}/config/check.sh\"".to_string(),
        "sh ~/config/check.sh".to_string(),
        "HELPERS=~/config sh -c 'sh \"$HELPERS/check.sh\"'".to_string(),
        format!("sh {base}//config/check.sh"),
        format!("sh {base}/./config/check.sh"),
        // Quotes that split the path are removed, as the shell removes them.
        "sh \"$HOME\"/config/check.sh".to_string(),
        "sh \"${HOME}\"/config/check.sh".to_string(),
        "sh $HOME\"/config/check.sh\"".to_string(),
        "sh ~/\"config\"/check.sh".to_string(),
        format!("sh '{base}'/config/check.sh"),
        format!("sh \"{base}/con\"fig/check.sh"),
        // A variable before the path may be empty, quoted or not.
        format!("sh \"$PREFIX\"{base}/config/check.sh"),
    ] {
        let out = validate_with_home(&scratch, &contract, task(&run), &scratch.base);
        assert_eq!(out.status.code(), Some(1), "{run}: {}", text(&out.stdout));
        assert!(text(&out.stderr).contains("names the directory"), "{run}");
    }
    // A backslash escape is removed too, where it is not a separator.
    if cfg!(unix) {
        let run = format!("sh {base}/con\\fig/check.sh");
        let out = validate_with_home(&scratch, &contract, task(&run), &scratch.base);
        assert_eq!(out.status.code(), Some(1), "{run}: {}", text(&out.stdout));
    }
    // The task's own HOME is the one its shell expands.
    let declared = serde_json::json!({
        "name": "check", "run": "sh $HOME/config/check.sh", "env": {"HOME": base},
    });
    let out = validate_with_home(&scratch, &contract, declared, &scratch.repo);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stdout));
    // Another variable, another directory, or a HOME that leads elsewhere
    // names something else.
    for (run, home) in [
        ("sh $HOMEDIR/config/check.sh", &scratch.base),
        ("sh $HOME/config-old/check.sh", &scratch.base),
        ("sh ~user/config/check.sh", &scratch.base),
        ("sh $HOME/config/check.sh", &scratch.repo),
        // A quote inside an ordinary argument is only a quote.
        (
            "printf '%s\\n' \"don't\" \"$HOME\"/config-old/check.sh",
            &scratch.base,
        ),
        ("echo \"it's $HOME/configs\"", &scratch.base),
    ] {
        let out = validate_with_home(&scratch, &contract, task(run), home);
        assert_eq!(out.status.code(), Some(0), "{run}: {}", text(&out.stderr));
    }
}

/// With no git repository around the current directory there is no inside
/// or outside to judge (and nothing can run), so a contract that declares
/// contract inputs validates and locks there as any other contract does.
#[test]
fn contract_inputs_validate_and_lock_where_there_is_no_repository() {
    let scratch = Scratch::new("norepo");
    fs::write(scratch.config.join("check.sh"), "exit 0\n").unwrap();
    let contract = scratch.config.join("sykli.json");
    fs::write(&contract, helper_contract(RUNS_HELPER, &["check.sh"])).unwrap();
    let ceiling = scratch.base.parent().unwrap();
    for cwd in [&scratch.config, &scratch.base] {
        for (command, code) in [("validate", 0), ("lock", 0)] {
            let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
                .args([command, contract.to_str().unwrap()])
                .current_dir(cwd)
                .env("GIT_CEILING_DIRECTORIES", ceiling)
                .output()
                .unwrap();
            assert_eq!(
                out.status.code(),
                Some(code),
                "{command} in {}: {}",
                cwd.display(),
                text(&out.stderr)
            );
        }
    }
    // From the repository it judges, it is the same contract.
    let (_, value) = receipt(&scratch, &scratch.repo, &contract);
    assert_eq!(value["outcome"], "passed", "{value}");
}

/// A contract kept in a directory that holds the repository may name paths
/// in the repository (they are repository paths, declared as inputs), but
/// not the files beside it, nor reach them out of the repository by `..`.
#[test]
fn a_repository_inside_the_contract_directory_is_named_freely() {
    let scratch = Scratch::new("enclosing");
    let repo = &scratch.repo;
    let contract = scratch.base.join("sykli.json");
    let repo_path = repo.display().to_string();
    let task = |run: String, env: serde_json::Value| serde_json::json!({"name": "check", "run": run, "env": env, "inputs": ["file.txt"]});
    for (run, env) in [
        (
            format!("test -f '{repo_path}/file.txt'"),
            serde_json::json!({}),
        ),
        (
            "true".to_string(),
            serde_json::json!({"CARGO_TARGET_DIR": format!("{repo_path}/target")}),
        ),
        (
            format!("test -f \"{repo_path}\"/file.txt"),
            serde_json::json!({}),
        ),
        (format!("cd '{repo_path}' && true"), serde_json::json!({})),
    ] {
        let out = validate_with_home(&scratch, &contract, task(run.clone(), env), repo);
        assert_eq!(out.status.code(), Some(0), "{run}: {}", text(&out.stderr));
    }
    let out = sykli(repo, &["lock", contract.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let (_, value) = receipt(&scratch, repo, &contract);
    assert_eq!(value["outcome"], "passed", "{value}");

    // Unpinned, so each edit below is judged for what it names.
    fs::remove_file(scratch.base.join("sykli.lock")).unwrap();
    for run in [
        format!("sh {}/check.sh", scratch.base.display()),
        format!("sh {repo_path}/../check.sh"),
        // A quote that ends before the `..` does not end the path.
        format!("sh \"{repo_path}\"/../check.sh"),
        format!("sh '{repo_path}'/../check.sh"),
    ] {
        let out = validate_with_home(
            &scratch,
            &contract,
            task(run.clone(), serde_json::json!({})),
            repo,
        );
        assert_eq!(out.status.code(), Some(1), "{run}: {}", text(&out.stdout));
        assert!(text(&out.stderr).contains("names the directory"), "{run}");
    }
}

/// The task's copy of its contract inputs is its owner's alone (a 0700
/// directory, 0600 files, the owner's execute bit kept where the file has
/// one), in the temporary directory the run is given, and gone when the
/// task ends. Directories other runs left there, as a sykli that was killed
/// leaves its copy, never stand in a new run's way, and the user's own
/// copies left unchanged for over a day are removed.
#[cfg(unix)]
#[test]
fn a_tasks_copy_is_its_owners_alone_and_leftovers_never_block_it() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new("private");
    let repo = &scratch.repo;
    fs::write(scratch.config.join("check.sh"), "exit 0\n").unwrap();
    let tool = scratch.config.join("tool.sh");
    fs::write(&tool, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    let run = r#"ls -ld "$SYKLI_CONTRACT_DIR" "$SYKLI_CONTRACT_DIR/check.sh" "$SYKLI_CONTRACT_DIR/tool.sh" && sh "$SYKLI_CONTRACT_DIR/check.sh" && "$SYKLI_CONTRACT_DIR/tool.sh""#;
    let contract = scratch.contract(&helper_contract(run, &["check.sh", "tool.sh"]));
    let temporary = scratch.base.join("tmp");
    fs::create_dir(&temporary).unwrap();
    let mut left = Vec::new();
    for name in [
        "sykli-contract-0",
        "sykli-contract-1-1",
        "sykli-contract-2-4",
    ] {
        fs::create_dir(temporary.join(name)).unwrap();
        left.push(name.to_string());
    }
    // One of this user's copies unchanged for two days is stale: removed.
    let stale = temporary.join("sykli-contract-stale");
    fs::create_dir(&stale).unwrap();
    fs::write(stale.join("check.sh"), "exit 0\n").unwrap();
    let two_days_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86_400);
    fs::File::open(&stale)
        .unwrap()
        .set_modified(two_days_ago)
        .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["run", contract.to_str().unwrap(), "--json"])
        .current_dir(repo)
        .env("TMPDIR", &temporary)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let listing = value["tasks"][0]["stdout"].as_str().unwrap().to_string();
    let modes = listing
        .lines()
        .map(|line| line.split_whitespace().next().unwrap_or(""))
        .collect::<Vec<_>>();
    assert_eq!(modes.len(), 3, "{listing}");
    assert!(modes[0].starts_with("drwx------"), "{listing}");
    assert!(modes[1].starts_with("-rw-------"), "{listing}");
    assert!(modes[2].starts_with("-rwx------"), "{listing}");
    assert!(listing.contains(temporary.to_str().unwrap()), "{listing}");
    // Its own copy is gone; what others left is left as it was.
    assert_eq!(names(&temporary), left);
}

/// A file declared in `contract_inputs` is read from beside the contract:
/// its bytes and execute bits join the task's key, the receipt records it,
/// and the task reaches it as `$SYKLI_CONTRACT_DIR/NAME`.
#[test]
fn a_declared_file_beside_an_outside_contract_joins_the_key_and_the_receipt() {
    let scratch = Scratch::new("contractinputs");
    let repo = &scratch.repo;
    let script = scratch.config.join("check.sh");
    fs::write(&script, "exit 0\n").unwrap();
    let contract = scratch.contract(&helper_contract(RUNS_HELPER, &["check.sh"]));
    let path = contract.to_str().unwrap();
    let out = sykli(repo, &["validate", path]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));

    let (_, value) = receipt(&scratch, repo, &contract);
    assert_eq!(value["outcome"], "passed", "{value}");
    let recorded = value["tasks"][0]["contract_inputs"].clone();
    let record = &recorded["check.sh"];
    assert_eq!(record["sha256"].as_str(), Some(sha256(&script).as_str()));
    if cfg!(unix) {
        assert_eq!(record["mode"], 0, "{record}");
    }
    // Not in any tree, so no entry; the repository's records are as before.
    assert!(record.get("object").is_none(), "{record}");
    let inputs = value["tasks"][0]["inputs"].as_object().unwrap();
    assert_eq!(inputs.keys().collect::<Vec<_>>(), ["file.txt"]);
    // Reused, the record says what the reused pass was keyed by.
    let (_, value) = receipt(&scratch, repo, &contract);
    assert_eq!(value["outcome"], "cached", "{value}");
    assert_eq!(value["tasks"][0]["contract_inputs"], recorded);

    // Any change to the helper misses the cache.
    fs::write(&script, "# still passing\nexit 0\n").unwrap();
    let (_, value) = receipt(&scratch, repo, &contract);
    assert_eq!(value["outcome"], "passed", "{value}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let (_, value) = receipt(&scratch, repo, &contract);
        assert_eq!(value["outcome"], "passed", "{value}");
        assert_eq!(
            value["tasks"][0]["contract_inputs"]["check.sh"]["mode"],
            0o111
        );
    }
    fs::write(&script, "exit 1\n").unwrap();
    let out = sykli(repo, &["run", path]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let (_, value) = receipt(&scratch, repo, &contract);
    assert_eq!(value["outcome"], "failed", "{value}");

    // A missing one is an input error, as a missing repository input is.
    fs::remove_file(&script).unwrap();
    let (_, value) = receipt(&scratch, repo, &contract);
    assert_eq!(value["outcome"], "errored", "{value}");
    assert_eq!(value["tasks"][0]["class"], "input_error", "{value}");
    assert!(value["tasks"][0]["contract_inputs"]["check.sh"].is_null());

    // Nothing was written into the repository or beside the contract.
    assert_eq!(scratch.status(), "");
    assert_eq!(names(&scratch.config), ["sykli.json", "sykli.lock"]);
}

/// `$SYKLI_CONTRACT_DIR` holds exactly the declared files, as they were
/// hashed, so a helper cannot read an undeclared file beside the contract
/// through it: sykli could not know about that file, so it must not let a
/// pass depend on it. The directory is the task's own and is gone when the
/// task ends; a task that declares no contract inputs has none.
#[test]
fn a_task_reaches_only_the_files_beside_its_contract_that_it_declares() {
    let scratch = Scratch::new("onlydeclared");
    let repo = &scratch.repo;
    fs::write(
        scratch.config.join("check.sh"),
        ". \"$SYKLI_CONTRACT_DIR/lib.sh\"\nhelper\n",
    )
    .unwrap();
    fs::write(scratch.config.join("lib.sh"), "helper() { true; }\n").unwrap();
    let run = format!("echo \"$SYKLI_CONTRACT_DIR\"; {RUNS_HELPER}");
    let contract = scratch.contract(&helper_contract(&run, &["check.sh"]));
    let (_, value) = receipt(&scratch, repo, &contract);
    assert_eq!(value["outcome"], "failed", "{value}");

    let contract = scratch.contract(&helper_contract(&run, &["check.sh", "lib.sh"]));
    let (_, value) = receipt(&scratch, repo, &contract);
    assert_eq!(value["outcome"], "passed", "{value}");
    let private = PathBuf::from(value["tasks"][0]["stdout"].as_str().unwrap().trim());
    assert!(private.is_absolute(), "{value}");
    assert!(!private.exists(), "{} outlived its task", private.display());
    assert!(!private.starts_with(&scratch.config) && !private.starts_with(repo));
    fs::write(scratch.config.join("lib.sh"), "helper() { :; }\n").unwrap();
    let (_, value) = receipt(&scratch, repo, &contract);
    assert_eq!(value["outcome"], "passed", "{value}");

    let none = r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"test -z \"${SYKLI_CONTRACT_DIR+set}\""}]}"#;
    let contract = scratch.contract(none);
    let (_, value) = receipt(&scratch, repo, &contract);
    assert_eq!(value["outcome"], "passed", "{value}");
    assert!(
        value["tasks"][0].get("contract_inputs").is_none(),
        "{value}"
    );
}

/// `verify` reads each contract input from beside the contract as it is
/// now, with or without `--commit` (no commit carries it), and refuses a
/// receipt whose run read other bytes, or that does not record them.
#[test]
fn verify_judges_the_files_beside_an_outside_contract_as_they_are_now() {
    let scratch = Scratch::new("verifyhelper");
    let repo = &scratch.repo;
    let script = scratch.config.join("check.sh");
    fs::write(&script, "exit 0\n").unwrap();
    let contract = scratch.contract(&helper_contract(RUNS_HELPER, &["check.sh"]));
    let (path, value) = receipt(&scratch, repo, &contract);
    assert_eq!(value["outcome"], "passed", "{value}");
    for commit in [None, Some("HEAD")] {
        let out = verify(repo, &path, &contract, commit);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));
        assert!(
            text(&out.stdout).contains("ok: contract inputs"),
            "{}",
            text(&out.stdout)
        );
    }

    fs::write(&script, "exit 0 # edited after the run\n").unwrap();
    for commit in [None, Some("HEAD")] {
        let out = verify(repo, &path, &contract, commit);
        assert_eq!(out.status.code(), Some(3), "{}", text(&out.stdout));
        let reason = refusal(&out);
        assert!(reason.starts_with("refused: contract inputs"), "{reason}");
        assert!(reason.contains("check.sh"), "{reason}");
    }
    fs::write(&script, "exit 0\n").unwrap();
    assert_eq!(verify(repo, &path, &contract, None).status.code(), Some(0));

    // A receipt edited to record other bytes, or none, is refused too.
    let mut edited = value.clone();
    edited["tasks"][0]["contract_inputs"]["check.sh"]["sha256"] = "0".repeat(64).into();
    let forged = scratch.base.join("forged.json");
    fs::write(&forged, serde_json::to_vec(&edited).unwrap()).unwrap();
    let mut stripped = value.clone();
    stripped["tasks"][0]
        .as_object_mut()
        .unwrap()
        .remove("contract_inputs");
    let older = scratch.base.join("stripped.json");
    fs::write(&older, serde_json::to_vec(&stripped).unwrap()).unwrap();
    for receipt in [&forged, &older] {
        for commit in [None, Some("HEAD")] {
            let out = verify(repo, receipt, &contract, commit);
            assert_eq!(out.status.code(), Some(3), "{}", text(&out.stdout));
            assert!(refusal(&out).starts_with("refused: contract inputs"));
        }
    }
}

/// Both refusals hold together for an outside contract with contract
/// inputs. `validate` exits 1 for one whose task names the contract's
/// directory, which is a contract problem, checked first. It exits 2, as
/// `run` refuses before any task, for undeclared repository files beside a
/// declared input. `run` exits 2 for either.
#[test]
fn contract_inputs_keep_the_undeclared_file_rules_and_their_exit_codes() {
    let scratch = Scratch::new("bothrules");
    let repo = &scratch.repo;
    fs::create_dir(repo.join("fixtures")).unwrap();
    fs::write(repo.join("fixtures/a.txt"), "a\n").unwrap();
    fs::write(repo.join("fixtures/b.txt"), "b\n").unwrap();
    commit(repo, "fixtures");
    fs::write(scratch.config.join("check.sh"), "exit 0\n").unwrap();
    let contract = scratch.config.join("sykli.json");
    let path = contract.to_str().unwrap();
    let task = |run: &str| {
        serde_json::json!({"schema": "sykli-contract.v1", "tasks": [{
            "name": "check", "run": run, "inputs": ["fixtures/a.txt"],
            "contract_inputs": ["check.sh"],
        }]})
        .to_string()
    };
    fs::write(&contract, task(RUNS_HELPER)).unwrap();
    let out = sykli(repo, &["validate", path]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("fixtures/b.txt (task check)"));
    assert_eq!(sykli(repo, &["run", path]).status.code(), Some(2));

    let by_path = format!("{RUNS_HELPER}; sh '{}/check.sh'", scratch.config.display());
    fs::write(&contract, task(&by_path)).unwrap();
    let out = sykli(repo, &["validate", path]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("names the directory"));
    let out = sykli(repo, &["run", path]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("names the directory"));
}

/// Files beside a contract inside the repository are in the repository:
/// they are declared as `inputs`, and a contract input there is refused. A
/// contract input must stay inside the contract's directory.
#[test]
fn contract_inputs_are_files_inside_the_directory_of_an_outside_contract() {
    let scratch = Scratch::new("placement");
    let repo = &scratch.repo;
    fs::write(repo.join("check.sh"), "exit 0\n").unwrap();
    fs::write(
        repo.join("sykli.json"),
        helper_contract(RUNS_HELPER, &["check.sh"]),
    )
    .unwrap();
    let out = sykli(repo, &["validate", "sykli.json"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stdout));
    assert!(
        text(&out.stderr).contains("contract_inputs"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(sykli(repo, &["run", "sykli.json"]).status.code(), Some(2));

    let contract = scratch.config.join("sykli.json");
    fs::write(
        &contract,
        helper_contract(RUNS_HELPER, &["../repo/check.sh"]),
    )
    .unwrap();
    let out = sykli(repo, &["validate", contract.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stdout));
    assert!(
        text(&out.stderr).contains("must stay inside the contract's directory"),
        "{}",
        text(&out.stderr)
    );
}

/// A detached run's coordinator, and a resumed run, read the contract's
/// directory from the run's record and give the task the same files; a run
/// is not resumed once a file beside its contract changed.
#[cfg(unix)]
#[test]
fn detached_and_resumed_runs_reach_the_declared_files_beside_the_contract() {
    let scratch = Scratch::new("detachhelper");
    let repo = &scratch.repo;
    let script = scratch.config.join("check.sh");
    fs::write(&script, "exit 0\n").unwrap();
    let contract = scratch.contract(&helper_contract(RUNS_HELPER, &["check.sh"]));
    let path = contract.to_str().unwrap();
    let out = sykli(repo, &["run", path, "--detach", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let run = serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["run"]
        .as_str()
        .unwrap()
        .to_owned();
    let status = || -> serde_json::Value {
        let out = sykli(repo, &["status", "--run", &run, "--json"]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while status()["state"] != "completed" {
        assert!(std::time::Instant::now() < deadline, "{}", status());
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let done = status();
    assert_eq!(done["outcome"], "passed", "{done}");
    let kept = PathBuf::from(done["receipt"]["kept"]["path"].as_str().unwrap());
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&kept).unwrap()).unwrap();
    assert_eq!(
        value["tasks"][0]["contract_inputs"]["check.sh"]["sha256"].as_str(),
        Some(sha256(&script).as_str())
    );
    assert_eq!(
        verify(repo, &kept, &contract, Some("HEAD")).status.code(),
        Some(0)
    );

    if !cfg!(debug_assertions) {
        return;
    }
    let interrupted = |content: &str| {
        fs::write(&script, content).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
            .args(["run", path, "--json"])
            .current_dir(repo)
            .env("SYKLI_TEST_FAIL_AT", "after-dispatch")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
        text(&out.stderr)
            .lines()
            .find_map(|line| line.strip_prefix("run: ").map(str::to_owned))
            .unwrap_or_else(|| panic!("no run id: {}", text(&out.stderr)))
    };
    let run = interrupted("exit 0 # resumed\n");
    let out = sykli(repo, &["resume", "--run", &run, "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["outcome"], "passed", "{value}");
    assert!(value["tasks"][0]["contract_inputs"]["check.sh"].is_object());

    let run = interrupted("exit 0 # interrupted\n");
    fs::write(&script, "exit 0 # changed before the resume\n").unwrap();
    let out = sykli(repo, &["resume", "--run", &run]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("beside the contract"),
        "{}",
        text(&out.stderr)
    );
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

const ZEROS: &str = "0000000000000000000000000000000000000000";

/// `sykli hook pre-push [--contract CONTRACT] origin URL`, fed git's line
/// for pushing `sha` to `remote_ref`.
fn pre_push(repo: &Path, contract: Option<&Path>, sha: &str, remote_ref: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sykli"));
    command.args(["hook", "pre-push"]);
    if let Some(contract) = contract {
        command.arg("--contract").arg(contract);
    }
    stdio::output(
        command
            .args(["origin", "/nowhere/origin.git"])
            .current_dir(repo),
        format!("refs/heads/work {sha} {remote_ref} {ZEROS}\n").as_bytes(),
    )
    .unwrap()
}

#[test]
fn pre_push_with_a_contract_outside_the_repository_gates_the_default_branch() {
    let scratch = Scratch::new("prepush");
    let repo = &scratch.repo;
    let contract = scratch.contract(READS_FILE);
    let head = git(repo, &["rev-parse", "HEAD"]);
    // The repository carries no contract, so without --contract nothing gates.
    let out = pre_push(repo, None, &head, "refs/heads/main");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("not gated"));

    // A failing commit, with its honest receipt, is refused.
    fs::write(repo.join("file.txt"), "broken\n").unwrap();
    let broken = commit(repo, "broken");
    let run = sykli(repo, &["run", contract.to_str().unwrap()]);
    assert_eq!(run.status.code(), Some(1), "{}", text(&run.stderr));
    let out = pre_push(repo, Some(&contract), &broken, "refs/heads/main");
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let message = text(&out.stderr);
    assert!(message.contains("refusing to push"), "{message}");
    assert!(message.contains("outcome"), "{message}");
    assert!(
        message.contains(&format!("sykli run {}", contract.display())),
        "the refusal names the command that produces a receipt: {message}"
    );
    // Other branches are not gated.
    let out = pre_push(repo, Some(&contract), &broken, "refs/heads/feature");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));

    // A verified commit is accepted, and its receipt kept as evidence.
    fs::write(repo.join("file.txt"), "one\n").unwrap();
    let fixed = commit(repo, "fixed");
    let run = sykli(repo, &["run", contract.to_str().unwrap()]);
    assert_eq!(run.status.code(), Some(0), "{}", text(&run.stderr));
    let out = pre_push(repo, Some(&contract), &fixed, "refs/heads/main");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let kept = repo.join(".git/sykli/receipts");
    let kept = names(&kept)
        .into_iter()
        .map(|name| kept.join(name))
        .collect::<Vec<_>>();
    assert_eq!(kept.len(), 1, "{kept:?}");
    let out = verify(repo, &kept[0], &contract, Some(&fixed));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));

    // Once the person's contract changes, that receipt no longer opens the gate.
    fs::write(&contract, READS_FILE.replace("= one", "= one && true")).unwrap();
    assert!(
        sykli(repo, &["lock", contract.to_str().unwrap()])
            .status
            .success()
    );
    let out = pre_push(repo, Some(&contract), &fixed, "refs/heads/main");
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("contract expected"));

    // A contract that is not there cannot gate anything.
    let missing = scratch.config.join("missing.json");
    let out = pre_push(repo, Some(&missing), &fixed, "refs/heads/main");
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert_eq!(scratch.status(), "");
}

#[test]
fn pre_commit_with_a_contract_outside_the_repository_checks_the_staged_tree() {
    let scratch = Scratch::new("precommit");
    let repo = &scratch.repo;
    let contract = scratch.contract(READS_FILE);
    let hook = |contract: &Path| {
        sykli(
            repo,
            &[
                "hook",
                "pre-commit",
                "--contract",
                contract.to_str().unwrap(),
            ],
        )
    };
    fs::write(repo.join("file.txt"), "broken\n").unwrap();
    git(repo, &["add", "file.txt"]);
    let out = hook(&contract);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("refusing the commit"));

    fs::write(repo.join("file.txt"), "one\n").unwrap();
    fs::write(repo.join("other.txt"), "other\n").unwrap();
    git(repo, &["add", "--all"]);
    let out = hook(&contract);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("the staged tree passed"));
    // That run is the evidence the push needs.
    let pushed = commit(repo, "with other");
    let out = pre_push(repo, Some(&contract), &pushed, "refs/heads/main");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));

    let out = hook(&scratch.config.join("missing.json"));
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
}

/// The demonstration of the whole idea, with real `git push`: a hook the
/// repository never carries calls sykli with a contract it never carries.
#[cfg(unix)]
#[test]
fn a_hook_with_a_contract_outside_the_repository_gates_real_pushes() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new("realpush");
    let repo = &scratch.repo;
    let contract = scratch.contract(READS_FILE);
    let origin = scratch.base.join("origin.git");
    git(
        repo,
        &["init", "--quiet", "--bare", origin.to_str().unwrap()],
    );
    git(repo, &["remote", "add", "origin", origin.to_str().unwrap()]);
    let hook = repo.join(".git/hooks/pre-push");
    fs::create_dir_all(hook.parent().unwrap()).unwrap();
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\nexec '{}' hook pre-push --contract '{}' \"$@\"\n",
            env!("CARGO_BIN_EXE_sykli"),
            contract.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let push = || {
        Command::new("git")
            .args(["push", "--quiet", "origin", "main"])
            .current_dir(repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
    };

    fs::write(repo.join("file.txt"), "broken\n").unwrap();
    commit(repo, "broken");
    assert_eq!(
        sykli(repo, &["run", contract.to_str().unwrap()])
            .status
            .code(),
        Some(1)
    );
    let out = push();
    assert!(!out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("refusing to push"));

    fs::write(repo.join("file.txt"), "one\n").unwrap();
    commit(repo, "fixed");
    assert_eq!(
        sykli(repo, &["run", contract.to_str().unwrap()])
            .status
            .code(),
        Some(0)
    );
    let out = push();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        git(repo, &["rev-parse", "HEAD"]),
        git(&origin, &["rev-parse", "main"])
    );
    assert_eq!(scratch.status(), "");
}

#[test]
fn init_with_a_contract_path_writes_outside_the_repository_and_nothing_in_it() {
    let scratch = Scratch::new("init");
    let repo = &scratch.repo;
    for (path, contents) in [
        (
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("src/main.rs", "fn main() {}\n"),
        ("tests/it.rs", "#[test]\nfn it() {}\n"),
    ] {
        fs::create_dir_all(repo.join(path).parent().unwrap()).unwrap();
        fs::write(repo.join(path), contents).unwrap();
    }
    commit(repo, "crate");
    assert_eq!(scratch.status(), "");
    // A directory that does not exist yet, as a person's configuration
    // might lay contracts out by repository.
    let contract = scratch.config.join("owner").join("repo").join("sykli.json");
    let path = contract.to_str().unwrap();
    let out = sykli(&repo.join("src"), &["init", "--contract", path]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        names(contract.parent().unwrap()),
        ["sykli.json", "sykli.lock"]
    );
    assert!(text(&out.stdout).contains(path), "{}", text(&out.stdout));

    // Inferred from the repository as ever, with repository paths.
    let written: serde_json::Value = serde_json::from_slice(&fs::read(&contract).unwrap()).unwrap();
    let tasks = written["tasks"].as_array().unwrap();
    assert_eq!(
        tasks.iter().map(|task| &task["name"]).collect::<Vec<_>>(),
        ["fmt", "clippy", "test"]
    );
    // Every tracked file, the fixture's `file.txt` too: a test may read it.
    assert_eq!(
        tasks[2]["inputs"],
        serde_json::json!(["Cargo.toml", "file.txt", "src/main.rs", "tests/it.rs"])
    );
    // Nothing in the repository was touched: no .sykli/, no .gitignore.
    assert_eq!(scratch.status(), "");
    assert!(!repo.join(".sykli").exists());
    assert!(!repo.join(".gitignore").exists());

    // The contract it wrote is locked and complete for the Cargo gate, and
    // the gate catches a source file added later.
    let explain = |cwd: &Path| {
        let out = sykli(cwd, &["plan", path, "--explain", "--json"]);
        let plan: serde_json::Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|_| panic!("no plan: {}", text(&out.stderr)));
        // The gate reports paths with the platform's separator.
        let missing = plan["input_coverage"]["cargo_missing_inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|path| path.as_str().unwrap().replace('\\', "/"))
            .collect::<Vec<_>>();
        (out.status.code(), serde_json::json!(missing))
    };
    assert_eq!(explain(repo), (Some(0), serde_json::json!([])));
    fs::write(repo.join("src/extra.rs"), "").unwrap();
    git(repo, &["add", "src/extra.rs"]);
    assert_eq!(
        explain(repo),
        (Some(2), serde_json::json!(["src/extra.rs"]))
    );

    // An existing contract is not overwritten without --force.
    let out = sykli(repo, &["init", "--contract", path]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let out = sykli(repo, &["init", "--contract", path, "--force"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        fs::read_to_string(&contract)
            .unwrap()
            .contains("src/extra.rs")
    );

    // Without a repository there is nothing to infer from.
    let out = sykli(&scratch.config, &["init", "--contract", "elsewhere.json"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("git repository"));
    assert!(!scratch.config.join("elsewhere.json").exists());
}

/// `sykli hook pre-push --contract CONTRACT FLAGS... origin URL` for one push.
fn pre_push_flags(repo: &Path, contract: &Path, flags: &[&str], sha: &str, to: &str) -> Output {
    stdio::output(
        Command::new(env!("CARGO_BIN_EXE_sykli"))
            .args(["hook", "pre-push", "--contract"])
            .arg(contract)
            .args(flags)
            .args(["origin", "/nowhere/origin.git"])
            .current_dir(repo),
        format!("refs/heads/work {sha} {to} {ZEROS}\n").as_bytes(),
    )
    .unwrap()
}

/// Three pushes of a commit with no receipt that the guessed default branch
/// let through: each is refused once the caller names the branch it protects.
#[test]
fn naming_the_branch_refuses_the_pushes_a_guess_let_through() {
    let scratch = Scratch::new("guess");
    let repo = &scratch.repo;
    let contract = scratch.contract(READS_FILE);
    let origin = scratch.base.join("origin.git");
    git(
        repo,
        &[
            "init",
            "--quiet",
            "--bare",
            "--initial-branch=master",
            origin.to_str().unwrap(),
        ],
    );
    git(repo, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(repo, &["push", "--quiet", "origin", "main:master"]);
    git(repo, &["remote", "set-head", "origin", "--auto"]);
    // The host renames master to main; after a pruning fetch, origin/HEAD
    // still names origin/master, which is gone.
    git(&origin, &["branch", "-m", "master", "main"]);
    git(&origin, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    git(repo, &["fetch", "--quiet", "--prune", "origin"]);
    git(
        repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/master",
        ],
    );
    fs::write(repo.join("file.txt"), "one\n\n").unwrap();
    let head = commit(repo, "no receipt");
    let push = |flags: &[&str], to: &str| pre_push_flags(repo, &contract, flags, &head, to);
    let refused = |out: Output| {
        assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
        assert!(text(&out.stderr).contains("refusing to push"));
    };

    // A: the renamed default branch.
    refused(push(&["--branch", "main"], "refs/heads/main"));
    // B: no origin/HEAD at all, and the default branch is trunk.
    git(
        repo,
        &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
    );
    assert_eq!(push(&[], "refs/heads/trunk").status.code(), Some(0));
    refused(push(&["--branch", "trunk"], "refs/heads/trunk"));
    // C: the protected branch is release, not the default.
    assert_eq!(push(&[], "refs/heads/release").status.code(), Some(0));
    refused(push(&["--branch", "release"], "refs/heads/release"));
    let out = push(&["--branch", "release"], "refs/heads/main");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}

/// A refusal says what would let the push through.
#[test]
fn pre_push_refusals_name_the_step_that_helps() {
    let scratch = Scratch::new("hints");
    let repo = &scratch.repo;
    let contract = scratch.contract(READS_FILE);
    let head = git(repo, &["rev-parse", "HEAD"]);

    // Edited without re-locking: no run passes until the edit is accepted.
    fs::write(&contract, READS_FILE.replace("= one", "= one && true")).unwrap();
    let out = pre_push(repo, Some(&contract), &head, "refs/heads/main");
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let message = text(&out.stderr);
    assert!(
        message.contains(&format!("sykli lock {}", contract.display())),
        "{message}"
    );

    // A contract inside the worktree that the pushed commit does not carry,
    // ignored or untracked, can never be read by `verify --commit`.
    let local = repo.join("local");
    fs::create_dir(&local).unwrap();
    let inside = local.join("sykli.json");
    fs::write(&inside, READS_FILE).unwrap();
    assert!(
        sykli(repo, &["lock", inside.to_str().unwrap()])
            .status
            .success()
    );
    for ignored in [true, false] {
        let exclude = if ignored {
            ".sykli/\nlocal/\n"
        } else {
            ".sykli/\n"
        };
        fs::write(repo.join(".git/info/exclude"), exclude).unwrap();
        let out = pre_push(repo, Some(&inside), &head, "refs/heads/main");
        assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
        let message = text(&out.stderr);
        assert!(message.contains("outside the worktree"), "{message}");
        assert!(!message.contains("sykli run"), "{message}");
    }
}

/// The lock is the one beside the path the contract is named by. A link to a
/// contract whose lock is beside the target, not the link, runs unpinned;
/// that is rarely meant, so it is said.
#[cfg(unix)]
#[test]
fn a_linked_contract_with_its_lock_only_beside_the_target_is_warned_about() {
    let scratch = Scratch::new("link");
    let repo = &scratch.repo;
    let real = scratch.contract(READS_FILE);
    let linked = scratch.base.join("linked");
    fs::create_dir(&linked).unwrap();
    let link = linked.join("sykli.json");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let out = sykli(repo, &["run", link.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let message = text(&out.stderr);
    assert_eq!(message.matches("warning:").count(), 1, "{message}");
    assert!(message.contains("unpinned"), "{message}");

    // Linking the lock as well pins it, and there is nothing to say.
    std::os::unix::fs::symlink(scratch.config.join("sykli.lock"), linked.join("sykli.lock"))
        .unwrap();
    let out = sykli(repo, &["run", link.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        !text(&out.stderr).contains("warning:"),
        "{}",
        text(&out.stderr)
    );
}
