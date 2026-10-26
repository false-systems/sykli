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
    /// When the test first signalled a sykli process (ms since the epoch).
    signalled_at: std::cell::Cell<Option<u64>>,
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
        let fixture = Fixture {
            base,
            repo,
            signalled_at: std::cell::Cell::new(None),
        };
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
        let out = own_git_config(&mut Command::new("git"))
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
        own_git_config(&mut command)
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

    /// `sh -c SCRIPT` in the repository, as a caller would start sykli
    /// (`$SYKLI`), with sykli's own test environment.
    fn shell(&self, script: &str) -> Command {
        let mut command = Command::new("sh");
        own_git_config(&mut command)
            .arg("-c")
            .arg(script)
            .current_dir(&self.repo)
            .env("SYKLI", env!("CARGO_BIN_EXE_sykli"))
            .env("SYKLI_TEST_PAUSE_DIR", &self.base)
            .env_remove("SYKLI_TEST_PAUSE_AT")
            .env_remove("SYKLI_TEST_FAIL_AT")
            .stdin(Stdio::null());
        command
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

    /// Signal a sykli process (a coordinator), noting when it was first
    /// signalled: an attempt still running then was stopped, not finished.
    fn stop(&self, pid: u32, name: &str) {
        if self.signalled_at.get().is_none() {
            self.signalled_at.set(Some(now_ms()));
        }
        signal(pid, name);
    }

    /// Start `args`, wait until it reaches `point`, and SIGKILL it there.
    fn kill_at(&self, args: &[&str], point: &str) -> Output {
        let child = self.spawn(args, Some(point));
        let pid = self.wait_for_pid(point);
        self.stop(pid, "KILL");
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

    /// Child readiness precedes the coordinator's durable Started append.
    /// Keep the child latched until the evidence this crash test needs exists.
    fn wait_started(&self, name: &str) {
        let run = self.run_id();
        wait_until(|| {
            attempts(&self.status(&run), name)
                .last()
                .is_some_and(|attempt| attempt["started"].is_u64())
        });
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

/// Git settings of the test's own, never the machine's: no global excludes
/// file, and `main` for a new repository.
fn own_git_config(command: &mut Command) -> &mut Command {
    command
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "core.excludesFile")
        .env("GIT_CONFIG_VALUE_0", "/dev/null")
        .env("GIT_CONFIG_KEY_1", "init.defaultBranch")
        .env("GIT_CONFIG_VALUE_1", "main")
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

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
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
/// run's status unless its acceptance is recorded, no attempt that was
/// still running when the test signalled sykli is passed (its exit status,
/// even 0, answers the signal, not the task's work), and no receipt claims
/// a pass for this commit unless every task in it was accepted.
fn nothing_unknown_passed(fixture: &Fixture, status: &Value) {
    for task in status["tasks"].as_array().unwrap() {
        if matches!(task["state"].as_str(), Some("passed")) {
            let last = task["attempts"].as_array().unwrap().last().unwrap();
            assert_eq!(last["outcome"], "passed", "{task}");
            assert_eq!(last["recorded"], true, "{task}");
            assert_eq!(last["exit"], 0, "{task}");
        }
        if let Some(signalled) = fixture.signalled_at.get() {
            for attempt in task["attempts"].as_array().unwrap() {
                let running = attempt["started"].as_u64().is_some_and(|at| at < signalled)
                    && attempt["finished"]
                        .as_u64()
                        .is_some_and(|at| at >= signalled);
                assert!(
                    !(running && attempt["outcome"] == "passed"),
                    "an attempt running when sykli was signalled is passed: {task}"
                );
            }
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
    f.wait_started("second");
    f.stop(child.id(), "KILL");
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
    // Invariant H: the resume said it was running `second` again, whose
    // earlier attempt had started and may have finished; `first` it kept.
    let warned = text(&resumed.stderr);
    assert!(warned.contains("re-running second"), "{warned}");
    assert!(!warned.contains("re-running first"), "{warned}");
    // Invariant G: the commit's status says `second` passed only after a
    // lost attempt, not plainly passed.
    let commit = f.commit_status("HEAD");
    assert_eq!(commit["verdict"], "passed");
    let second = commit["runs"][0]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["name"] == "second")
        .unwrap()
        .clone();
    assert_eq!(second["attempts"], json!(["lost", "passed"]), "{commit}");
    assert_eq!(
        commit["current"][0]["retried"],
        json!(["second"]),
        "{commit}"
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
    // One spelling, whatever the temporary directory is called (on macOS
    // /var is a link to /private/var): the path as the file system has it.
    let canonical = fs::canonicalize(&receipts[0]).unwrap();
    assert_eq!(status["receipt"]["path"], canonical.to_str().unwrap());
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
/// failed), the run as interrupted, and the run exits 128+N. `first`
/// passed well before the stop (an outcome accepted within 100 ms of it is
/// withdrawn: the stop may have caused it) and is kept, not run again.
#[test]
fn window_11_a_cancelled_run_records_its_stopped_task_as_interrupted() {
    for (signal_name, number) in [("TERM", 15), ("INT", 2)] {
        let f = Fixture::new(&format!("w11{signal_name}"), "one\n", two_tasks);
        let child = f.spawn(&["run", "sykli.json", "--json"], None);
        f.wait_ready("second");
        std::thread::sleep(Duration::from_millis(300));
        let task_pid: u32 = fs::read_to_string(f.base.join("second.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        f.stop(child.id(), signal_name);
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
        // What ended the attempt is recorded as its own: sykli stops a group
        // with TERM first, whatever stopped the run.
        let stopped = &attempts(&status, "second")[0];
        assert_eq!(stopped["signal"], 15, "{stopped}");
        assert_eq!(stopped["exit"], Value::Null, "{stopped}");
        assert_eq!(stopped["stopped_by"], number, "{stopped}");
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
    f.stop(child.id(), "TERM");
    let pid = f.wait_for_pid("during-cancel");
    assert_eq!(pid, child.id());
    f.stop(pid, "KILL");
    child.wait_with_output().unwrap();
    let run = f.run_id();
    let status = f.status(&run);
    assert_eq!(status["state"], "lost");
    assert_eq!(state(&status, "second"), "lost");
    nothing_unknown_passed(&f, &status);
}

/// Window 11, a task that answers the stop with exit 0 (the usual
/// `trap cleanup INT TERM` idiom): its work never finished, so its exit 0 is
/// not a pass (invariant B). An attempt whose process group sykli signalled
/// is interrupted whatever its exit status; the run stops, exits 128+N and
/// writes no receipt and no cache entry, so the next run runs the task again.
#[test]
fn window_11_a_task_that_exits_zero_on_the_stop_is_interrupted_not_passed() {
    let f = Fixture::new("w11trap", "one\n", |base| {
        let b = base.display();
        vec![(
            "t".into(),
            format!(
                "trap 'exit 0' INT TERM; echo $$ > '{b}/t.pid'; echo run >> '{b}/t.count'; \
                 if [ ! -e '{b}/go' ]; then printf ready > '{b}/t.ready'; sleep 30 & wait $!; fi; \
                 test \"$(cat file.txt)\" = one"
            ),
            vec![],
        )]
    });
    let child = f.spawn(&["run", "sykli.json", "--json"], None);
    f.wait_ready("t");
    f.stop(child.id(), "TERM");
    let out = child.wait_with_output().unwrap();
    let run = f.run_id();
    let status = f.status(&run);
    nothing_unknown_passed(&f, &status);
    assert_eq!(out.status.code(), Some(143), "{}", text(&out.stderr));
    assert!(out.stdout.is_empty(), "no receipt: {}", text(&out.stdout));
    assert_eq!(status["state"], "interrupted");
    assert_eq!(state(&status, "t"), "interrupted");
    assert!(f.receipts().is_empty());
    assert_eq!(f.commit_status("HEAD")["verdict"], "none");
    // It answered the stop with exit 0, and that is what its attempt says.
    let stopped = &attempts(&status, "t")[0];
    assert_eq!(stopped["exit"], 0, "{stopped}");
    assert_eq!(stopped["signal"], Value::Null, "{stopped}");
    assert_eq!(stopped["stopped_by"], 15, "{stopped}");

    // Nothing was cached: the next run runs the task again, and passes.
    f.unlatch();
    let again = f.output(&["run", "sykli.json", "--json"]);
    assert_eq!(again.status.code(), Some(0), "{}", text(&again.stderr));
    let receipt: Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(receipt["tasks"][0]["outcome"], "passed");
    assert_eq!(f.count("t"), 2);
}

/// Window 11, a stop while a process that left the task's group (`setsid`)
/// still holds the task's output after its shell exited 0: the stop is
/// honoured at once, not when that process lets go, and the attempt is
/// interrupted, not passed.
#[test]
fn window_11_a_stop_does_not_wait_for_a_process_that_left_the_group() {
    let f = Fixture::new("w11setsid", "one\n", |base| {
        let b = base.display();
        vec![(
            "t".into(),
            format!(
                "echo $$ > '{b}/t.pid'; \
                 perl -MPOSIX -e 'POSIX::setsid(); open F, \">\", shift; print F $$; close F; sleep 30' \
                 '{b}/holder.pid' & \
                 while [ ! -s '{b}/holder.pid' ]; do sleep 0.01; done; printf ready > '{b}/t.ready'; exit 0"
            ),
            vec![],
        )]
    });
    let child = f.spawn(&["run", "sykli.json", "--json"], None);
    f.wait_ready("t");
    let shell: u32 = fs::read_to_string(f.base.join("t.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let holder: u32 = fs::read_to_string(f.base.join("holder.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // The shell has exited (and was reaped); the holder keeps its output open.
    wait_until(|| !alive(shell));
    assert!(alive(holder));
    let mut child = child;
    let signalled = Instant::now();
    f.stop(child.id(), "TERM");
    let mut exited = None;
    while signalled.elapsed() < Duration::from_secs(3) {
        if let Some(status) = child.try_wait().unwrap() {
            exited = Some(status);
            break;
        }
        if signalled.elapsed() > Duration::from_secs(1) {
            // A second signal must be honoured too.
            let _ = Command::new("kill")
                .args(["-TERM", &child.id().to_string()])
                .status();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = Command::new("kill")
        .args(["-KILL", &holder.to_string()])
        .status();
    let elapsed = signalled.elapsed();
    let out = child.wait_with_output().unwrap();
    let status = f.status(&f.run_id());
    nothing_unknown_passed(&f, &status);
    assert!(
        exited.is_some(),
        "still running {elapsed:?} after the stop: {}",
        text(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(143), "{}", text(&out.stderr));
    assert_eq!(state(&status, "t"), "interrupted");
    assert!(f.receipts().is_empty());
}

/// Window 11, a stop that reaches the task directly as well as sykli: one
/// `kill` of sykli and of the task's group (as a service manager stopping a
/// whole control group does). The task traps TERM and exits 0, often before
/// sykli's own handler has run. Its exit answered the stop, so it is never
/// kept as a pass: the attempt is interrupted, and a resume runs the task
/// again. Repeated, since which process sees the signal first varies.
#[test]
fn window_11_a_stop_sent_to_the_task_too_is_never_a_pass() {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    for round in 0..5 {
        let f = Fixture::new(&format!("w11both{round}"), "one\n", |base| {
            let b = base.display();
            vec![(
                "t".into(),
                format!(
                    "trap 'exit 0' TERM; echo $$ > '{b}/t.pid'; echo run >> '{b}/t.count'; \
                     if [ ! -e '{b}/go' ]; then printf ready > '{b}/t.ready'; sleep 30 & wait $!; fi"
                ),
                vec![],
            )]
        });
        let child = f.spawn(&["run", "sykli.json", "--json"], None);
        f.wait_ready("t");
        let group: i32 = fs::read_to_string(f.base.join("t.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        f.signalled_at.set(Some(now_ms()));
        // SAFETY: plain kill(2) of the task's group, then of sykli.
        unsafe {
            kill(-group, 15);
            kill(child.id() as i32, 15);
        }
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(143), "{}", text(&out.stderr));
        let run = f.run_id();
        let status = f.status(&run);
        nothing_unknown_passed(&f, &status);
        assert_ne!(state(&status, "t"), "passed", "{status}");
        assert!(f.receipts().is_empty());

        f.unlatch();
        let resumed = f.resume(&run);
        assert_eq!(resumed.status.code(), Some(0), "{}", text(&resumed.stderr));
        assert_eq!(f.count("t"), 2, "the resume ran the task again");
        nothing_unknown_passed(&f, &f.status(&run));
    }
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
    // A step the journal refused is not the task erroring.
    assert!(
        !text(&out.stderr).contains("errored: t"),
        "{}",
        text(&out.stderr)
    );
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

/// A resume never takes over a run whose coordinator is alive: it exits 2
/// with `run-busy`, records nothing, and the run goes on. The words are the
/// lease's busy error (`journal::Lease::acquire`) as `Coordinator::reopen`
/// maps it, so a rename on either side alone shows here.
#[test]
fn resume_refuses_a_run_whose_coordinator_is_alive() {
    let f = Fixture::new("busy", "one\n", one_task);
    let out = f.output(&["run", "sykli.json", "--detach", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    let run = answer["run"].as_str().unwrap().to_owned();
    f.wait_ready("t");
    let records = || {
        fs::read_dir(f.runs_dir().join(&run).join("records"))
            .unwrap()
            .count()
    };
    let before = records();

    let out = f.resume(&run);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stderr),
        format!("error: run-busy: run {run}'s coordinator is still running\n")
    );
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
    assert_eq!(records(), before, "a refused resume records nothing");
    let status = f.status(&run);
    assert_eq!(status["state"], "running");
    assert_eq!(status["generation"], 1);
    assert_eq!(status["coordinator_live"], true);

    f.release("t.latch");
    wait_until(|| f.status(&run)["state"] == "completed");
    assert_eq!(state(&f.status(&run), "t"), "passed");
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
        json!(["schema", "contract", "source", "consistency"])
    );
    assert_eq!(
        verdict["trust"]["not_established"],
        json!(["outcome", "provenance", "producer", "execution"])
    );
    // The receipt's own outcome, as a field: its claim, not established.
    assert_eq!(verdict["claimed_outcome"], "passed");

    // A cached receipt claims `cached`.
    let cached = f.output(&["run", "sykli.json", "--json"]);
    let cached_path = f.base.join("cached.json");
    fs::write(&cached_path, &cached.stdout).unwrap();
    let out = f.output(&[
        "verify",
        cached_path.to_str().unwrap(),
        "--contract",
        "sykli.json",
        "--commit",
        "HEAD",
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let verdict: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(verdict["claimed_outcome"], "cached");
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
    // Refused for c2, it still claims what it claimed.
    assert_eq!(verdict["claimed_outcome"], "passed");

    // A failed run's receipt claims `failed`, and verify exits 1.
    let failed = f.output(&["run", "sykli.json", "--json"]);
    assert_eq!(failed.status.code(), Some(1), "{}", text(&failed.stderr));
    let failed_path = f.base.join("failed.json");
    fs::write(&failed_path, &failed.stdout).unwrap();
    let out = f.output(&[
        "verify",
        failed_path.to_str().unwrap(),
        "--contract",
        "sykli.json",
        "--commit",
        &c2,
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(1));
    let verdict: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(verdict["stage"], "outcome");
    assert_eq!(verdict["claimed_outcome"], "failed");

    // A receipt that cannot be read claims nothing.
    let unreadable = f.base.join("unreadable.json");
    fs::write(&unreadable, "not json").unwrap();
    let out = f.output(&[
        "verify",
        unreadable.to_str().unwrap(),
        "--contract",
        "sykli.json",
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(2));
    let verdict: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(verdict["claimed_outcome"], Value::Null);
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

/// A run's receipt path is reported in one spelling, the file system's,
/// whichever spelling of the repository a caller gave (`--repo-root` through
/// a symbolic link here, or macOS's /var for /private/var): `status --run`,
/// and `status --commit`'s receipts and runs, agree.
#[test]
fn a_receipt_path_is_reported_in_one_spelling() {
    let f = Fixture::new("spelling", "one\n", one_task);
    f.unlatch();
    let alias = f.base.join("alias");
    std::os::unix::fs::symlink(&f.repo, &alias).unwrap();
    let out = f.output(&[
        "--repo-root",
        alias.to_str().unwrap(),
        "--git-common-dir",
        alias.join(".git").to_str().unwrap(),
        "run",
        "sykli.json",
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let run = f.run_id();
    let canonical = fs::canonicalize(&f.receipts()[0]).unwrap();
    let canonical = canonical.to_str().unwrap();
    assert_eq!(f.status(&run)["receipt"]["path"], canonical);
    let commit = f.commit_status("HEAD");
    assert_eq!(commit["current"][0]["receipt"], canonical, "{commit}");
    assert_eq!(commit["runs"][0]["receipt"]["path"], canonical, "{commit}");
}

/// `run --detach` hands the run to a coordinator that keeps none of its
/// caller's descriptors: a pipe the caller left open at fd 5 (as a harness
/// does) reaches EOF as soon as `run` returns, not when the run ends.
#[test]
fn a_detached_run_keeps_none_of_its_callers_descriptors() {
    let f = Fixture::new("detachfd", "one\n", one_task);
    let mut child = f
        .shell("(\"$SYKLI\" run sykli.json --detach --json 5>&1) | cat")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The run's task is running, held at its latch.
    f.wait_ready("t");
    let started = Instant::now();
    let mut returned = None;
    while started.elapsed() < Duration::from_secs(10) {
        if let Some(status) = child.try_wait().unwrap() {
            returned = Some(status);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    f.unlatch();
    f.release("t.latch");
    let out = child.wait_with_output().unwrap();
    assert!(
        returned.is_some(),
        "the caller's pipe stayed open while the run went on: {}",
        text(&out.stderr)
    );
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(answer["schema"], "sykli-run.v1");
    let run = answer["run"].as_str().unwrap().to_owned();
    wait_until(|| f.status(&run)["state"] == "completed");
}

/// The newest recorded result for a commit's tree and contract decides its
/// verdict (invariant G): a newer failure is never hidden by an older pass
/// (a flaky check), and a pass after a failure (a rerun) passes.
#[test]
fn status_commit_lets_the_newest_recorded_result_decide() {
    let f = Fixture::new("flaky", "one\n", |base| {
        let b = base.display();
        vec![(
            "t".into(),
            format!("echo run >> '{b}/t.count'; test ! -e '{b}/fail'"),
            vec![],
        )]
    });
    let judgements = |status: &Value| {
        status["current"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["judgement"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        f.output(&["run", "sykli.json", "--json"]).status.code(),
        Some(0)
    );
    assert_eq!(f.commit_status("HEAD")["verdict"], "passed");

    fs::remove_dir_all(f.repo.join(".sykli/cache")).unwrap();
    fs::write(f.base.join("fail"), "").unwrap();
    assert_eq!(
        f.output(&["run", "sykli.json", "--json"]).status.code(),
        Some(1)
    );
    let status = f.commit_status("HEAD");
    assert_eq!(judgements(&status), ["failed", "verified"], "{status}");
    assert_eq!(status["verdict"], "failed", "{status}");

    fs::remove_file(f.base.join("fail")).unwrap();
    assert_eq!(
        f.output(&["run", "sykli.json", "--json"]).status.code(),
        Some(0)
    );
    let status = f.commit_status("HEAD");
    assert_eq!(
        judgements(&status),
        ["verified", "failed", "verified"],
        "{status}"
    );
    assert_eq!(status["verdict"], "passed", "{status}");
    assert_eq!(f.count("t"), 3);
}

/// A failure the journal recorded is never hidden by a pass no run recorded
/// (F1). A copy of the failed receipt with its outcome rewritten to passed
/// and its `run` removed verifies on its own, as documented; but the
/// commit's verdict is `conflict`, never `passed`, whichever time the copy
/// claims, before the failure or after it.
#[test]
fn status_commit_never_lets_an_unrecorded_pass_hide_a_recorded_failure() {
    for shift in [-1000_i64, 1000] {
        let f = Fixture::new("f1", "two\n", one_task);
        f.unlatch();
        let out = f.output(&["run", "sykli.json", "--json"]);
        assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
        let mut forged: Value = serde_json::from_slice(&out.stdout).unwrap();
        forged["outcome"] = "passed".into();
        for task in forged["tasks"].as_array_mut().unwrap() {
            task["outcome"] = "passed".into();
            task["exit_code"] = 0.into();
            task["error"] = Value::Null;
            task["importable"] = true.into();
            task["class"] = Value::Null;
        }
        forged.as_object_mut().unwrap().remove("run");
        forged["started_at_ms"] = (forged["started_at_ms"].as_i64().unwrap() + shift).into();
        let bytes = serde_json::to_vec_pretty(&forged).unwrap();
        let kept = f.repo.join(".git/sykli/receipts");
        fs::create_dir_all(&kept).unwrap();
        let path = kept.join(format!("rcpt_{:x}.json", Sha256::digest(&bytes)));
        fs::write(&path, &bytes).unwrap();
        // The documented limit: an outcome rewrite verifies on its own.
        assert_eq!(f.verify_commit(&path, "HEAD").status.code(), Some(0));

        let status = f.commit_status("HEAD");
        assert_eq!(status["verdict"], "conflict", "{status}");
        assert!(status["conflict"].is_string(), "{status}");
        let recorded = status["current"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| (entry["judgement"].clone(), entry["recorded"].clone()))
            .collect::<Vec<_>>();
        assert!(
            recorded.contains(&(json!("failed"), json!(true))),
            "{status}"
        );
        assert!(
            recorded.contains(&(json!("verified"), json!(false))),
            "{status}"
        );
    }
}

/// Invariant H and the lease model: a task whose process closes the
/// descriptors it inherits (as `sudo` does) holds no attempt lease. After
/// its coordinator is killed, the process group the journal recorded shows
/// it still running: it is not lost, and a resume does not run it again
/// while it runs. Once it has ended, it is lost, and a resume runs it.
#[test]
fn a_task_that_drops_its_lease_is_running_while_its_group_lives() {
    let f = Fixture::new("dropped", "one\n", |base| {
        let b = base.display();
        vec![(
            "t".into(),
            format!(
                "exec perl -MPOSIX -e 'POSIX::close($_) for 3..1023; exec @ARGV' sh -c \
                 'echo $$ > \"{b}/t.pid\"; echo run >> \"{b}/t.count\"; \
                 if [ ! -e \"{b}/go\" ]; then printf ready > \"{b}/t.ready\"; read token < \"{b}/t.latch\"; fi'"
            ),
            vec![],
        )]
    });
    let child = f.spawn(&["run", "sykli.json", "--json"], None);
    f.wait_ready("t");
    f.wait_started("t");
    f.stop(child.id(), "KILL");
    child.wait_with_output().unwrap();
    let run = f.run_id();
    let status = f.status(&run);
    assert_eq!(state(&status, "t"), "running", "{status}");
    let refused = f.resume(&run);
    assert_eq!(refused.status.code(), Some(2), "{}", text(&refused.stderr));
    assert!(
        text(&refused.stderr).contains("still running"),
        "{}",
        text(&refused.stderr)
    );
    assert!(
        text(&refused.stderr).contains("--assume-ended"),
        "{}",
        text(&refused.stderr)
    );
    assert_eq!(f.count("t"), 1);

    f.unlatch();
    f.release("t.latch");
    wait_until(|| state(&f.status(&run), "t") == "lost");
    let resumed = f.resume(&run);
    assert_eq!(resumed.status.code(), Some(0), "{}", text(&resumed.stderr));
    assert_eq!(f.count("t"), 2);
    nothing_unknown_passed(&f, &f.status(&run));
}

/// A run removed by the run budget is reported as pruned, not as an
/// unknown id; and a receipt it wrote before it concluded stays historical
/// once its journal is gone: nothing shows the run concluded with it.
#[test]
fn a_pruned_run_is_reported_and_its_unconcluded_receipt_stays_historical() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("pruned", "one\n", one_task);
    f.unlatch();
    let c1 = f.git(&["rev-parse", "HEAD"]);
    f.kill_at(&["run", "sykli.json", "--json"], "after-receipt");
    let lost = f.run_id();
    assert_eq!(
        f.commit_status(&c1)["historical"][0]["judgement"],
        "unconcluded"
    );
    // Another commit's run, under a budget of one, removes it.
    fs::write(f.repo.join("file.txt"), "two\n").unwrap();
    f.commit("c2");
    f.sykli(&["run", "sykli.json", "--json"])
        .env("SYKLI_RUN_BUDGET", "1")
        .output()
        .unwrap();
    assert!(!f.runs().contains(&lost));
    let out = f.output(&["status", "--run", &lost, "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("pruned"),
        "{}",
        text(&out.stderr)
    );

    let status = f.commit_status(&c1);
    assert_eq!(status["verdict"], "none", "{status}");
    assert_eq!(status["historical"][0]["judgement"], "unconcluded");
}

/// A run whose journal cannot be read is named, never skipped silently, and
/// a receipt naming it is not current: nothing shows it concluded with it.
#[test]
fn an_unreadable_run_is_named_and_its_receipt_is_not_current() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("unreadable", "one\n", one_task);
    f.unlatch();
    f.kill_at(&["run", "sykli.json", "--json"], "after-receipt");
    let run = f.run_id();
    fs::write(
        f.runs_dir()
            .join(&run)
            .join("records/00000000000000000001.json"),
        "garbage",
    )
    .unwrap();
    let status = f.commit_status("HEAD");
    assert_eq!(status["verdict"], "none", "{status}");
    assert_eq!(status["historical"][0]["judgement"], "unconcluded");
    assert_eq!(
        status["unreadable_runs"][0]["run"],
        run.as_str(),
        "{status}"
    );
}

/// A journal that does not load says which file it could not read.
#[test]
fn journal_errors_name_the_file() {
    let f = Fixture::new("journalerror", "one\n", one_task);
    f.unlatch();
    assert_eq!(
        f.output(&["run", "sykli.json", "--json"]).status.code(),
        Some(0)
    );
    let run = f.run_id();
    fs::write(
        f.runs_dir()
            .join(&run)
            .join("records/00000000000000000002.json"),
        "garbage",
    )
    .unwrap();
    let out = f.output(&["status", "--run", &run, "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("00000000000000000002.json"),
        "{}",
        text(&out.stderr)
    );
}

/// `resume --help` says what a resume promises: at least once.
#[test]
fn resume_help_says_tasks_run_at_least_once() {
    let f = Fixture::new("resumehelp", "one\n", one_task);
    let help = text(&f.output(&["resume", "--help"]).stdout);
    assert!(help.contains("at least once"), "{help}");
}

unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

/// A task that traps TERM and exits 0 (its "cleanup"), holding until it
/// is stopped unless `BASE/go` exists; it counts its runs in `BASE/NAME.count`
/// and writes its pid, its group's id, to `BASE/NAME.pid`.
fn trapping(base: &Path, name: &str, then: &str) -> String {
    let b = base.display();
    format!(
        "trap 'exit 0' TERM; echo $$ > '{b}/{name}.pid'; echo run >> '{b}/{name}.count'; \
         if [ ! -e '{b}/go' ]; then printf ready > '{b}/{name}.ready'; sleep 30 & wait $!; fi; {then}"
    )
}

fn pid_of(f: &Fixture, task: &str) -> i32 {
    fs::read_to_string(f.base.join(format!("{task}.pid")))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// Invariant B for the last task of a run: the stop reaches its group first
/// and sykli only later (two `kill`s, as a supervisor stopping children
/// before their parent does). The task traps it and exits 0, and nothing else
/// is left to run; sykli must not conclude with that pass before the window
/// in which its own stop may still arrive is over. (The window is widened
/// here, debug builds only, so the test does not hang on scheduling.)
#[test]
fn window_11_a_stop_after_the_last_task_exited_is_never_a_pass() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("w11last", "one\n", |base| {
        vec![("t".into(), trapping(base, "t", "true"), vec![])]
    });
    let child = f
        .sykli(&["run", "sykli.json", "--json"])
        .env("SYKLI_TEST_STOP_WINDOW_MS", "3000")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    f.wait_ready("t");
    let group = pid_of(&f, "t");
    f.signalled_at.set(Some(now_ms()));
    // SAFETY: plain kill(2) of the task's group.
    unsafe {
        kill(-group, 15);
    }
    std::thread::sleep(Duration::from_millis(300));
    // SAFETY: plain kill(2) of sykli, which may have exited already.
    unsafe {
        kill(child.id() as i32, 15);
    }
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(143), "{}", text(&out.stderr));
    let run = f.run_id();
    let status = f.status(&run);
    nothing_unknown_passed(&f, &status);
    assert_ne!(state(&status, "t"), "passed", "{status}");
    assert!(f.receipts().is_empty());
    assert_eq!(f.commit_status("HEAD")["verdict"], "none");
    f.unlatch();
    let resumed = f.resume(&run);
    assert_eq!(resumed.status.code(), Some(0), "{}", text(&resumed.stderr));
    assert_eq!(f.count("t"), 2, "the resume ran the task again");
}

/// Invariant G: only a pass is withdrawn. A task that fails on its own just
/// before a stop reaches sykli keeps its failure: a failure is never a false
/// pass, and calling it an interruption would hide it.
#[test]
fn window_11_a_failure_just_before_a_stop_is_kept() {
    let f = Fixture::new("w11fail", "one\n", |base| {
        let b = base.display();
        vec![
            ("a".into(), format!("touch '{b}/a.failing'; exit 1"), vec![]),
            ("t".into(), trapping(base, "t", "true"), vec![]),
        ]
    });
    let child = f.spawn(&["run", "sykli.json", "--json"], None);
    f.wait_ready("t");
    wait_until(|| f.base.join("a.failing").exists());
    std::thread::sleep(Duration::from_millis(30));
    f.stop(child.id(), "TERM");
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(143), "{}", text(&out.stderr));
    let run = f.run_id();
    let status = f.status(&run);
    assert_eq!(state(&status, "a"), "failed", "{status}");
    assert_eq!(attempts(&status, "a")[0]["withdrawn"], false, "{status}");
    f.unlatch();
    let resumed = f.resume(&run);
    assert_eq!(resumed.status.code(), Some(1), "{}", text(&resumed.stderr));
    assert_eq!(
        attempts(&f.status(&run), "a").len(),
        1,
        "a is not run again"
    );
}

/// A withdrawn pass takes with it what was decided after it: here `b`, which
/// runs after `a`, was reused from the cache on the strength of `a`'s pass.
/// When the resume finds `a` failing, `b` is blocked, not left cached.
#[test]
fn a_withdrawn_pass_reopens_what_was_decided_after_it() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("w11dep", "one\n", |base| {
        let b = base.display();
        vec![
            (
                "a".into(),
                trapping(base, "a", &format!("test ! -e '{b}/fail'")),
                vec![],
            ),
            ("b".into(), "true".into(), vec!["a".into()]),
        ]
    });
    // Both pass and are cached; then only a's entry is dropped, so a runs
    // again and b is reused.
    f.unlatch();
    let first = f.output(&["run", "sykli.json", "--json"]);
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr));
    // a's cache key, from its `dispatched` record in the first run's journal.
    let records = f.runs_dir().join(f.run_id()).join("records");
    let key = fs::read_dir(&records)
        .unwrap()
        .filter_map(|entry| {
            let record: Value =
                serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap();
            let fact = &record["record"]["fact"];
            (fact["kind"] == "dispatched" && fact["task"] == "a")
                .then(|| fact["cache_key"].as_str().unwrap().to_owned())
        })
        .next()
        .unwrap();
    fs::remove_dir_all(f.repo.join(".sykli/cache").join(key)).unwrap();
    fs::remove_file(f.base.join("go")).unwrap();

    let child = f
        .sykli(&["run", "sykli.json", "--json"])
        .env("SYKLI_TEST_STOP_WINDOW_MS", "5000")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    f.wait_ready("a");
    // SAFETY: plain kill(2) of a's group: it traps TERM and exits 0.
    unsafe {
        kill(-pid_of(&f, "a"), 15);
    }
    let run = loop {
        let runs = f.runs();
        if let Some(run) = runs.iter().find(|run| {
            f.status(run)["tasks"].as_array().is_some_and(|tasks| {
                tasks
                    .iter()
                    .any(|t| t["name"] == "b" && t["state"] == "cached")
            })
        }) {
            break run.clone();
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    f.signalled_at.set(Some(now_ms()));
    f.stop(child.id(), "TERM");
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(143), "{}", text(&out.stderr));
    let status = f.status(&run);
    assert_ne!(state(&status, "a"), "passed", "{status}");
    assert_ne!(state(&status, "b"), "cached", "{status}");

    fs::write(f.base.join("fail"), "").unwrap();
    f.unlatch();
    let resumed = f.resume(&run);
    assert_eq!(resumed.status.code(), Some(1), "{}", text(&resumed.stderr));
    let receipt: Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(receipt["tasks"][0]["outcome"], "failed", "{receipt}");
    assert_eq!(receipt["tasks"][1]["outcome"], "blocked", "{receipt}");
}

/// Concurrent runs prune concurrently. Every run directory left is a whole
/// run (never one holding only a re-created `lease`), the count stays within
/// the budget plus the runs in flight, and no run is pruned while it is being
/// created (each one succeeds).
#[test]
fn concurrent_pruning_leaves_only_whole_runs() {
    let f = Fixture::new("prunerace", "one\n", one_task);
    f.unlatch();
    for _ in 0..12 {
        let children = (0..4)
            .map(|_| {
                f.sykli(&["run", "sykli.json", "--json"])
                    .env("SYKLI_RUN_BUDGET", "2")
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        for child in children {
            let out = child.wait_with_output().unwrap();
            assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        }
    }
    let entries = fs::read_dir(f.runs_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    for path in &entries {
        assert!(
            path.join("records/00000000000000000001.json").is_file(),
            "not a whole run: {} holds {:?}",
            path.display(),
            fs::read_dir(path).map(|e| e.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
        );
    }
    assert!(entries.len() <= 2 + 4, "{entries:?}");
    let commit = f.commit_status("HEAD");
    assert_eq!(commit["unreadable_runs"], json!([]), "{commit}");
}

/// `--assume-ended` leaves a trace: the lost record of an attempt whose
/// process group still existed says that the override let it go.
#[test]
fn assume_ended_is_recorded() {
    let f = Fixture::new("assumed", "one\n", |base| {
        let b = base.display();
        vec![(
            "t".into(),
            format!(
                "exec perl -MPOSIX -e 'POSIX::close($_) for 3..1023; exec @ARGV' sh -c \
                 'echo $$ > \"{b}/t.pid\"; echo run >> \"{b}/t.count\"; \
                 if [ ! -e \"{b}/go\" ]; then printf ready > \"{b}/t.ready\"; read token < \"{b}/t.latch\"; fi'"
            ),
            vec![],
        )]
    });
    let child = f.spawn(&["run", "sykli.json", "--json"], None);
    f.wait_ready("t");
    f.wait_started("t");
    f.stop(child.id(), "KILL");
    child.wait_with_output().unwrap();
    let run = f.run_id();
    let group = pid_of(&f, "t");
    f.unlatch();
    let resumed = f.output(&["resume", "--run", &run, "--assume-ended", "--json"]);
    assert_eq!(resumed.status.code(), Some(0), "{}", text(&resumed.stderr));
    f.release("t.latch");
    let first = &attempts(&f.status(&run), "t")[0];
    assert_eq!(first["outcome"], "lost", "{first}");
    let reason = first["reason"].as_str().unwrap_or_default();
    assert!(reason.contains("--assume-ended"), "{first}");
    assert!(reason.contains(&group.to_string()), "{first}");
}

/// A run keeps its receipt in its own directory, in the common git
/// directory, before it concludes: the evidence outlives the checkout that
/// ran it, as a caller's disposable checkout is removed once its run ends.
/// From another worktree of the clone, `status --run` still names the
/// receipt, `verify --commit` accepts it from there, and `status --commit`
/// counts it as recorded.
#[test]
fn a_runs_receipt_outlives_the_worktree_that_ran_it() {
    let f = Fixture::new("kept", "one\n", one_task);
    f.unlatch();
    for command in ["status", "verify"] {
        let help = f.output(&[command, "--help"]);
        assert!(
            text(&help.stdout).contains("receipt.kept"),
            "{command} --help"
        );
    }
    let rev = f.git(&["rev-parse", "HEAD"]);
    let checkout = f.base.join("checkout");
    f.git(&[
        "worktree",
        "add",
        "--quiet",
        "--detach",
        checkout.to_str().unwrap(),
        "HEAD",
    ]);
    let out = f
        .sykli(&["run", "sykli.json", "--detach", "--json"])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    let run = answer["run"].as_str().unwrap().to_owned();
    wait_until(|| f.status(&run)["state"] == "completed");

    let status = f.status(&run);
    let receipt = &status["receipt"];
    let path = PathBuf::from(receipt["path"].as_str().unwrap());
    assert!(
        path.starts_with(fs::canonicalize(&checkout).unwrap()),
        "{receipt}"
    );
    assert_eq!(receipt["present"], true, "{receipt}");
    let kept = PathBuf::from(
        receipt["kept"]["path"]
            .as_str()
            .unwrap_or_else(|| panic!("no kept receipt: {receipt}")),
    );
    assert_eq!(
        kept.parent(),
        Some(fs::canonicalize(f.runs_dir().join(&run)).unwrap().as_path()),
        "{receipt}"
    );
    assert_eq!(receipt["kept"]["present"], true, "{receipt}");
    let bytes = fs::read(&kept).unwrap();
    assert_eq!(bytes, fs::read(&path).unwrap());
    assert_eq!(receipt["digest"], format!("{:x}", Sha256::digest(&bytes)));

    // The checkout goes, as a caller's rolling checkout does.
    f.git(&["worktree", "remove", "--force", checkout.to_str().unwrap()]);
    assert!(!checkout.exists());
    let status = f.status(&run);
    assert_eq!(status["state"], "completed", "{status}");
    assert_eq!(status["receipt"]["present"], false, "{status}");
    assert_eq!(status["receipt"]["kept"]["present"], true, "{status}");
    assert_eq!(status["receipt"]["kept"]["path"], kept.to_str().unwrap());
    let human = f.output(&["status", "--run", &run]);
    assert!(
        text(&human.stdout).contains(kept.to_str().unwrap()),
        "{}",
        text(&human.stdout)
    );

    let verified = f.verify_commit(&kept, &rev);
    assert_eq!(
        verified.status.code(),
        Some(0),
        "{}{}",
        text(&verified.stdout),
        text(&verified.stderr)
    );
    let commit = f.commit_status(&rev);
    assert_eq!(commit["verdict"], "passed", "{commit}");
    assert_eq!(
        commit["current"][0]["receipt"],
        kept.to_str().unwrap(),
        "{commit}"
    );
    assert_eq!(commit["current"][0]["recorded"], true, "{commit}");
}

/// A failed run's receipt, kept by the run, still decides the commit once
/// the worktree that ran it is gone: a failure is never hidden by losing
/// the checkout.
#[test]
fn a_failed_runs_kept_receipt_still_fails_the_commit() {
    let f = Fixture::new("keptfail", "two\n", one_task);
    f.unlatch();
    let rev = f.git(&["rev-parse", "HEAD"]);
    let checkout = f.base.join("checkout");
    f.git(&[
        "worktree",
        "add",
        "--quiet",
        "--detach",
        checkout.to_str().unwrap(),
        "HEAD",
    ]);
    let out = f
        .sykli(&["run", "sykli.json", "--json"])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    f.git(&["worktree", "remove", "--force", checkout.to_str().unwrap()]);
    let run = f.run_id();
    let kept = f.status(&run)["receipt"]["kept"].clone();
    assert_eq!(kept["present"], true, "{kept}");
    let commit = f.commit_status(&rev);
    assert_eq!(commit["verdict"], "failed", "{commit}");
    assert_eq!(commit["current"][0]["receipt"], kept["path"], "{commit}");
    assert_eq!(commit["current"][0]["judgement"], "failed", "{commit}");
}
