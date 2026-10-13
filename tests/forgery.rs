//! Forged and edited receipts, judged against a commit.
//!
//! A receipt is a JSON file anyone who can write the checkout can edit; it
//! is not signed. `verify --commit` therefore recomputes everything it can
//! from the commit itself: the tree, and each task's declared inputs as the
//! commit stores them. A receipt edited to name another commit's tree, or
//! with one input's digest or path changed, does not verify. What it cannot
//! catch is spelled out here too: a failing run's receipt rewritten to say
//! "passed", with everything else left consistent, verifies, because nothing
//! in an unsigned receipt shows who ran what.

use std::{
    fs,
    path::PathBuf,
    process::{Command, Output, Stdio},
};

const ZEROS: &str = "0000000000000000000000000000000000000000";

/// The task passes only while `file.txt` says `one`.
const READS_FILE: &str = r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"test \"$(cat file.txt)\" = one","inputs":["file.txt"]}]}"#;

struct Scratch {
    base: PathBuf,
    repo: PathBuf,
}

impl Scratch {
    /// A repository whose committed contract gates `file.txt`, at commit c1
    /// where the task passes.
    fn new(name: &str, contract: &str) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("sykli-forgery-{name}-{nonce}"));
        let repo = base.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let scratch = Scratch { base, repo };
        scratch.git(&["init", "--quiet", "--initial-branch=main"]);
        fs::write(scratch.repo.join("file.txt"), "one\n").unwrap();
        fs::write(scratch.repo.join(".gitignore"), ".sykli/\n").unwrap();
        fs::write(scratch.repo.join("sykli.json"), contract).unwrap();
        assert!(
            scratch
                .sykli(&["lock", "sykli.json"], None)
                .status
                .success()
        );
        scratch.commit("c1");
        scratch
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(&self.repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn commit(&self, message: &str) -> String {
        self.git(&["add", "--all"]);
        self.git(&["commit", "--quiet", "--no-verify", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn sykli(&self, args: &[&str], stdin: Option<&str>) -> Output {
        use std::io::Write;
        let mut child = Command::new(env!("CARGO_BIN_EXE_sykli"))
            .args(args)
            .current_dir(&self.repo)
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

    /// A run's receipt, as JSON.
    fn receipt(&self) -> serde_json::Value {
        let out = self.sykli(&["run", "sykli.json", "--json"], None);
        serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|_| panic!("no receipt: {}", text(&out.stderr)))
    }

    /// `verify RECEIPT --contract sykli.json --commit REV` for a receipt
    /// written outside the repository.
    fn verify(&self, receipt: &serde_json::Value, rev: &str) -> Output {
        let path = self.base.join("receipt.json");
        fs::write(&path, serde_json::to_vec_pretty(receipt).unwrap()).unwrap();
        self.sykli(
            &[
                "verify",
                path.to_str().unwrap(),
                "--contract",
                "sykli.json",
                "--commit",
                rev,
            ],
            None,
        )
    }

    /// The pre-push hook for a push of `sha` to main, with `receipt` as the
    /// only receipt in the worktree.
    fn push(&self, receipt: &serde_json::Value, sha: &str) -> Output {
        let _ = fs::remove_dir_all(self.repo.join(".sykli"));
        fs::create_dir_all(self.repo.join(".sykli")).unwrap();
        fs::write(
            self.repo.join(".sykli/receipt.json"),
            serde_json::to_vec_pretty(receipt).unwrap(),
        )
        .unwrap();
        self.sykli(
            &["hook", "pre-push", "origin", "/nowhere/origin.git"],
            Some(&format!("refs/heads/work {sha} refs/heads/main {ZEROS}\n")),
        )
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The first failing check, as verify prints it on stderr.
fn refusal(out: &Output) -> String {
    text(&out.stderr)
}

/// Point a receipt at another tree, as the forgery in the issue does.
fn rebind(receipt: &mut serde_json::Value, tree: &str) {
    receipt["subject"]["tree_oid"] = tree.into();
    receipt["subject"]["head_tree_oid"] = tree.into();
    receipt["subject"]["dirty"] = false.into();
}

/// A passing receipt for c1, edited to name c3's tree, where the task
/// fails, verified for c3 and pushed as c3. Before the fix it verified
/// (exit 0) and the push went through.
#[test]
fn a_passing_receipt_rebound_to_a_failing_commit_does_not_verify() {
    let scratch = Scratch::new("rebind", READS_FILE);
    let c1_receipt = scratch.receipt();
    assert_eq!(c1_receipt["outcome"], "passed");
    fs::write(scratch.repo.join("file.txt"), "broken\n").unwrap();
    let c3 = scratch.commit("c3: the task fails");
    let c3_tree = scratch.git(&["rev-parse", "HEAD^{tree}"]);
    let failing = scratch.receipt();
    assert_eq!(failing["outcome"], "failed", "c3 really fails");

    let mut forged = c1_receipt.clone();
    rebind(&mut forged, &c3_tree);
    let out = scratch.verify(&forged, &c3);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    assert!(refusal(&out).contains("inputs"), "{}", refusal(&out));
    let out = scratch.push(&forged, &c3);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    // The hook keeps nothing as evidence for a push it refuses.
    assert!(
        !scratch.repo.join(".git/sykli/receipts").exists(),
        "a refused push keeps nothing"
    );

    // The genuine receipt for c1 still verifies for c1.
    let c1 = scratch.git(&["rev-parse", "HEAD~1"]);
    let out = scratch.verify(&c1_receipt, &c1);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
}

/// One task's recorded input digest, one input's path, or the aggregate
/// inputs digest, edited in an otherwise genuine receipt: none verifies.
#[test]
fn an_edited_input_digest_or_path_does_not_verify() {
    let scratch = Scratch::new("inputs", READS_FILE);
    let head = scratch.git(&["rev-parse", "HEAD"]);
    let genuine = scratch.receipt();
    assert_eq!(scratch.verify(&genuine, &head).status.code(), Some(0));

    // The aggregate over all inputs.
    let mut edited = genuine.clone();
    edited["subject"]["inputs_digest"] = "0".repeat(64).into();
    let out = scratch.verify(&edited, &head);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );

    // One task's record of one input: what the run read (its SHA-256), which
    // must hash to the aggregate, and the tree entry it had, which must be
    // the commit's.
    let record = &genuine["tasks"][0]["inputs"]["file.txt"];
    assert!(record.is_object(), "per-task input records: {genuine}");
    let entry = scratch.git(&["ls-tree", "HEAD", "file.txt"]);
    let entry = entry.split('\t').next().unwrap().replacen(" blob ", " ", 1);
    assert_eq!(record["object"], entry.as_str(), "the snapshot's entry");
    for (field, value) in [
        ("sha256", "0".repeat(64)),
        ("object", format!("100644 {}", "0".repeat(40))),
    ] {
        let mut edited = genuine.clone();
        edited["tasks"][0]["inputs"]["file.txt"][field] = value.into();
        let out = scratch.verify(&edited, &head);
        assert_eq!(
            out.status.code(),
            Some(3),
            "{field}: {}{}",
            text(&out.stdout),
            text(&out.stderr)
        );
    }
    // Without --commit too: the records must hash to the inputs digest.
    let mut edited = genuine.clone();
    edited["tasks"][0]["inputs"]["file.txt"]["sha256"] = "0".repeat(64).into();
    let path = scratch.base.join("edited.json");
    fs::write(&path, serde_json::to_vec(&edited).unwrap()).unwrap();
    let out = scratch.sykli(
        &["verify", path.to_str().unwrap(), "--contract", "sykli.json"],
        None,
    );
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stdout));
    let mut edited = genuine.clone();
    edited["tasks"][0]["inputs"]["file.txt"]["object"] =
        format!("100644 {}", "0".repeat(40)).into();
    assert!(
        refusal(&scratch.verify(&edited, &head)).contains("file.txt"),
        "the refusal names the input"
    );

    // One input's path.
    let mut edited = genuine.clone();
    let inputs = edited["tasks"][0]["inputs"].as_object_mut().unwrap();
    let record = inputs.remove("file.txt").unwrap();
    inputs.insert("other.txt".into(), record);
    let out = scratch.verify(&edited, &head);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
}

/// The limit of an unsigned receipt, stated as a test of what happens today:
/// a failing run's receipt with its outcome rewritten to "passed", and every
/// field kept consistent with that, verifies for its own commit. Nothing in
/// the receipt shows that the task really ran, or who ran it; only
/// authenticated execution evidence could, and sykli has none.
#[test]
fn a_failing_receipt_rewritten_to_passed_is_not_detected() {
    let scratch = Scratch::new("outcome", READS_FILE);
    fs::write(scratch.repo.join("file.txt"), "broken\n").unwrap();
    let c3 = scratch.commit("c3: the task fails");
    let failing = scratch.receipt();
    assert_eq!(failing["outcome"], "failed");
    assert_eq!(scratch.verify(&failing, &c3).status.code(), Some(1));

    let mut forged = failing.clone();
    forged["outcome"] = "passed".into();
    for task in forged["tasks"].as_array_mut().unwrap() {
        task["outcome"] = "passed".into();
        task["exit_code"] = 0.into();
        task["error"] = serde_json::Value::Null;
        task["importable"] = true.into();
    }
    let out = scratch.verify(&forged, &c3);
    assert_eq!(
        out.status.code(),
        Some(0),
        "undetectable without authenticated execution evidence: {}",
        text(&out.stdout)
    );
}

/// Receipts written before per-task input records still verify where they
/// are genuinely consistent with the commit: their aggregate is recomputed
/// from the commit's files. When a declared input is not in the commit, an
/// older receipt cannot be checked that way, and it is refused.
#[test]
fn an_older_receipt_verifies_only_where_the_commit_bears_it_out() {
    let scratch = Scratch::new("older", READS_FILE);
    let head = scratch.git(&["rev-parse", "HEAD"]);
    let mut older = scratch.receipt();
    for task in older["tasks"].as_array_mut().unwrap() {
        task.as_object_mut().unwrap().remove("inputs");
    }
    let out = scratch.verify(&older, &head);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    let mut rebound = older.clone();
    fs::write(scratch.repo.join("file.txt"), "broken\n").unwrap();
    let c3 = scratch.commit("c3");
    rebind(&mut rebound, &scratch.git(&["rev-parse", "HEAD^{tree}"]));
    let out = scratch.verify(&rebound, &c3);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );

    // A declared input the commit does not carry (ignored, generated).
    let scratch = Scratch::new(
        "untracked",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"test -f file.txt","inputs":["file.txt","local.txt"]}]}"#,
    );
    fs::write(scratch.repo.join(".gitignore"), ".sykli/\nlocal.txt\n").unwrap();
    fs::write(scratch.repo.join("local.txt"), "local\n").unwrap();
    let head = scratch.commit("ignore local.txt");
    let genuine = scratch.receipt();
    // A current receipt names that input's digest and is checked for the rest.
    assert_eq!(scratch.verify(&genuine, &head).status.code(), Some(0));
    let mut older = genuine.clone();
    for task in older["tasks"].as_array_mut().unwrap() {
        task.as_object_mut().unwrap().remove("inputs");
    }
    let out = scratch.verify(&older, &head);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    assert!(refusal(&out).contains("local.txt"), "{}", refusal(&out));
}

/// The content a run read is compared with the commit as git stores it, or
/// as git checks it out: with `core.autocrlf`, a file with CRLF on disk is
/// stored with LF, and a genuine receipt still verifies.
#[test]
fn line_ending_conversion_does_not_refuse_a_genuine_receipt() {
    let scratch = Scratch::new(
        "crlf",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"test -s notes.txt","inputs":["notes.txt"]}]}"#,
    );
    scratch.git(&["config", "core.autocrlf", "true"]);
    fs::write(scratch.repo.join("notes.txt"), "a\r\nb\r\n").unwrap();
    let head = scratch.commit("crlf on disk");
    assert_eq!(scratch.git(&["cat-file", "-s", "HEAD:notes.txt"]), "4");
    let genuine = scratch.receipt();
    let out = scratch.verify(&genuine, &head);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
}

/// An input's execute bit is part of what the commit says: a receipt from a
/// run of an executable script, rebound to a commit where the same script is
/// not executable, does not verify.
#[cfg(unix)]
#[test]
fn an_execute_bit_the_commit_does_not_have_does_not_verify() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new(
        "mode",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"./check.sh","inputs":["check.sh"]}]}"#,
    );
    let script = scratch.repo.join("check.sh");
    fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let c1 = scratch.commit("executable");
    let genuine = scratch.receipt();
    assert_eq!(genuine["outcome"], "passed");
    assert_eq!(scratch.verify(&genuine, &c1).status.code(), Some(0));
    fs::set_permissions(&script, fs::Permissions::from_mode(0o644)).unwrap();
    let c2 = scratch.commit("not executable");
    let mut forged = genuine.clone();
    rebind(&mut forged, &scratch.git(&["rev-parse", "HEAD^{tree}"]));
    let out = scratch.verify(&forged, &c2);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
}

/// Execute bits as git sees them: a script with mode 0o744 on disk is
/// executable to git (100755), and a genuine receipt for it verifies.
#[cfg(unix)]
#[test]
fn an_owner_only_execute_bit_verifies_as_git_records_it() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new(
        "mode744",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"./check.sh","inputs":["check.sh"]}]}"#,
    );
    let script = scratch.repo.join("check.sh");
    fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o744)).unwrap();
    let head = scratch.commit("owner-executable");
    assert!(
        scratch
            .git(&["ls-tree", "HEAD", "check.sh"])
            .starts_with("100755 ")
    );
    let genuine = scratch.receipt();
    assert_eq!(genuine["tasks"][0]["inputs"]["check.sh"]["mode"], 0o100);
    let out = scratch.verify(&genuine, &head);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
}

/// A symbolic link as a declared input is judged by its tree entry, as git
/// stores a link (the target's name), while the run read the file it points
/// to. A genuine receipt verifies; one rebound to a commit where the link
/// points elsewhere does not.
#[cfg(unix)]
#[test]
fn a_symlinked_input_is_judged_by_the_link_the_commit_stores() {
    let scratch = Scratch::new(
        "symlink",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"test -s link.txt","inputs":["link.txt"]}]}"#,
    );
    fs::write(scratch.repo.join("a.txt"), "a\n").unwrap();
    fs::write(scratch.repo.join("b.txt"), "b\n").unwrap();
    std::os::unix::fs::symlink("a.txt", scratch.repo.join("link.txt")).unwrap();
    let c1 = scratch.commit("link to a");
    let genuine = scratch.receipt();
    assert!(
        genuine["tasks"][0]["inputs"]["link.txt"]["object"]
            .as_str()
            .unwrap()
            .starts_with("120000 ")
    );
    let out = scratch.verify(&genuine, &c1);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    fs::remove_file(scratch.repo.join("link.txt")).unwrap();
    std::os::unix::fs::symlink("b.txt", scratch.repo.join("link.txt")).unwrap();
    let c2 = scratch.commit("link to b");
    let mut forged = genuine.clone();
    rebind(&mut forged, &scratch.git(&["rev-parse", "HEAD^{tree}"]));
    let out = scratch.verify(&forged, &c2);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stdout));
}
