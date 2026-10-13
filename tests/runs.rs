//! Durable graph runs, interrupted for real.
//!
//! Every test here starts real `sykli` processes and stops them with real
//! signals: SIGKILL of the coordinator at a named point (the debug-build
//! hook `SYKLI_TEST_PAUSE_AT` holds it there until it is killed), SIGKILL of
//! a task's shell, SIGTERM and SIGINT of a run. After each, the run's
//! journal is read through `sykli status --run` and `sykli status
//! --commit`, and one thing is always asserted: a check whose outcome was
//! not observed and accepted is never reported as passed.
#![cfg(unix)]

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NONCE: AtomicU64 = AtomicU64::new(0);

/// A repository whose committed contract runs `tasks`, each reading
/// `file.txt`; `base` holds the latches and markers the tasks use, outside
/// the repository so they never change its tree.
struct Fixture {
    base: PathBuf,
    repo: PathBuf,
}

/// A task that waits at its latch until released, unless `BASE/go` exists,
/// and counts its runs in `BASE/NAME.count`.
fn latched(base: &Path, name: &str, then: &str) -> String {
    let b = base.display();
    format!(
        "echo $$ > '{b}/{name}.pid'; echo run >> '{b}/{name}.count'; \
         if [ ! -e '{b}/go' ]; then printf ready > '{b}/{name}.ready'; read token < '{b}/{name}.latch'; fi; {then}"
    )
}

/// A task that only counts its runs.
fn counted(base: &Path, name: &str, then: &str) -> String {
    format!("echo run >> '{}/{name}.count'; {then}", base.display())
}

impl Fixture {
    /// `tasks` as (name, command, after), each with `file.txt` as its input.
    fn new(
        name: &str,
        file: &str,
        tasks: impl Fn(&Path) -> Vec<(String, String, Vec<String>)>,
    ) -> Self {
        let base = std::env::temp_dir().join(format!(
            "sykli-runs-{name}-{}-{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&base);
        let repo = base.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let fixture = Fixture { base, repo };
        fixture.git(&["init", "--quiet", "--initial-branch=main"]);
        fs::write(fixture.repo.join("file.txt"), file).unwrap();
        fs::write(fixture.repo.join(".gitignore"), ".sykli/\n").unwrap();
        let tasks = tasks(&fixture.base)
            .into_iter()
            .map(|(name, run, after)| json!({"name": name, "run": run, "after": after, "inputs": ["file.txt"]}))
            .collect::<Vec<_>>();
        fs::write(
            fixture.repo.join("sykli.json"),
            serde_json::to_vec(&json!({"schema": "sykli-contract.v1", "tasks": tasks})).unwrap(),
        )
        .unwrap();
        let lock = fixture.sykli(&["lock", "sykli.json"]).output().unwrap();
        assert!(lock.status.success(), "{}", text(&lock.stderr));
        fixture.commit("c1");
        for name in ["first", "second", "t"] {
            mkfifo(&fixture.base.join(format!("{name}.latch")));
        }
        fixture
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(&self.repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", text(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn commit(&self, message: &str) -> String {
        self.git(&["add", "--all"]);
        self.git(&["commit", "--quiet", "--no-verify", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn sykli(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sykli"));
        command
            .args(args)
            .current_dir(&self.repo)
            .env("SYKLI_TEST_PAUSE_DIR", &self.base)
            .env_remove("SYKLI_TEST_PAUSE_AT")
            .env_remove("SYKLI_TEST_FAIL_AT")
            .stdin(Stdio::null());
        command
    }

    fn output(&self, args: &[&str]) -> Output {
        self.sykli(args).output().unwrap()
    }

    /// A command started in the background, paused at `point` when given.
    fn spawn(&self, args: &[&str], point: Option<&str>) -> Child {
        let mut command = self.sykli(args);
        if let Some(point) = point {
            mkfifo(&self.base.join(format!("{point}.release")));
            command.env("SYKLI_TEST_PAUSE_AT", point);
        }
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    /// Start `args`, wait until it reaches `point`, and SIGKILL it there.
    fn kill_at(&self, args: &[&str], point: &str) -> Output {
        let child = self.spawn(args, Some(point));
        let pid = self.wait_for_pid(point);
        signal(pid, "KILL");
        let out = child.wait_with_output().unwrap();
        assert_eq!(
            out.status.code(),
            None,
            "killed at {point}: {}",
            text(&out.stderr)
        );
        out
    }

    fn wait_for_pid(&self, point: &str) -> u32 {
        let path = self.base.join(format!("{point}.pid"));
        wait_until(|| path.exists());
        let pid = fs::read_to_string(&path).unwrap().trim().parse().unwrap();
        fs::remove_file(&path).unwrap();
        pid
    }

    /// Release a paused process or a latched task.
    fn release(&self, name: &str) {
        let path = self.base.join(name);
        // Opening a FIFO for writing waits for its reader; do it aside.
        std::thread::spawn(move || {
            let _ = fs::write(path, "go\n");
        });
    }

    fn wait_ready(&self, task: &str) {
        let path = self.base.join(format!("{task}.ready"));
        wait_until(|| path.exists());
        fs::remove_file(path).unwrap();
    }

    /// Let every latched task run straight through from now on.
    fn unlatch(&self) {
        fs::write(self.base.join("go"), "").unwrap();
    }

    fn runs_dir(&self) -> PathBuf {
        self.repo.join(".git/sykli/runs")
    }

    /// Every run this clone keeps.
    fn runs(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(self.runs_dir()) else {
            return Vec::new();
        };
        entries
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect()
    }

    /// The one run this test started.
    fn run_id(&self) -> String {
        let runs = self.runs();
        assert_eq!(runs.len(), 1, "{runs:?}");
        runs[0].clone()
    }

    fn status(&self, run: &str) -> Value {
        let out = self.output(&["status", "--run", run, "--json"]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn commit_status(&self, rev: &str) -> Value {
        let out = self.output(&[
            "status",
            "--commit",
            rev,
            "--contract",
            "sykli.json",
            "--json",
        ]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn count(&self, task: &str) -> usize {
        fs::read_to_string(self.base.join(format!("{task}.count")))
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    fn receipts(&self) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(self.repo.join(".sykli/receipts")) else {
            return Vec::new();
        };
        entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("rcpt_"))
            })
            .collect()
    }

    fn resume(&self, run: &str) -> Output {
        self.output(&["resume", "--run", run, "--json"])
    }

    fn verify_commit(&self, receipt: &Path, rev: &str) -> Output {
        self.output(&[
            "verify",
            receipt.to_str().unwrap(),
            "--contract",
            "sykli.json",
            "--commit",
            rev,
        ])
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Nothing a test started may outlive it.
        self.unlatch();
        for task in ["first", "second", "t"] {
            if let Ok(pid) = fs::read_to_string(self.base.join(format!("{task}.pid"))) {
                let _ = Command::new("kill")
                    .args(["-KILL", pid.trim()])
                    .stderr(Stdio::null())
                    .status();
            }
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// The pause and failure hooks exist only in debug builds of sykli, which
/// is what `cargo test` builds; under `cargo test --release` a test that
/// needs them says so and stops.
fn hooks_missing() -> bool {
    if cfg!(debug_assertions) {
        return false;
    }
    eprintln!("skipped: the crash hooks are compiled into debug builds only");
    true
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn mkfifo(path: &Path) {
    let _ = fs::remove_file(path);
    assert!(Command::new("mkfifo").arg(path).status().unwrap().success());
}

fn signal(pid: u32, name: &str) {
    assert!(
        Command::new("kill")
            .args([&format!("-{name}"), &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
}

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !predicate() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn task<'a>(status: &'a Value, name: &str) -> &'a Value {
    status["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["name"] == name)
        .unwrap_or_else(|| panic!("no task {name} in {status}"))
}

fn state(status: &Value, name: &str) -> String {
    task(status, name)["state"].as_str().unwrap().to_owned()
}

fn attempts(status: &Value, name: &str) -> Vec<Value> {
    task(status, name)["attempts"].as_array().unwrap().clone()
}

/// The invariant every test ends on: no task is passed (or cached) in the
/// run's status unless its acceptance is recorded, and no receipt claims a
/// pass for this commit unless every task in it was accepted.
fn nothing_unknown_passed(fixture: &Fixture, status: &Value) {
    for task in status["tasks"].as_array().unwrap() {
        if matches!(task["state"].as_str(), Some("passed")) {
            let last = task["attempts"].as_array().unwrap().last().unwrap();
            assert_eq!(last["outcome"], "passed", "{task}");
            assert_eq!(last["recorded"], true, "{task}");
            assert_eq!(last["exit"], 0, "{task}");
        }
    }
    let commit = fixture.commit_status("HEAD");
    if commit["verdict"] == "passed" {
        for entry in commit["current"].as_array().unwrap() {
            if entry["judgement"] != "verified" {
                continue;
            }
            let Some(run) = entry["run"].as_str() else {
                continue;
            };
            let journal = fixture.status(run);
            for task in journal["tasks"].as_array().unwrap() {
                assert!(
                    matches!(task["state"].as_str(), Some("passed" | "cached")),
                    "a verified receipt names a task its run did not accept: {task}"
                );
            }
        }
    }
}

fn one_task(base: &Path) -> Vec<(String, String, Vec<String>)> {
    vec![(
        "t".into(),
        latched(base, "t", "test \"$(cat file.txt)\" = one"),
        vec![],
    )]
}

fn two_tasks(base: &Path) -> Vec<(String, String, Vec<String>)> {
    vec![
        (
            "first".into(),
            counted(base, "first", "test \"$(cat file.txt)\" = one"),
            vec![],
        ),
        (
            "second".into(),
            latched(base, "second", "test \"$(cat file.txt)\" = one"),
            vec!["first".into()],
        ),
    ]
}

/// A run that completes: its receipt names its run, and its status reports
/// every field a caller polling for the run reads.
#[test]
fn a_run_names_its_run_and_its_status_has_every_field() {
    let f = Fixture::new("fields", "one\n", one_task);
    f.unlatch();
    let out = f.output(&["run", "sykli.json", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let receipt: Value = serde_json::from_slice(&out.stdout).unwrap();
    let run = receipt["run"].as_str().expect("the receipt names its run");
    assert_eq!(run, f.run_id());
    assert!(text(&out.stderr).contains(&format!("run: {run}")));

    let status = f.status(run);
    assert_eq!(status["schema"], "sykli-run-status.v1");
    assert_eq!(status["run"], run);
    assert_eq!(status["state"], "completed");
    assert_eq!(status["subject"]["tree"], receipt["subject"]["tree_oid"]);
    assert_eq!(status["subject"]["commit"], f.git(&["rev-parse", "HEAD"]));
    assert_eq!(status["subject"]["dirty"], false);
    assert_eq!(status["contract_digest"], receipt["contract_hash"]);
    assert_eq!(status["lock_digest"], receipt["lock_digest"]);
    assert_eq!(state(&status, "t"), "passed");
    let attempt = &attempts(&status, "t")[0];
    assert_eq!(attempt["id"].as_str().unwrap().len(), 64);
    assert!(attempt["started"].is_u64() && attempt["finished"].is_u64());
    assert_eq!(attempt["exit"], 0);
    assert_eq!(attempt["signal"], Value::Null);
    assert_eq!(attempt["outcome"], "passed");
    assert_eq!(attempt["supersedes"], Value::Null);
    let path = PathBuf::from(status["receipt"]["path"].as_str().unwrap());
    let bytes = fs::read(&path).unwrap();
    let digest = format!("{:x}", Sha256::digest(&bytes));
    assert_eq!(status["receipt"]["digest"], digest);
    assert_eq!(
        path.file_name().unwrap().to_str().unwrap(),
        format!("rcpt_{digest}.json")
    );
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), receipt);

    // The same tree again: reused, and said so, with no attempt.
    let again = f.output(&["run", "sykli.json", "--json"]);
    assert_eq!(again.status.code(), Some(0));
    let again: Value = serde_json::from_slice(&again.stdout).unwrap();
    let status = f.status(again["run"].as_str().unwrap());
    assert_eq!(state(&status, "t"), "cached");
    assert!(attempts(&status, "t").is_empty());
    assert_eq!(
        task(&status, "t")["reused"]["provenance"],
        path.file_name().unwrap().to_str().unwrap()
    );
    assert_eq!(f.count("t"), 1);
}

/// Window 1: killed before the run record is durable. Nothing ran and
/// nothing is claimed.
#[test]
fn window_01_killed_before_the_run_record_leaves_nothing_claimed() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("w01", "one\n", one_task);
    f.kill_at(&["run", "sykli.json", "--json"], "before-record");
    assert!(f.runs().is_empty());
    assert!(f.receipts().is_empty());
    assert_eq!(f.count("t"), 0);
    let commit = f.commit_status("HEAD");
    assert_eq!(commit["verdict"], "none");
}

/// Window 2 (invariant A): the run record is durable before any task is
/// dispatched. Killed after scheduling, before dispatch: the run is lost,
/// every task pending, nothing ran.
#[test]
fn window_02_the_run_is_recorded_before_any_task_is_dispatched() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("w02", "one\n", two_tasks);
    f.kill_at(&["run", "sykli.json", "--json"], "after-record");
    let run = f.run_id();
    let records = f.runs_dir().join(&run).join("records");
    let names = fs::read_dir(&records).unwrap().count();
    assert_eq!(names, 1, "only the request is recorded");
    let first: Value =
        serde_json::from_slice(&fs::read(records.join("00000000000000000001.json")).unwrap())
            .unwrap();
    assert_eq!(first["record"]["schema"], "sykli-run-record.v1");
    assert_eq!(first["record"]["fact"]["kind"], "requested");
    let status = f.status(&run);
    assert_eq!(status["state"], "lost");
    assert_eq!(state(&status, "first"), "pending");
    assert_eq!(state(&status, "second"), "pending");
    assert_eq!(status["receipt"], Value::Null);
    assert_eq!(f.count("first") + f.count("second"), 0);
    nothing_unknown_passed(&f, &status);
    assert_eq!(f.commit_status("HEAD")["runs"][0]["state"], "lost");
}

/// Window 3: killed right after a dispatch was recorded, before the shell
/// was spawned. The attempt is lost; it never started.
#[test]
fn window_03_a_dispatched_attempt_that_never_started_is_lost() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("w03", "one\n", one_task);
    f.kill_at(&["run", "sykli.json", "--json"], "after-dispatch");
    let status = f.status(&f.run_id());
    assert_eq!(state(&status, "t"), "lost");
    let attempt = &attempts(&status, "t")[0];
    assert_eq!(attempt["started"], Value::Null);
    assert_eq!(attempt["outcome"], "lost");
    assert_eq!(attempt["recorded"], false);
    assert_eq!(f.count("t"), 0);
    nothing_unknown_passed(&f, &status);
}

/// Window 4: killed after the worker started. Its shell lives on as an
/// orphan holding the attempt's lease, so it is running, not lost; and when
/// it then exits 0 with nobody watching, it is lost, never passed. A resume
/// is refused while it runs, and afterwards retries it as a new attempt.
#[test]
fn window_04_an_orphan_that_exits_zero_unobserved_is_lost_not_passed() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("w04", "one\n", one_task);
    f.kill_at(&["run", "sykli.json", "--json"], "after-start");
    let run = f.run_id();
    f.wait_ready("t");
    let status = f.status(&run);
    assert_eq!(status["state"], "lost");
    assert_eq!(state(&status, "t"), "running", "the orphan holds its lease");
    let refused = f.resume(&run);
    assert_eq!(refused.status.code(), Some(2));
    assert!(
        text(&refused.stderr).contains("still running"),
        "{}",
        text(&refused.stderr)
    );

    f.unlatch();
    f.release("t.latch");
    wait_until(|| state(&f.status(&run), "t") == "lost");
    let status = f.status(&run);
    nothing_unknown_passed(&f, &status);
    assert!(f.receipts().is_empty());

    let resumed = f.resume(&run);
    assert_eq!(resumed.status.code(), Some(0), "{}", text(&resumed.stderr));
    let status = f.status(&run);
    assert_eq!(status["state"], "completed");
    let tries = attempts(&status, "t");
    assert_eq!(tries.len(), 2);
    assert_eq!(tries[0]["outcome"], "lost");
    assert_eq!(tries[1]["outcome"], "passed");
    assert_eq!(tries[1]["supersedes"], tries[0]["id"]);
    assert_eq!(f.count("t"), 2);
}

/// Window 5, a lost worker: the coordinator dies during execution. The
/// task that finished keeps its accepted pass; the running one is lost; a
/// resume retries only it and keeps both attempts.
#[test]
fn window_05_a_coordinator_killed_during_execution_keeps_what_finished() {
    let f = Fixture::new("w05", "one\n", two_tasks);
    let child = f.spawn(&["run", "sykli.json", "--json"], None);
    f.wait_ready("second");
    signal(child.id(), "KILL");
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), None);
    let run = f.run_id();
    let status = f.status(&run);
    assert_eq!(state(&status, "first"), "passed");
    assert_eq!(state(&status, "second"), "running");
    f.unlatch();
    f.release("second.latch");
    wait_until(|| state(&f.status(&run), "second") == "lost");
    nothing_unknown_passed(&f, &f.status(&run));

    let resumed = f.resume(&run);
    assert_eq!(resumed.status.code(), Some(0), "{}", text(&resumed.stderr));
    let receipt: Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(receipt["run"], run);
    assert_eq!(receipt["outcome"], "passed");
    let status = f.status(&run);
    assert_eq!(status["generation"], 2);
    assert_eq!(f.count("first"), 1, "an accepted pass is kept, not re-run");
    let tries = attempts(&status, "second");
    assert_eq!(tries.len(), 2);
    assert_eq!(tries[0]["outcome"], "lost");
    assert_eq!(tries[0]["recorded"], true);
    assert_eq!(tries[1]["supersedes"], tries[0]["id"]);
    let path = status["receipt"]["path"].as_str().unwrap();
    assert_eq!(
        f.verify_commit(Path::new(path), "HEAD").status.code(),
        Some(0)
    );
}

/// Window 6: the process completed, its output is not yet kept. Lost.
#[test]
fn window_06_completed_but_output_not_kept_is_lost() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("w06", "one\n", one_task);
    f.unlatch();
    f.kill_at(&["run", "sykli.json", "--json"], "after-exit");
    let run = f.run_id();
    let status = f.status(&run);
    assert_eq!(state(&status, "t"), "lost");
    assert_eq!(f.count("t"), 1, "it ran");
    assert!(!f.runs_dir().join(&run).join("outputs").exists());
    nothing_unknown_passed(&f, &status);
}

/// Window 7: the output is kept but not accepted. A kept output is not a
/// result: the task is lost, and no receipt is written.
#[test]
fn window_07_kept_output_without_acceptance_is_not_a_result() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("w07", "one\n", one_task);
    f.unlatch();
    f.kill_at(&["run", "sykli.json", "--json"], "after-output");
    let run = f.run_id();
    let outputs = fs::read_dir(f.runs_dir().join(&run).join("outputs"))
        .unwrap()
        .filter(|entry| {
            !entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with('.')
        })
        .count();
    assert_eq!(outputs, 1);
    let status = f.status(&run);
    assert_eq!(state(&status, "t"), "lost");
    assert!(f.receipts().is_empty());
    nothing_unknown_passed(&f, &status);
}

/// Window 8: killed while the receipt is being written. Every task was
/// accepted, but there is no receipt and the run is not complete; a resume
/// writes it without running anything again.
#[test]
fn window_08_killed_during_receipt_creation() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("w08", "one\n", one_task);
    f.unlatch();
    f.kill_at(&["run", "sykli.json", "--json"], "during-receipt");
    let run = f.run_id();
    let status = f.status(&run);
    assert_eq!(status["state"], "lost");
    assert_eq!(state(&status, "t"), "passed");
    assert_eq!(status["receipt"], Value::Null);
    assert!(f.receipts().is_empty(), "a torn receipt is never a receipt");
    assert_eq!(f.commit_status("HEAD")["verdict"], "none");
    nothing_unknown_passed(&f, &status);
    let resumed = f.resume(&run);
    assert_eq!(resumed.status.code(), Some(0), "{}", text(&resumed.stderr));
    assert_eq!(f.count("t"), 1);
    assert_eq!(f.status(&run)["state"], "completed");
    assert_eq!(f.receipts().len(), 1);
    assert_eq!(f.commit_status("HEAD")["verdict"], "passed");
}

/// Window 9: the receipt is durable, the run is not yet concluded (its
/// caller was not told). The receipt holds only accepted outcomes; a resume
/// concludes the run with the very same receipt.
#[test]
fn window_09_after_the_receipt_before_the_run_concludes() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("w09", "one\n", one_task);
    f.unlatch();
    f.kill_at(&["run", "sykli.json", "--json"], "after-receipt");
    let run = f.run_id();
    let receipts = f.receipts();
    assert_eq!(receipts.len(), 1);
    let before = fs::read(&receipts[0]).unwrap();
    let status = f.status(&run);
    assert_eq!(status["state"], "lost");
    assert_eq!(state(&status, "t"), "passed");
    nothing_unknown_passed(&f, &status);
    // Until its run concludes with it, the receipt is not current evidence.
    let commit = f.commit_status("HEAD");
    assert_eq!(commit["verdict"], "none");
    assert_eq!(commit["historical"][0]["judgement"], "unconcluded");
    let resumed = f.resume(&run);
    assert_eq!(resumed.status.code(), Some(0), "{}", text(&resumed.stderr));
    let status = f.status(&run);
    assert_eq!(status["state"], "completed");
    assert_eq!(f.commit_status("HEAD")["verdict"], "passed");
    assert_eq!(f.receipts(), receipts, "the same receipt, not a second one");
    assert_eq!(fs::read(&receipts[0]).unwrap(), before);
    assert_eq!(status["receipt"]["path"], receipts[0].to_str().unwrap());
    assert_eq!(f.count("t"), 1);
}

/// Window 10: a retry interrupted again. Every attempt stays in the
/// history, each superseding the one before.
#[test]
fn window_10_a_retry_killed_again_keeps_every_attempt() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("w10", "one\n", one_task);
    f.unlatch();
    f.kill_at(&["run", "sykli.json", "--json"], "after-start");
    let run = f.run_id();
    wait_until(|| state(&f.status(&run), "t") == "lost");
    f.kill_at(&["resume", "--run", &run, "--json"], "after-dispatch");
    let status = f.status(&run);
    assert_eq!(state(&status, "t"), "lost");
    assert_eq!(attempts(&status, "t").len(), 2);
    nothing_unknown_passed(&f, &status);
    let resumed = f.resume(&run);
    assert_eq!(resumed.status.code(), Some(0), "{}", text(&resumed.stderr));
    let status = f.status(&run);
    assert_eq!(status["generation"], 3);
    let tries = attempts(&status, "t");
    assert_eq!(tries.len(), 3);
    assert_eq!(
        tries
            .iter()
            .map(|t| t["outcome"].clone())
            .collect::<Vec<_>>(),
        [json!("lost"), json!("lost"), json!("passed")]
    );
    assert_eq!(tries[1]["supersedes"], tries[0]["id"]);
    assert_eq!(tries[2]["supersedes"], tries[1]["id"]);
}

/// Window 11, cancellation: SIGTERM and SIGINT stop the run. Its running
/// task's whole process group is stopped and recorded as interrupted (not
/// failed), the run as interrupted, and the run exits 128+N.
#[test]
fn window_11_a_cancelled_run_records_its_stopped_task_as_interrupted() {
    for (signal_name, number) in [("TERM", 15), ("INT", 2)] {
        let f = Fixture::new(&format!("w11{signal_name}"), "one\n", two_tasks);
        let child = f.spawn(&["run", "sykli.json", "--json"], None);
        f.wait_ready("second");
        let task_pid: u32 = fs::read_to_string(f.base.join("second.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        signal(child.id(), signal_name);
        let out = child.wait_with_output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(128 + number),
            "{}",
            text(&out.stderr)
        );
        assert!(!alive(task_pid), "the task's process was stopped");
        let run = f.run_id();
        let status = f.status(&run);
        assert_eq!(status["state"], "interrupted");
        assert_eq!(state(&status, "first"), "passed");
        assert_eq!(state(&status, "second"), "interrupted");
        assert_eq!(attempts(&status, "second")[0]["signal"], number);
        assert!(f.receipts().is_empty());
        nothing_unknown_passed(&f, &status);
        f.unlatch();
        let resumed = f.resume(&run);
        assert_eq!(resumed.status.code(), Some(0), "{}", text(&resumed.stderr));
        let status = f.status(&run);
        assert_eq!(status["state"], "completed");
        assert_eq!(attempts(&status, "second").len(), 2);
        assert_eq!(f.count("first"), 1);
    }
}

/// Window 11, killed during the cancellation itself: after the task was
/// stopped, before that was recorded. Lost, not interrupted, never passed.
#[test]
fn window_11_killed_while_cancelling() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("w11k", "one\n", two_tasks);
    let child = f.spawn(&["run", "sykli.json", "--json"], Some("during-cancel"));
    f.wait_ready("second");
    signal(child.id(), "TERM");
    let pid = f.wait_for_pid("during-cancel");
    assert_eq!(pid, child.id());
    signal(pid, "KILL");
    child.wait_with_output().unwrap();
    let run = f.run_id();
    let status = f.status(&run);
    assert_eq!(status["state"], "lost");
    assert_eq!(state(&status, "second"), "lost");
    nothing_unknown_passed(&f, &status);
}

/// Window 12: killed during recovery, after the lost attempts were
/// recorded. The record stays true, and a second recovery completes.
#[test]
fn window_12_killed_during_recovery() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("w12", "one\n", one_task);
    f.unlatch();
    f.kill_at(&["run", "sykli.json", "--json"], "after-start");
    let run = f.run_id();
    wait_until(|| state(&f.status(&run), "t") == "lost");
    f.kill_at(&["resume", "--run", &run, "--json"], "after-lost");
    let status = f.status(&run);
    assert_eq!(status["generation"], 2);
    assert_eq!(attempts(&status, "t")[0]["recorded"], true);
    assert_eq!(state(&status, "t"), "lost");
    nothing_unknown_passed(&f, &status);
    let resumed = f.resume(&run);
    assert_eq!(resumed.status.code(), Some(0), "{}", text(&resumed.stderr));
    assert_eq!(f.status(&run)["generation"], 3);
    assert_eq!(f.status(&run)["state"], "completed");
}

/// A killed process: a task's own shell dies by SIGKILL. It is a failed
/// attempt with its signal recorded, never a pass.
#[test]
fn a_task_killed_by_a_signal_is_failed_with_its_signal() {
    let f = Fixture::new("killed", "one\n", one_task);
    let child = f.spawn(&["run", "sykli.json", "--json"], None);
    f.wait_ready("t");
    let pid: u32 = fs::read_to_string(f.base.join("t.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    signal(pid, "KILL");
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let receipt: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(receipt["outcome"], "failed");
    let status = f.status(receipt["run"].as_str().unwrap());
    assert_eq!(state(&status, "t"), "failed");
    let attempt = &attempts(&status, "t")[0];
    assert_eq!(attempt["signal"], 9);
    assert_eq!(attempt["exit"], Value::Null);
    let path = status["receipt"]["path"].as_str().unwrap();
    assert_eq!(
        f.verify_commit(Path::new(path), "HEAD").status.code(),
        Some(1)
    );
    nothing_unknown_passed(&f, &status);
}

/// Re-chain a run's records after one was added or changed, as a forger
/// with write access to the journal would.
fn rechain(records: &Path) {
    let mut paths: Vec<_> = fs::read_dir(records)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    paths.sort();
    let mut previous = Value::Null;
    for path in paths {
        let mut envelope: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        envelope["record"]["previous"] = previous;
        let mut bytes = b"sykli-run-record.v1\0".to_vec();
        bytes.extend(serde_json::to_vec(&envelope["record"]).unwrap());
        envelope["id"] = format!("{:x}", Sha256::digest(bytes)).into();
        previous = envelope["id"].clone();
        fs::write(path, serde_json::to_vec(&envelope).unwrap()).unwrap();
    }
}

/// Duplicate and conflicting completions (invariant C): a second
/// `finished` record for an attempt that already finished is refused when
/// the journal is read, identical or not, so one attempt never has two
/// accepted outcomes.
#[test]
fn a_duplicate_or_conflicting_completion_is_refused() {
    for conflicting in [false, true] {
        let f = Fixture::new("dup", "one\n", one_task);
        f.unlatch();
        assert_eq!(
            f.output(&["run", "sykli.json", "--json"]).status.code(),
            Some(0)
        );
        let run = f.run_id();
        let records = f.runs_dir().join(&run).join("records");
        let mut paths: Vec<_> = fs::read_dir(&records)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        paths.sort();
        let finished = paths
            .iter()
            .find(|path| {
                fs::read_to_string(path)
                    .unwrap()
                    .contains("\"kind\":\"finished\"")
            })
            .unwrap();
        let mut copy: Value = serde_json::from_slice(&fs::read(finished).unwrap()).unwrap();
        // Before the run's conclusion, as a replay would arrive.
        let concluded = paths.last().unwrap().clone();
        let conclusion = fs::read(&concluded).unwrap();
        let sequence = copy["record"]["sequence"].as_u64().unwrap();
        let _ = sequence;
        let next = paths.len() as u64;
        copy["record"]["sequence"] = next.into();
        if conflicting {
            copy["record"]["fact"]["outcome"] = "failed".into();
            copy["record"]["fact"]["exit_code"] = 1.into();
        }
        fs::write(&concluded, serde_json::to_vec(&copy).unwrap()).unwrap();
        let mut conclusion: Value = serde_json::from_slice(&conclusion).unwrap();
        conclusion["record"]["sequence"] = (next + 1).into();
        fs::write(
            records.join(format!("{:020}.json", next + 1)),
            serde_json::to_vec(&conclusion).unwrap(),
        )
        .unwrap();
        rechain(&records);
        let out = f.output(&["status", "--run", &run, "--json"]);
        assert_eq!(out.status.code(), Some(2));
        let expected = if conflicting {
            "conflicting terminal reports"
        } else {
            "duplicate terminal record"
        };
        assert!(
            text(&out.stderr).contains(expected),
            "{}",
            text(&out.stderr)
        );
    }
}

/// A stale completion (invariant D): coordinator A is held after its task
/// finished, before acceptance. Its lease is lost (the files removed), and
/// a resume B takes the run over, retries the task and concludes it. When A
/// then tries to accept its attempt, the journal refuses it: B's result
/// stands, and A's attempt stays lost.
#[test]
fn a_stale_coordinator_cannot_complete_an_attempt_a_newer_one_took_over() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("stale", "one\n", one_task);
    f.unlatch();
    let a = f.spawn(&["run", "sykli.json", "--json"], Some("after-output"));
    let pid = f.wait_for_pid("after-output");
    assert_eq!(pid, a.id());
    let run = f.run_id();
    let directory = f.runs_dir().join(&run);
    fs::remove_file(directory.join("lease")).unwrap();
    fs::remove_dir_all(directory.join("attempt-leases")).unwrap();
    let b = f.resume(&run);
    assert_eq!(b.status.code(), Some(0), "{}", text(&b.stderr));
    f.release("after-output.release");
    let a = a.wait_with_output().unwrap();
    assert_eq!(a.status.code(), Some(2), "{}", text(&a.stderr));
    assert!(text(&a.stderr).contains("stale"), "{}", text(&a.stderr));
    let status = f.status(&run);
    assert_eq!(status["state"], "completed");
    let tries = attempts(&status, "t");
    assert_eq!(tries.len(), 2);
    assert_eq!(tries[0]["outcome"], "lost");
    assert_eq!(tries[0]["generation"], 1);
    assert_eq!(tries[1]["outcome"], "passed");
    assert_eq!(tries[1]["generation"], 2);
    assert_eq!(f.receipts().len(), 1);
}

/// Source changed during validation: the task read other content than the
/// run recorded. Its result is about another tree, so it is errored, and
/// the receipt for the recorded commit does not pass.
#[test]
fn source_changed_during_validation_is_never_a_pass() {
    // At c1 the task fails; it is changed to pass while the run waits.
    let f = Fixture::new("drift", "two\n", one_task);
    let child = f.spawn(&["run", "sykli.json", "--json"], None);
    f.wait_ready("t");
    fs::write(f.repo.join("file.txt"), "one\n").unwrap();
    f.release("t.latch");
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let receipt: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_ne!(receipt["outcome"], "passed");
    assert_eq!(receipt["tasks"][0]["class"], "input_changed");
    fs::write(f.repo.join("file.txt"), "two\n").unwrap();
    let path = f.base.join("receipt.json");
    fs::write(&path, &out.stdout).unwrap();
    let verified = f.verify_commit(&path, "HEAD");
    assert_ne!(
        verified.status.code(),
        Some(0),
        "{}",
        text(&verified.stdout)
    );
    nothing_unknown_passed(&f, &f.status(receipt["run"].as_str().unwrap()));
}

/// Invariant E: a step that cannot be persisted is never acknowledged. The
/// task's acceptance fails to be written: the run exits 2, and the task is
/// lost, not passed.
#[test]
fn a_failed_write_is_never_acknowledged() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("fail", "one\n", one_task);
    f.unlatch();
    let out = f
        .sykli(&["run", "sykli.json", "--json"])
        .env("SYKLI_TEST_FAIL_AT", "after-output")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(out.stdout.is_empty());
    let status = f.status(&f.run_id());
    assert_eq!(state(&status, "t"), "lost");
    assert!(f.receipts().is_empty());
    nothing_unknown_passed(&f, &status);
}

/// `run --detach` answers once the run is durable, and the run outlives the
/// command that started it.
#[test]
fn a_detached_run_outlives_its_client() {
    let f = Fixture::new("detach", "one\n", one_task);
    let help = f.output(&["run", "--help"]);
    assert!(text(&help.stdout).contains("--detach"));
    let help = f.output(&["status", "--help"]);
    assert!(text(&help.stdout).contains("--run"));

    let out = f.output(&["run", "sykli.json", "--detach", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(answer["schema"], "sykli-run.v1");
    let run = answer["run"].as_str().unwrap().to_owned();
    assert_eq!(run, f.run_id());
    f.wait_ready("t");
    let status = f.status(&run);
    assert_eq!(status["state"], "running");
    assert_eq!(status["coordinator_live"], true);
    assert_eq!(state(&status, "t"), "running");
    assert_eq!(f.commit_status("HEAD")["verdict"], "pending");
    f.release("t.latch");
    wait_until(|| f.status(&run)["state"] == "completed");
    let status = f.status(&run);
    assert_eq!(state(&status, "t"), "passed");
    let path = status["receipt"]["path"].as_str().unwrap();
    let receipt: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(receipt["run"], run.as_str());
    assert_eq!(f.commit_status("HEAD")["verdict"], "passed");
}

/// A resume refuses a run whose source changed since it was requested:
/// its receipt would claim a tree that did not produce it.
#[test]
fn resume_refuses_a_changed_source() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("changed", "one\n", one_task);
    f.unlatch();
    f.kill_at(&["run", "sykli.json", "--json"], "after-dispatch");
    let run = f.run_id();
    let before = fs::read_dir(f.runs_dir().join(&run).join("records"))
        .unwrap()
        .count();
    fs::write(f.repo.join("file.txt"), "changed\n").unwrap();
    let out = f.resume(&run);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("source changed"),
        "{}",
        text(&out.stderr)
    );
    let after = fs::read_dir(f.runs_dir().join(&run).join("records"))
        .unwrap()
        .count();
    assert_eq!(before, after, "a refused resume records nothing");
    assert_eq!(f.status(&run)["state"], "lost");
}

/// `verify --json`: one document with every check, the first failing
/// stage, the exit code, and the trust tiers, with the exit code unchanged.
#[test]
fn verify_json_reports_the_verdict_and_what_it_establishes() {
    let f = Fixture::new("verifyjson", "one\n", one_task);
    f.unlatch();
    let out = f.output(&["run", "sykli.json", "--json"]);
    let path = f.base.join("receipt.json");
    fs::write(&path, &out.stdout).unwrap();
    let out = f.output(&[
        "verify",
        path.to_str().unwrap(),
        "--contract",
        "sykli.json",
        "--commit",
        "HEAD",
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let verdict: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(verdict["schema"], "sykli-verify.v1");
    assert_eq!(verdict["verified"], true);
    assert_eq!(verdict["exit"], 0);
    assert_eq!(verdict["stage"], Value::Null);
    assert_eq!(
        verdict["trust"]["established"],
        json!(["schema", "contract", "source", "consistency", "outcome"])
    );
    assert_eq!(
        verdict["trust"]["not_established"],
        json!(["provenance", "producer", "execution"])
    );
    fs::write(f.repo.join("file.txt"), "two\n").unwrap();
    let c2 = f.commit("c2");
    let out = f.output(&[
        "verify",
        path.to_str().unwrap(),
        "--contract",
        "sykli.json",
        "--commit",
        &c2,
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(3));
    let verdict: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(verdict["verified"], false);
    assert_eq!(verdict["exit"], 3);
    assert_eq!(verdict["stage"], "commit");
    assert!(
        verdict["trust"]["not_established"]
            .as_array()
            .unwrap()
            .contains(&json!("source"))
    );
}

/// `status --commit`: what applies to a commit now is kept apart from what
/// is only history. A pass for c1 is not shown as current for c2; a
/// receipt of c1's tree that no longer verifies is historical.
#[test]
fn status_commit_keeps_current_and_historical_apart() {
    let f = Fixture::new("commit", "one\n", one_task);
    f.unlatch();
    let c1 = f.git(&["rev-parse", "HEAD"]);
    let out = f.output(&["run", "sykli.json", "--json"]);
    let receipt: Value = serde_json::from_slice(&out.stdout).unwrap();
    let status = f.commit_status(&c1);
    assert_eq!(status["schema"], "sykli-commit-status.v1");
    assert_eq!(status["verdict"], "passed");
    assert_eq!(status["current"].as_array().unwrap().len(), 1);
    assert_eq!(status["current"][0]["run"], receipt["run"]);
    assert_eq!(status["runs"][0]["state"], "completed");

    // Receipts of the same tree that are not current: one claiming the run
    // whose journal concluded with another, and one that does not verify.
    let mut copy = receipt.clone();
    copy["started_at_ms"] = 1.into();
    let mut edited = receipt.clone();
    edited.as_object_mut().unwrap().remove("run");
    edited["started_at_ms"] = 2.into();
    edited["tasks"][0]["inputs"]["file.txt"]["sha256"] = "0".repeat(64).into();
    for (name, receipt) in [("copy", &copy), ("edited", &edited)] {
        fs::write(
            f.repo.join(format!(".sykli/receipts/rcpt_{name}.json")),
            serde_json::to_vec_pretty(receipt).unwrap(),
        )
        .unwrap();
    }
    let status = f.commit_status(&c1);
    assert_eq!(status["verdict"], "passed");
    assert_eq!(status["current"].as_array().unwrap().len(), 1);
    let judgements = status["historical"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["judgement"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(judgements, ["not-applicable", "unrecorded"]);

    // c2 fails; c1's pass is not current for it, and stays c1's.
    fs::write(f.repo.join("file.txt"), "two\n").unwrap();
    let c2 = f.commit("c2");
    let status = f.commit_status(&c2);
    assert_eq!(status["verdict"], "none");
    assert!(status["current"].as_array().unwrap().is_empty());
    let out = f.output(&["run", "sykli.json", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let status = f.commit_status(&c2);
    assert_eq!(status["verdict"], "failed");
    assert_eq!(status["current"][0]["judgement"], "failed");
    assert_eq!(f.commit_status(&c1)["verdict"], "passed");
}

/// Concurrent runs that store the same cache entry: each either installs
/// its entry or finds an equally valid one, never fails, and leaves one
/// valid entry and no debris. An entry that no longer verifies (its receipt
/// is gone) is replaced, not deleted in place.
#[test]
fn concurrent_runs_store_one_valid_cache_entry_without_errors() {
    let f = Fixture::new("cache", "one\n", |base| {
        vec![(
            "t".into(),
            format!(
                "while [ ! -e '{}/go' ]; do sleep 0.01; done; true",
                base.display()
            ),
            vec![],
        )]
    });
    // An entry for the key whose receipt is gone: it must be replaced.
    fs::write(f.base.join("go"), "").unwrap();
    assert_eq!(
        f.output(&["run", "sykli.json", "--json"]).status.code(),
        Some(0)
    );
    for receipt in f.receipts() {
        fs::remove_file(receipt).unwrap();
    }
    fs::remove_file(f.base.join("go")).unwrap();
    let children = (0..6)
        .map(|_| f.spawn(&["run", "sykli.json", "--json"], None))
        .collect::<Vec<_>>();
    std::thread::sleep(Duration::from_millis(300));
    fs::write(f.base.join("go"), "").unwrap();
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        assert!(
            !text(&out.stderr).contains("cache write failed"),
            "{}",
            text(&out.stderr)
        );
    }
    let cache = f.repo.join(".sykli/cache");
    let entries = fs::read_dir(&cache)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1, "{entries:?}");
    // And it hits.
    let out = f.output(&["run", "sykli.json", "--json"]);
    let receipt: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(receipt["outcome"], "cached");
}

/// The run budget: a clone keeps at most `SYKLI_RUN_BUDGET` runs, dropping
/// the oldest concluded ones first.
#[test]
fn runs_are_kept_within_their_budget() {
    let f = Fixture::new("budget", "one\n", one_task);
    f.unlatch();
    for _ in 0..4 {
        let out = f
            .sykli(&["run", "sykli.json", "--json"])
            .env("SYKLI_RUN_BUDGET", "2")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0));
    }
    assert_eq!(f.runs().len(), 2);
}

/// The journal is never written through a symbolic link, as kept receipts
/// are not.
#[test]
fn a_symlinked_run_store_is_refused() {
    let f = Fixture::new("link", "one\n", one_task);
    f.unlatch();
    let elsewhere = f.base.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, f.repo.join(".git/sykli")).unwrap();
    let out = f.output(&["run", "sykli.json", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("symbolic link"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    assert_eq!(f.count("t"), 0);
}

/// A task that exits 0 without its declared output failed, and that is a
/// recorded, accepted outcome like any other: the run completes, failed.
#[test]
fn a_task_missing_its_declared_output_is_a_recorded_failure() {
    let f = Fixture::new("output", "one\n", one_task);
    fs::write(
        f.repo.join("sykli.json"),
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"true","inputs":["file.txt"],"outputs":["made.txt"]}]}"#,
    )
    .unwrap();
    assert!(f.output(&["lock", "sykli.json"]).status.success());
    f.commit("c2");
    let out = f.output(&["run", "sykli.json", "--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let receipt: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(receipt["tasks"][0]["class"], "missing_output");
    let status = f.status(receipt["run"].as_str().unwrap());
    assert_eq!(status["state"], "completed");
    assert_eq!(state(&status, "t"), "failed");
    assert_eq!(attempts(&status, "t")[0]["exit"], 0);
    nothing_unknown_passed(&f, &status);
}

/// An execute bit changed while the run is in progress is a changed input
/// too: the bytes are the same, but not what the run recorded.
#[test]
fn an_execute_bit_changed_during_validation_is_never_a_pass() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new("drift-mode", "one\n", one_task);
    let child = f.spawn(&["run", "sykli.json", "--json"], None);
    f.wait_ready("t");
    fs::set_permissions(f.repo.join("file.txt"), fs::Permissions::from_mode(0o755)).unwrap();
    f.release("t.latch");
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let receipt: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(receipt["tasks"][0]["class"], "input_changed");
    nothing_unknown_passed(&f, &f.status(receipt["run"].as_str().unwrap()));
}
