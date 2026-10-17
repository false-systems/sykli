//! Forged and edited receipts, judged against a commit.
//!
//! A receipt is a JSON file anyone who can write the checkout can edit; it
//! is not signed. `verify --commit` therefore reads what it can from the
//! commit itself: the tree, and each declared input's tree entry, bytes and
//! execute bit as the commit stores them. A receipt edited to name another
//! commit's tree, with one input's digest or path changed, or with the
//! commit's own entries copied in, does not verify, and an input the commit
//! cannot show at its declared path is refused, never passed unjudged. What
//! it cannot catch is spelled out here too: a failing run's receipt
//! rewritten to say "passed", with everything else left consistent,
//! verifies, because nothing in an unsigned receipt shows who ran what.

#[path = "support/stdio.rs"]
mod stdio;

#[cfg(unix)]
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::collections::BTreeMap;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
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

    /// A commit's tree, as a receipt's `tree_oid` names it.
    fn tree(&self, rev: &str) -> String {
        self.git(&["rev-parse", &format!("{rev}^{{tree}}")])
    }

    /// A commit's tree entry at `path`, `MODE ID`, as a receipt's input
    /// record names it: what a forger reads with `git ls-tree`.
    fn entry(&self, rev: &str, path: &str) -> String {
        let line = self.git(&["ls-tree", rev, "--", path]);
        let (head, _) = line.split_once('\t').expect("an entry");
        let fields = head.split(' ').collect::<Vec<_>>();
        format!("{} {}", fields[0], fields[2])
    }

    fn sykli(&self, args: &[&str], stdin: Option<&str>) -> Output {
        stdio::output(
            Command::new(env!("CARGO_BIN_EXE_sykli"))
                .args(args)
                .current_dir(&self.repo),
            stdin.unwrap_or_default().as_bytes(),
        )
        .unwrap()
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

/// Assert an exit code, showing everything verify or the hook said.
fn exits(out: &Output, code: i32, label: &str) {
    assert_eq!(
        out.status.code(),
        Some(code),
        "{label}: {}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
}

#[cfg(unix)]
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Recompute a receipt's `inputs_digest` from its own input records, by the
/// public formula: what a forger who edits a record's `sha256` or `mode`
/// does so the records still hash to the digest. (Only the symbolic-link
/// tests, which are Unix-only, need it.)
#[cfg(unix)]
fn redigest(receipt: &mut serde_json::Value) {
    let mut tasks = BTreeMap::new();
    for task in receipt["tasks"].as_array().unwrap() {
        let mut inputs = BTreeMap::new();
        for (input, record) in task["inputs"].as_object().unwrap() {
            let digest = (!record.is_null()).then(|| {
                let mode = record["mode"].as_u64().unwrap_or(0);
                let sha = record["sha256"].as_str().unwrap();
                sha256_hex(&serde_json::to_vec(&("sykli-input.v2", sha, mode)).unwrap())
            });
            inputs.insert(input.clone(), digest);
        }
        tasks.insert(task["name"].as_str().unwrap().to_string(), inputs);
    }
    receipt["subject"]["inputs_digest"] = sha256_hex(&serde_json::to_vec(&tasks).unwrap()).into();
}

/// The same receipt as a sykli without per-task input records wrote it.
fn older(receipt: &serde_json::Value) -> serde_json::Value {
    let mut older = receipt.clone();
    for task in older["tasks"].as_array_mut().unwrap() {
        task.as_object_mut().unwrap().remove("inputs");
    }
    older
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
    let previous = older(&scratch.receipt());
    let out = scratch.verify(&previous, &head);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    let mut rebound = previous.clone();
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

    // A declared input the commit does not carry (ignored): what the run
    // read there cannot be judged against the commit, so neither a current
    // receipt nor an older one verifies, and the refusal names it.
    let scratch = Scratch::new(
        "untracked",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"test -f file.txt","inputs":["file.txt","local.txt"]}]}"#,
    );
    fs::write(scratch.repo.join(".gitignore"), ".sykli/\nlocal.txt\n").unwrap();
    fs::write(scratch.repo.join("local.txt"), "local\n").unwrap();
    let head = scratch.commit("ignore local.txt");
    let genuine = scratch.receipt();
    assert_eq!(genuine["outcome"], "passed");
    let out = scratch.verify(&genuine, &head);
    exits(&out, 3, "current receipt, ignored input");
    assert!(refusal(&out).contains("local.txt"), "{}", refusal(&out));
    let out = scratch.verify(&older(&genuine), &head);
    exits(&out, 3, "older receipt, ignored input");
    assert!(refusal(&out).contains("local.txt"), "{}", refusal(&out));
    exits(&scratch.push(&genuine, &head), 1, "hook, ignored input");
}

/// A declared input that neither the run nor the commit has is judged
/// absent on both sides; one that a task declares as an output is generated
/// by the run, from inputs judged in their own right, and is reported as
/// such instead of being judged against the commit, which cannot have it.
#[test]
fn absent_and_generated_inputs_verify_as_what_they_are() {
    let scratch = Scratch::new(
        "generated",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"gen","run":"mkdir -p out && cat file.txt > out/gen.txt","inputs":["file.txt"],"outputs":["out/gen.txt"]},{"name":"use","after":["gen"],"run":"grep -q one out/gen.txt","inputs":["out/gen.txt"]}]}"#,
    );
    fs::write(scratch.repo.join(".gitignore"), ".sykli/\nout/\n").unwrap();
    let head = scratch.commit("ignore what the run writes");
    // First run: out/gen.txt does not exist yet when the run reads its inputs.
    let first = scratch.receipt();
    assert_eq!(first["outcome"], "passed", "{first}");
    assert!(first["tasks"][1]["inputs"]["out/gen.txt"].is_null());
    exits(&scratch.verify(&first, &head), 0, "absent on both sides");
    // Later runs read the output an earlier run left: generated, not refused.
    let again = scratch.receipt();
    assert!(again["tasks"][1]["inputs"]["out/gen.txt"].is_object());
    let out = scratch.verify(&again, &head);
    exits(&out, 0, "generated input");
    assert!(
        text(&out.stdout).contains("out/gen.txt (an output of task gen)"),
        "{}",
        text(&out.stdout)
    );
    exits(&scratch.push(&again, &head), 0, "hook, generated input");
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

/// sykli#78 without recomputing anything: a passing receipt for c1 with
/// only two fields edited, `tree_oid` and the input's `object`, both copied
/// from what `git rev-parse` and `git ls-tree` say about c3, where the task
/// fails. The record's `sha256` (c1's bytes) and the inputs digest are left
/// as they were, so the records still hash to the digest; only the bytes
/// the commit stores at that path can show the receipt is not c3's.
#[test]
fn a_passing_receipt_with_the_failing_commits_entries_copied_in_does_not_verify() {
    let scratch = Scratch::new("swap", READS_FILE);
    let c1_receipt = scratch.receipt();
    assert_eq!(c1_receipt["outcome"], "passed");
    fs::write(scratch.repo.join("file.txt"), "two\n").unwrap();
    let c3 = scratch.commit("c3: the task fails");
    assert_eq!(scratch.receipt()["outcome"], "failed", "c3 really fails");

    let mut forged = c1_receipt.clone();
    forged["subject"]["tree_oid"] = scratch.tree(&c3).into();
    forged["tasks"][0]["inputs"]["file.txt"]["object"] = scratch.entry(&c3, "file.txt").into();
    assert_eq!(
        forged["tasks"][0]["inputs"]["file.txt"]["sha256"],
        c1_receipt["tasks"][0]["inputs"]["file.txt"]["sha256"],
        "no digest is recomputed"
    );
    let out = scratch.verify(&forged, &c3);
    exits(&out, 3, "two-field forgery");
    assert!(refusal(&out).contains("file.txt"), "{}", refusal(&out));

    // The hook refuses the push and keeps nothing as evidence.
    let out = scratch.push(&forged, &c3);
    exits(&out, 1, "hook, two-field forgery");
    assert!(
        !scratch.repo.join(".git/sykli/receipts").exists(),
        "a refused push keeps nothing"
    );
    // The genuine receipt for c1 still verifies for c1, and is kept.
    let c1 = scratch.git(&["rev-parse", "HEAD~1"]);
    exits(&scratch.verify(&c1_receipt, &c1), 0, "genuine c1");
    exits(&scratch.push(&c1_receipt, &c1), 0, "hook, genuine c1");
    assert!(scratch.repo.join(".git/sykli/receipts").is_dir());
}

/// The same two-field forgery where a checkout converts the input: with
/// `core.autocrlf`, the run reads CRLF while git stores LF. A genuine
/// receipt verifies (its bytes are what a checkout of the blob gives); the
/// forged one does not, and the hook keeps nothing.
#[test]
fn the_two_field_forgery_is_refused_where_a_checkout_converts_line_endings() {
    let scratch = Scratch::new(
        "swap-crlf",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"grep -q one file.txt","inputs":["file.txt"]}]}"#,
    );
    scratch.git(&["config", "core.autocrlf", "true"]);
    // CRLF on disk is the LF blob c1 already stores: nothing to commit.
    fs::write(scratch.repo.join("file.txt"), "one\r\n").unwrap();
    let c1 = scratch.git(&["rev-parse", "HEAD"]);
    assert_eq!(scratch.git(&["cat-file", "-s", "HEAD:file.txt"]), "4");
    let c1_receipt = scratch.receipt();
    assert_eq!(c1_receipt["outcome"], "passed");
    let out = scratch.verify(&c1_receipt, &c1);
    exits(&out, 0, "genuine, converted");
    assert!(
        text(&out.stdout)
            .contains("the bytes of file.txt as a checkout of the commit converts them"),
        "the output says the input was judged as converted: {}",
        text(&out.stdout)
    );

    fs::write(scratch.repo.join("file.txt"), "two\r\n").unwrap();
    let c3 = scratch.commit("c3: the task fails");
    let failing = scratch.receipt();
    assert_eq!(failing["outcome"], "failed");
    exits(
        &scratch.verify(&failing, &c3),
        1,
        "c3's own receipt, converted",
    );
    let mut forged = c1_receipt.clone();
    forged["subject"]["tree_oid"] = scratch.tree(&c3).into();
    forged["tasks"][0]["inputs"]["file.txt"]["object"] = scratch.entry(&c3, "file.txt").into();
    exits(
        &scratch.verify(&forged, &c3),
        3,
        "two-field forgery, converted",
    );
    exits(&scratch.push(&forged, &c3), 1, "hook, converted");
    assert!(!scratch.repo.join(".git/sykli/receipts").exists());
}

/// The task passes only while `tool.sh` is not executable.
#[cfg(unix)]
const NOT_EXECUTABLE: &str = r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"test ! -x tool.sh","inputs":["tool.sh"]}]}"#;

/// A commit that only makes an input executable, with the same blob: the
/// receipt's `sha256` still matches, so only its execute bits (`mode`) can
/// show that the run read the file as c1 had it. Copying c3's entry into
/// `object` while `mode` stays 0, or deleting `mode`, does not verify; nor
/// does a receipt without input records (as older sykli wrote them),
/// rebound with only `tree_oid` edited.
#[cfg(unix)]
#[test]
fn an_execute_bit_copied_into_a_receipt_does_not_verify() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new("exec-swap", NOT_EXECUTABLE);
    let tool = scratch.repo.join("tool.sh");
    fs::write(&tool, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o644)).unwrap();
    let c1 = scratch.commit("c1: not executable");
    let c1_receipt = scratch.receipt();
    assert_eq!(c1_receipt["outcome"], "passed");
    assert_eq!(c1_receipt["tasks"][0]["inputs"]["tool.sh"]["mode"], 0);
    exits(&scratch.verify(&c1_receipt, &c1), 0, "genuine c1");
    exits(
        &scratch.verify(&older(&c1_receipt), &c1),
        0,
        "older genuine c1",
    );

    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    let c3 = scratch.commit("c3: chmod +x only");
    assert!(scratch.entry(&c3, "tool.sh").starts_with("100755 "));
    assert_eq!(
        scratch.entry(&c3, "tool.sh")[7..],
        scratch.entry(&c1, "tool.sh")[7..],
        "the same blob"
    );
    assert_eq!(scratch.receipt()["outcome"], "failed", "c3 really fails");

    let mut forged = c1_receipt.clone();
    forged["subject"]["tree_oid"] = scratch.tree(&c3).into();
    forged["tasks"][0]["inputs"]["tool.sh"]["object"] = scratch.entry(&c3, "tool.sh").into();
    let out = scratch.verify(&forged, &c3);
    exits(&out, 3, "object swapped, mode 0");
    assert!(refusal(&out).contains("tool.sh"), "{}", refusal(&out));
    exits(&scratch.push(&forged, &c3), 1, "hook, object swapped");
    assert!(!scratch.repo.join(".git/sykli/receipts").exists());

    // Without `mode` the digest is the same (an absent mode hashes as 0),
    // so deleting it is no edit the records can show; it is refused too.
    let mut stripped = forged.clone();
    stripped["tasks"][0]["inputs"]["tool.sh"]
        .as_object_mut()
        .unwrap()
        .remove("mode");
    exits(
        &scratch.verify(&stripped, &c3),
        3,
        "object swapped, mode deleted",
    );

    // A receipt as older sykli wrote it, with only `tree_oid` edited.
    let mut rebound = older(&c1_receipt);
    rebound["subject"]["tree_oid"] = scratch.tree(&c3).into();
    exits(&scratch.verify(&rebound, &c3), 3, "older receipt, rebound");
}

/// With `core.filemode=false` git does not take execute bits from the file
/// system, and the receipt records the bit git has, so a genuine receipt
/// verifies whatever the disk says, and the forgery above is still refused.
#[cfg(unix)]
#[test]
fn with_core_filemode_false_a_receipt_records_the_execute_bit_git_has() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new(
        "filemode",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"test -f tool.sh","inputs":["tool.sh"]}]}"#,
    );
    scratch.git(&["config", "core.filemode", "false"]);
    let tool = scratch.repo.join("tool.sh");
    fs::write(&tool, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o644)).unwrap();
    let c1 = scratch.commit("c1");
    // Executable on disk; git, told not to trust that, still has 100644.
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(scratch.git(&["status", "--porcelain"]), "");
    let genuine = scratch.receipt();
    assert_eq!(genuine["tasks"][0]["inputs"]["tool.sh"]["mode"], 0);
    exits(
        &scratch.verify(&genuine, &c1),
        0,
        "filemode=false, 755 on disk",
    );

    // git made executable (as `update-index --chmod` does), not the disk.
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o644)).unwrap();
    scratch.git(&["update-index", "--chmod=+x", "tool.sh"]);
    scratch.git(&["commit", "--quiet", "--no-verify", "-m", "c3: +x in git"]);
    let c3 = scratch.git(&["rev-parse", "HEAD"]);
    assert!(scratch.entry(&c3, "tool.sh").starts_with("100755 "));
    let c3_receipt = scratch.receipt();
    assert_eq!(c3_receipt["tasks"][0]["inputs"]["tool.sh"]["mode"], 0o111);
    exits(
        &scratch.verify(&c3_receipt, &c3),
        0,
        "filemode=false, 100755 in git",
    );

    let mut forged = genuine.clone();
    forged["subject"]["tree_oid"] = scratch.tree(&c3).into();
    forged["tasks"][0]["inputs"]["tool.sh"]["object"] = scratch.entry(&c3, "tool.sh").into();
    exits(
        &scratch.verify(&forged, &c3),
        3,
        "filemode=false, object swapped",
    );
}

/// A symbolic link as a declared input: the run reads the file it points
/// to, which the link's own tree entry (the target's name) does not show.
/// When no task declares the target, the bytes the run read cannot be
/// judged against the commit, so the receipt is refused, never passed with
/// that input unjudged: a genuine one, and one rebound across a change to
/// the target.
#[cfg(unix)]
#[test]
fn a_symbolic_link_whose_target_is_not_declared_cannot_be_judged() {
    let scratch = Scratch::new(
        "link-undeclared",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"grep -q one link.txt","inputs":["link.txt"]}]}"#,
    );
    fs::write(scratch.repo.join("data.txt"), "one\n").unwrap();
    std::os::unix::fs::symlink("data.txt", scratch.repo.join("link.txt")).unwrap();
    let c1 = scratch.commit("c1");
    let c1_receipt = scratch.receipt();
    assert_eq!(c1_receipt["outcome"], "passed");
    let out = scratch.verify(&c1_receipt, &c1);
    exits(&out, 3, "genuine, target undeclared");
    assert!(refusal(&out).contains("data.txt"), "{}", refusal(&out));

    fs::write(scratch.repo.join("data.txt"), "two\n").unwrap();
    let c3 = scratch.commit("c3: the target changes");
    let mut forged = c1_receipt.clone();
    forged["subject"]["tree_oid"] = scratch.tree(&c3).into();
    exits(
        &scratch.verify(&forged, &c3),
        3,
        "rebound, target undeclared",
    );
    exits(&scratch.push(&forged, &c3), 1, "hook, target undeclared");
    assert!(!scratch.repo.join(".git/sykli/receipts").exists());
}

/// With its target declared too, a symbolic link is judged by both: its
/// own entry (the target's name) and the bytes the run read through it,
/// which must be the target's. A genuine receipt verifies; one rebound to a
/// commit where the link points elsewhere does not, nor does one whose
/// target's record was swapped for the next commit's while the link's still
/// says what c1's target held.
#[cfg(unix)]
#[test]
fn a_symbolic_link_is_judged_by_its_entry_and_its_declared_target() {
    let scratch = Scratch::new(
        "link-declared",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"grep -q one link.txt","inputs":["link.txt","a.txt","b.txt"]}]}"#,
    );
    fs::write(scratch.repo.join("a.txt"), "one\n").unwrap();
    fs::write(scratch.repo.join("b.txt"), "two\n").unwrap();
    std::os::unix::fs::symlink("a.txt", scratch.repo.join("link.txt")).unwrap();
    let c1 = scratch.commit("link to a");
    let genuine = scratch.receipt();
    assert!(
        genuine["tasks"][0]["inputs"]["link.txt"]["object"]
            .as_str()
            .unwrap()
            .starts_with("120000 ")
    );
    exits(
        &scratch.verify(&genuine, &c1),
        0,
        "genuine, target declared",
    );

    // The link points elsewhere.
    fs::remove_file(scratch.repo.join("link.txt")).unwrap();
    std::os::unix::fs::symlink("b.txt", scratch.repo.join("link.txt")).unwrap();
    let c2 = scratch.commit("link to b");
    let mut forged = genuine.clone();
    rebind(&mut forged, &scratch.tree(&c2));
    exits(&scratch.verify(&forged, &c2), 3, "link retargeted");

    // The target changes; a forger copies c3's genuine record of a.txt and
    // c3's tree into c1's receipt and recomputes the digest, leaving the
    // link's record (c1's bytes of a.txt) as it was.
    fs::remove_file(scratch.repo.join("link.txt")).unwrap();
    std::os::unix::fs::symlink("a.txt", scratch.repo.join("link.txt")).unwrap();
    fs::write(scratch.repo.join("a.txt"), "three\n").unwrap();
    let c3 = scratch.commit("a changes");
    let c3_receipt = scratch.receipt();
    assert_eq!(c3_receipt["outcome"], "failed");
    let mut forged = genuine.clone();
    rebind(&mut forged, &scratch.tree(&c3));
    forged["tasks"][0]["inputs"]["a.txt"] = c3_receipt["tasks"][0]["inputs"]["a.txt"].clone();
    redigest(&mut forged);
    let out = scratch.verify(&forged, &c3);
    exits(&out, 3, "the link's bytes are not its target's");
    assert!(refusal(&out).contains("link.txt"), "{}", refusal(&out));
}

/// A link target is followed by its names only where the names alone
/// decide the file: a `..` may leave only the link's own directories, which
/// the commit shows are directories. Then the link is judged through its
/// target, so a rebind across a change to the target is refused.
#[cfg(unix)]
#[test]
fn a_link_target_leaving_the_links_own_directory_is_judged() {
    let scratch = Scratch::new(
        "link-up",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"grep -q one dir/link","inputs":["dir/link","x"]}]}"#,
    );
    fs::create_dir_all(scratch.repo.join("dir")).unwrap();
    fs::write(scratch.repo.join("x"), "one\n").unwrap();
    std::os::unix::fs::symlink("../x", scratch.repo.join("dir/link")).unwrap();
    let c1 = scratch.commit("c1");
    let c1_receipt = scratch.receipt();
    assert_eq!(c1_receipt["outcome"], "passed");
    exits(&scratch.verify(&c1_receipt, &c1), 0, "genuine, ../x");
    fs::write(scratch.repo.join("x"), "two\n").unwrap();
    let c3 = scratch.commit("c3: x changes");
    let mut forged = c1_receipt.clone();
    forged["subject"]["tree_oid"] = scratch.tree(&c3).into();
    exits(&scratch.verify(&forged, &c3), 3, "rebound across x");
}

/// A `..` after a name the target itself gives is resolved on disk through
/// whatever that name is: with `sub -> deep/dir`, a link to `sub/../x`
/// reads `deep/x`, not `x`. Its name cannot say which file was read, so it
/// is refused both ways: a genuine receipt, and c1's receipt rebound to a
/// commit where only `deep/x` changed (which verified, and was kept by the
/// hook, while the link's target was taken to be `x`).
#[cfg(unix)]
#[test]
fn a_link_target_with_dotdot_after_a_name_cannot_be_judged() {
    let scratch = Scratch::new(
        "link-dotdot",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"grep -q one link","inputs":["link","x"]}]}"#,
    );
    fs::create_dir_all(scratch.repo.join("deep/dir")).unwrap();
    fs::write(scratch.repo.join("deep/dir/keep"), "keep\n").unwrap();
    fs::write(scratch.repo.join("deep/x"), "one\n").unwrap();
    fs::write(scratch.repo.join("x"), "one\n").unwrap();
    std::os::unix::fs::symlink("deep/dir", scratch.repo.join("sub")).unwrap();
    std::os::unix::fs::symlink("sub/../x", scratch.repo.join("link")).unwrap();
    let c1 = scratch.commit("c1");
    let c1_receipt = scratch.receipt();
    assert_eq!(c1_receipt["outcome"], "passed");
    let out = scratch.verify(&c1_receipt, &c1);
    exits(&out, 3, "genuine, sub/../x");
    assert!(refusal(&out).contains(".."), "{}", refusal(&out));

    fs::write(scratch.repo.join("deep/x"), "two\n").unwrap();
    let c3 = scratch.commit("c3: deep/x changes");
    let c3_receipt = scratch.receipt();
    assert_eq!(c3_receipt["outcome"], "failed", "c3 really fails");
    exits(&scratch.verify(&c3_receipt, &c3), 3, "c3's own receipt");
    let mut forged = c1_receipt.clone();
    forged["subject"]["tree_oid"] = scratch.tree(&c3).into();
    exits(&scratch.verify(&forged, &c3), 3, "rebound across deep/x");
    exits(
        &scratch.push(&forged, &c3),
        1,
        "hook, rebound across deep/x",
    );
    assert!(!scratch.repo.join(".git/sykli/receipts").exists());
}

/// A converted input is compared with what a checkout of the commit makes
/// of its blob, by the commit's own attributes, not the checkout's: an
/// uncommitted `.gitattributes` in the verifier's worktree cannot turn
/// c3's LF blob into the CRLF bytes c1's run read. Attributes in
/// `.git/info/attributes` (and `core.attributesFile`) still apply: they are
/// configuration, as trusted as the filter drivers it names, and this test
/// pins that too.
#[cfg(unix)]
#[test]
fn a_converted_input_is_judged_by_the_commits_attributes() {
    let scratch = Scratch::new(
        "attr-source",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"grep -q \"$(printf 'one\\r')\" f.txt","inputs":["f.txt"]}]}"#,
    );
    fs::write(scratch.repo.join(".gitattributes"), "* -text\n").unwrap();
    fs::write(scratch.repo.join("f.txt"), "one\r\n").unwrap();
    let c1 = scratch.commit("c1: CRLF stored as is");
    let c1_receipt = scratch.receipt();
    assert_eq!(c1_receipt["outcome"], "passed");
    exits(&scratch.verify(&c1_receipt, &c1), 0, "genuine c1");
    fs::write(scratch.repo.join("f.txt"), "one\n").unwrap();
    let c3 = scratch.commit("c3: LF");
    assert_eq!(scratch.receipt()["outcome"], "failed", "c3 really fails");

    let mut forged = c1_receipt.clone();
    forged["subject"]["tree_oid"] = scratch.tree(&c3).into();
    forged["tasks"][0]["inputs"]["f.txt"]["object"] = scratch.entry(&c3, "f.txt").into();
    exits(&scratch.verify(&forged, &c3), 3, "committed attributes");
    // An uncommitted line in the worktree would make a checkout write CRLF.
    fs::write(scratch.repo.join(".gitattributes"), "f.txt text eol=crlf\n").unwrap();
    exits(
        &scratch.verify(&forged, &c3),
        3,
        "uncommitted .gitattributes",
    );
    exits(
        &scratch.push(&forged, &c3),
        1,
        "hook, uncommitted .gitattributes",
    );
    assert!(!scratch.repo.join(".git/sykli/receipts").exists());
    scratch.git(&["checkout", "--", ".gitattributes"]);
    // The same line as configuration: trusted, as documented.
    fs::write(
        scratch.repo.join(".git/info/attributes"),
        "f.txt text eol=crlf\n",
    )
    .unwrap();
    exits(
        &scratch.verify(&forged, &c3),
        0,
        ".git/info/attributes is configuration",
    );
}

/// Checking a converted input against the commit's attributes needs git
/// 2.40 (`--attr-source`). A git without it cannot do that check, and the
/// receipt is not passed: `verify --commit` exits 2 and says what is
/// needed. (An older git is stood in for by a wrapper that refuses the
/// option the way such a git does.)
#[cfg(unix)]
#[test]
fn a_converted_input_without_attr_source_fails_closed() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new(
        "old-git",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"grep -q one file.txt","inputs":["file.txt"]}]}"#,
    );
    scratch.git(&["config", "core.autocrlf", "true"]);
    fs::write(scratch.repo.join("file.txt"), "one\r\n").unwrap();
    let head = scratch.git(&["rev-parse", "HEAD"]);
    let genuine = scratch.receipt();
    assert_eq!(genuine["outcome"], "passed");
    exits(&scratch.verify(&genuine, &head), 0, "with --attr-source");

    let path = std::env::var_os("PATH").unwrap();
    let git = std::env::split_paths(&path)
        .map(|dir| dir.join("git"))
        .find(|git| git.is_file())
        .expect("git on PATH");
    let bin = scratch.base.join("old-git");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("git"),
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do case \"$a\" in --attr-source=*) \
             echo \"unknown option: $a\" >&2; exit 129;; esac; done\nexec '{}' \"$@\"\n",
            git.display()
        ),
    )
    .unwrap();
    fs::set_permissions(bin.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
    let receipt = scratch.base.join("receipt.json");
    fs::write(&receipt, serde_json::to_vec(&genuine).unwrap()).unwrap();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&path));
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args([
            "verify",
            receipt.to_str().unwrap(),
            "--contract",
            "sykli.json",
            "--commit",
            &head,
        ])
        .current_dir(&scratch.repo)
        .env("PATH", std::env::join_paths(paths).unwrap())
        .output()
        .unwrap();
    exits(&out, 2, "a git without --attr-source");
    assert!(
        text(&out.stderr).contains("git 2.40"),
        "{}",
        text(&out.stderr)
    );
}

/// A file whose bytes are a link's target name, replaced by that link: the
/// two tree entries share one blob, so copying the link's entry into a
/// receipt of the file leaves the recorded bytes equal to the blob's. A
/// link is judged only by the file it leads to, so the receipt is refused
/// (as is a run that read a link's text, as a checkout without symbolic
/// links does).
#[cfg(unix)]
#[test]
fn a_file_swapped_for_a_link_with_the_same_blob_does_not_verify() {
    let scratch = Scratch::new(
        "link-text",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"test \"$(cat x)\" = data.txt","inputs":["x","data.txt"]}]}"#,
    );
    fs::write(scratch.repo.join("x"), "data.txt").unwrap();
    fs::write(scratch.repo.join("data.txt"), "one\n").unwrap();
    let c1 = scratch.commit("c1: x holds the name");
    let c1_receipt = scratch.receipt();
    assert_eq!(c1_receipt["outcome"], "passed");
    exits(&scratch.verify(&c1_receipt, &c1), 0, "genuine c1");

    fs::remove_file(scratch.repo.join("x")).unwrap();
    std::os::unix::fs::symlink("data.txt", scratch.repo.join("x")).unwrap();
    let c3 = scratch.commit("c3: x is a link");
    assert_eq!(scratch.receipt()["outcome"], "failed", "c3 really fails");
    let link = scratch.entry(&c3, "x");
    assert_eq!(link[7..], scratch.entry(&c1, "x")[7..], "one blob");

    let mut forged = c1_receipt.clone();
    forged["subject"]["tree_oid"] = scratch.tree(&c3).into();
    forged["tasks"][0]["inputs"]["x"]["object"] = link.into();
    let out = scratch.verify(&forged, &c3);
    exits(&out, 3, "a file's record on a link's entry");
    assert!(
        refusal(&out).contains("link's own text"),
        "{}",
        refusal(&out)
    );
}

/// Declared paths are named in the commit as declared, never resolved
/// through the checkout: a tracked directory replaced, in the checkout, by
/// a symbolic link to another tracked directory whose file holds c1's bytes
/// cannot make a rebound receipt judge that other file. Whoever writes
/// receipts can usually write the checkout too, so the hook must not trust
/// it either.
#[cfg(unix)]
#[test]
fn a_symbolic_link_in_the_checkout_cannot_redirect_a_declared_input() {
    let scratch = Scratch::new(
        "redirect",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"test \"$(cat src/a.txt)\" = one","inputs":["src/a.txt"]}]}"#,
    );
    fs::create_dir_all(scratch.repo.join("src")).unwrap();
    fs::create_dir_all(scratch.repo.join("old")).unwrap();
    fs::write(scratch.repo.join("src/a.txt"), "one\n").unwrap();
    fs::write(scratch.repo.join("old/a.txt"), "one\n").unwrap();
    scratch.commit("c1");
    let c1_receipt = scratch.receipt();
    assert_eq!(c1_receipt["outcome"], "passed");
    fs::write(scratch.repo.join("src/a.txt"), "two\n").unwrap();
    let c3 = scratch.commit("c3: the task fails");
    assert_eq!(scratch.receipt()["outcome"], "failed");

    fs::remove_dir_all(scratch.repo.join("src")).unwrap();
    std::os::unix::fs::symlink("old", scratch.repo.join("src")).unwrap();
    let mut forged = c1_receipt.clone();
    forged["subject"]["tree_oid"] = scratch.tree(&c3).into();
    exits(
        &scratch.verify(&forged, &c3),
        3,
        "rebound, src linked to old",
    );
    exits(&scratch.push(&forged, &c3), 1, "hook, src linked to old");
    assert!(!scratch.repo.join(".git/sykli/receipts").exists());
}

/// A file inside a submodule: the commit records the submodule only as a
/// commit id (a gitlink), so the bytes the run read there cannot be judged
/// against it. The receipt is refused, also when rebound across a bump of
/// the submodule.
#[cfg(unix)]
#[test]
fn an_input_inside_a_submodule_cannot_be_judged() {
    let scratch = Scratch::new(
        "submodule",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"grep -q one mod/file.txt","inputs":["mod/file.txt"]}]}"#,
    );
    let sub = scratch.base.join("sub");
    fs::create_dir_all(&sub).unwrap();
    let in_sub = |args: &[&str]| {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(&sub)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
    };
    in_sub(&["init", "--quiet", "--initial-branch=main"]);
    fs::write(sub.join("file.txt"), "one\n").unwrap();
    in_sub(&["add", "file.txt"]);
    in_sub(&["commit", "--quiet", "-m", "s1"]);
    scratch.git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "--quiet",
        "add",
        sub.to_str().unwrap(),
        "mod",
    ]);
    let c1 = scratch.commit("c1: submodule");
    let c1_receipt = scratch.receipt();
    assert_eq!(c1_receipt["outcome"], "passed");
    let out = scratch.verify(&c1_receipt, &c1);
    exits(&out, 3, "genuine, inside a submodule");
    assert!(refusal(&out).contains("submodule"), "{}", refusal(&out));

    fs::write(sub.join("file.txt"), "two\n").unwrap();
    in_sub(&["commit", "--quiet", "-am", "s2"]);
    let module = scratch.repo.join("mod");
    let out = Command::new("git")
        .args(["-c", "protocol.file.allow=always", "pull", "--quiet"])
        .current_dir(&module)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    let c3 = scratch.commit("c3: bump");
    let mut forged = c1_receipt.clone();
    forged["subject"]["tree_oid"] = scratch.tree(&c3).into();
    exits(&scratch.verify(&forged, &c3), 3, "rebound across a bump");
}

/// Whether this file system finds `variant` where `name` was written (case
/// or Unicode normalization folded), probed in a fresh directory.
fn folds(base: &std::path::Path, name: &str, variant: &str) -> bool {
    let probe = base.join("probe");
    fs::create_dir_all(&probe).unwrap();
    fs::write(probe.join(name), "x").unwrap();
    let folded = probe.join(variant).exists();
    fs::remove_dir_all(&probe).unwrap();
    folded
}

/// A declared input spelled differently from how git stores it: by case
/// (`Notes.TXT` for `notes.txt`), or by Unicode normalization (NFD for the
/// NFC git stores). On a file system that folds the difference the run
/// reads git's file under the declared name; elsewhere this test makes the
/// declared spelling an ignored file of its own. Either way the commit has
/// nothing at the declared bytes, so what the run read cannot be judged:
/// the receipt is refused, genuine or rebound across a change to the file.
/// (The normalization case runs where the task shell passes non-ASCII
/// names through unchanged, so not on Windows.)
#[test]
fn an_input_spelled_differently_from_git_cannot_be_judged() {
    let nfc = "caf\u{e9}.txt";
    let nfd = "cafe\u{301}.txt";
    let mut spellings = vec![("notes.txt", "Notes.TXT", "case")];
    if cfg!(unix) {
        spellings.push((nfc, nfd, "normalization"));
    }
    for (stored, declared, label) in spellings {
        let contract = serde_json::json!({
            "schema": "sykli-contract.v1",
            "tasks": [{"name": "t", "run": format!("grep -q one '{declared}'"), "inputs": [declared]}],
        })
        .to_string();
        let scratch = Scratch::new(label, &contract);
        scratch.git(&["config", "core.precomposeunicode", "true"]);
        if !folds(&scratch.base, stored, declared) {
            let ignore = format!(".sykli/\n{declared}\n");
            fs::write(scratch.repo.join(".gitignore"), ignore).unwrap();
            fs::write(scratch.repo.join(declared), "one\n").unwrap();
        }
        fs::write(scratch.repo.join(stored), "one\n").unwrap();
        let c1 = scratch.commit("c1");
        assert!(
            scratch
                .git(&["-c", "core.quotepath=false", "ls-files"])
                .lines()
                .all(|path| path != declared && !path.contains("cafe\u{301}")),
            "{label}: git stores only its own spelling"
        );
        let c1_receipt = scratch.receipt();
        assert_eq!(c1_receipt["outcome"], "passed", "{label}");
        let out = scratch.verify(&c1_receipt, &c1);
        exits(&out, 3, &format!("{label}: genuine"));

        fs::write(scratch.repo.join(stored), "two\n").unwrap();
        let c3 = scratch.commit("c3");
        let mut forged = c1_receipt.clone();
        forged["subject"]["tree_oid"] = scratch.tree(&c3).into();
        exits(
            &scratch.verify(&forged, &c3),
            3,
            &format!("{label}: rebound"),
        );
        exits(&scratch.push(&forged, &c3), 1, &format!("{label}: hook"));
    }
}

/// What a successful verify prints after `verified:`, so a reader of the
/// output, human or agent, is told what was established and what was not.
#[test]
fn a_verified_receipt_says_what_it_establishes_and_what_it_does_not() {
    let scratch = Scratch::new("trust", READS_FILE);
    let head = scratch.git(&["rev-parse", "HEAD"]);
    let out = scratch.verify(&scratch.receipt(), &head);
    exits(&out, 0, "genuine");
    let stdout = text(&out.stdout);
    let established = stdout
        .lines()
        .find(|line| line.starts_with("established: "))
        .unwrap_or_else(|| panic!("no established line: {stdout}"));
    for claim in [
        "every declared input",
        "bytes and execute bit",
        "never from the receipt",
    ] {
        assert!(established.contains(claim), "{claim}: {established}");
    }
    // A generated input's bytes are the receipt's claim, like an outcome:
    // only the inputs of the task that produces it are established.
    assert!(
        !established.contains("output"),
        "a generated input is not established: {established}"
    );
    let not = stdout
        .lines()
        .find(|line| line.starts_with("not established: "))
        .unwrap_or_else(|| panic!("no not-established line: {stdout}"));
    for limit in [
        "who produced it",
        "outcome rewritten",
        "recomputed",
        "converted",
        "an input a task outputs",
    ] {
        assert!(not.contains(limit), "{limit}: {not}");
    }
}
