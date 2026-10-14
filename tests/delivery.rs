//! Deliveries, interrupted for real, against a scripted FAKE target.
//!
//! TEST ADAPTER. The target in these tests is fake, and labelled so: its
//! `apply` records the attempt in `BASE/applies` and, when it succeeds,
//! writes the desired state (`$SYKLI_INTENDED $SYKLI_DELIVERY`) to
//! `BASE/desired`; its `observe` prints `BASE/target.json`, a
//! `sykli-observation.v1` the test writes, with `@INTENDED@` and
//! `@DELIVERY@` standing for whatever `BASE/desired` holds, and then moves
//! `BASE/next.json` (if any) over it. So each test decides exactly what the
//! target reports, case by case. A fake's report is never evidence of a real
//! deployment; it is here to inject faults deterministically. The opt-in test
//! at the end (`SYKLI_TEST_REAL_TARGET`) runs the same experiment against a
//! real target the caller names.
//!
//! Every crash is a real SIGKILL of the coordinator at a named point (the
//! debug-build hook `SYKLI_TEST_PAUSE_AT` holds it there), or while the
//! apply or the observer is held on a latch. After each, what is asserted
//! is invariant H: no apply ever runs again on its own, and an effect that
//! cannot be established from the target stays UNKNOWN.
#![cfg(unix)]

use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NONCE: AtomicU64 = AtomicU64::new(0);

/// A repository with a production contract making `payload.txt` into an
/// artifact, and a delivery contract with one target, `fake`.
struct Fixture {
    base: PathBuf,
    repo: PathBuf,
}

/// The fake target's commands, writing and reading under `base`.
fn fake_target(base: &Path, smoke: bool, freshness: u64, settle: u64) -> Value {
    let b = base.display();
    let mut target = json!({
        "artifact": {"target": "app", "product": "file"},
        "identity": {"kind": "content"},
        "apply": {"run": format!(
            "echo \"$SYKLI_DELIVERY_ATTEMPT\" >> '{b}/applies'; \
             if [ -e '{b}/apply.hold' ]; then printf ready > '{b}/apply.ready'; read token < '{b}/apply.latch'; fi; \
             test -s \"$SYKLI_ARTIFACT\" || exit 3; \
             code=$(cat '{b}/apply.exit' 2>/dev/null || echo 0); \
             if [ \"$code\" = 0 ]; then echo \"$SYKLI_INTENDED $SYKLI_DELIVERY\" > '{b}/desired'; fi; \
             exit \"$code\""
        )},
        "observe": {"run": format!(
            "if [ -e '{b}/observe.hold' ]; then printf ready > '{b}/observe.ready'; read token < '{b}/observe.latch'; fi; \
             set -- $(cat '{b}/desired' 2>/dev/null) sha256:0000 none; \
             sed -e \"s|@INTENDED@|$1|g\" -e \"s|@DELIVERY@|$2|g\" '{b}/target.json' || exit 1; \
             if [ -e '{b}/next.json' ]; then mv '{b}/next.json' '{b}/target.json'; fi"
        )},
        "freshness_seconds": freshness,
    });
    if smoke {
        target["smoke"] =
            json!({"run": format!("exit $(cat '{b}/smoke.exit' 2>/dev/null || echo 0)")});
    }
    if settle > 0 {
        target["settle_seconds"] = json!(settle);
    }
    target
}

/// What a healthy target reports: it is asked for what was applied, runs
/// it everywhere, converged and ready.
fn healthy() -> Value {
    json!({
        "schema": "sykli-observation.v1",
        "reachable": true,
        "present": true,
        "uid": "uid-1",
        "desired": {"revision": "7", "artifact": "@INTENDED@", "operation": "@DELIVERY@", "writer": "fake-apply"},
        "observed": {"revision": "7", "artifacts": ["@INTENDED@"], "converged": true},
        "readiness": {"ready": 2, "wanted": 2},
        "checks": [{"name": "available", "passed": true}],
        "controller": {"state": "complete", "reason": "NewReplicaSetAvailable"},
        "token": "rv-1",
    })
}

/// Accepted, rollout incomplete: asked for the new artifact, still running
/// the old one beside it, not converged.
fn rolling_out() -> Value {
    let mut v = healthy();
    v["observed"] =
        json!({"revision": "6", "artifacts": ["@INTENDED@", "sha256:01d"], "converged": false});
    v["readiness"] = json!({"ready": 1, "wanted": 2});
    v["controller"] = json!({"state": "progressing", "reason": "ReplicaSetUpdated"});
    v
}

fn unreachable() -> Value {
    json!({"schema": "sykli-observation.v1", "reachable": false, "reason": "connection refused"})
}

impl Fixture {
    fn new(name: &str) -> Self {
        Self::with(name, |base| fake_target(base, true, 300, 0))
    }

    fn with(name: &str, target: impl Fn(&Path) -> Value) -> Self {
        let base = std::env::temp_dir().join(format!(
            "sykli-delivery-{name}-{}-{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&base);
        let repo = base.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let fixture = Fixture { base, repo };
        fixture.git(&["init", "--quiet", "--initial-branch=main"]);
        fs::write(fixture.repo.join(".gitignore"), ".sykli/\n").unwrap();
        fs::write(fixture.repo.join("payload.txt"), "v1\n").unwrap();
        let output = json!({"type": {"kind": "file", "media_type": "application/octet-stream"}, "collect": "payload", "validator": "builtin.v1"});
        fs::write(
            fixture.repo.join("sykli.production.json"),
            serde_json::to_vec_pretty(&json!({
                "schema": "sykli-production-contract.v1",
                "targets": {"app": {
                    "inputs": {"source": {"type": {"kind": "source-tree"}, "paths": ["payload.txt"]}},
                    "profile": {"kind": "local-shell.v1", "tools": []},
                    "operations": {"build": {
                        "kind": "transform",
                        "inputs": {"source": {"expects": {"kind": "source-tree"}, "from": {"kind": "target-input", "port": "source"}}},
                        "run": "cp \"$SYKLI_INPUT_source/payload.txt\" \"$SYKLI_OUTPUT/payload\"",
                        "reuse": "never",
                        "outputs": {"file": output},
                    }},
                    "products": {"file": {"kind": "operation-output", "operation": "build", "port": "file"}},
                    "required_checks": [],
                }},
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            fixture.repo.join("sykli.delivery.json"),
            serde_json::to_vec_pretty(&json!({
                "schema": "sykli-delivery-contract.v1",
                "production": "sykli.production.json",
                "targets": {"fake": target(&fixture.base)},
            }))
            .unwrap(),
        )
        .unwrap();
        fixture.commit("c1");
        fixture.target(&healthy());
        for latch in ["apply.latch", "observe.latch"] {
            mkfifo(&fixture.base.join(latch));
        }
        fixture.produce();
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

    fn commit(&self, message: &str) {
        self.git(&["add", "--all"]);
        self.git(&["commit", "--quiet", "--no-verify", "-m", message]);
    }

    fn sykli(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sykli"));
        own_git_config(&mut command)
            .args(args)
            .current_dir(&self.repo)
            .env("SYKLI_TEST_PAUSE_DIR", &self.base)
            .env("SYKLI_TEST_OBSERVE_INTERVAL_MS", "50")
            .env_remove("SYKLI_TEST_PAUSE_AT")
            .env_remove("SYKLI_TEST_FAIL_AT")
            .stdin(Stdio::null());
        command
    }

    fn output(&self, args: &[&str]) -> Output {
        self.sykli(args).output().unwrap()
    }

    fn produce(&self) {
        let out = self.output(&["produce", "app", "--json"]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    }

    /// What the fake target reports from now on.
    fn target(&self, observation: &Value) {
        fs::write(
            self.base.join("target.json"),
            serde_json::to_vec(observation).unwrap(),
        )
        .unwrap();
    }

    /// What it reports after its next observation.
    fn then(&self, observation: &Value) {
        fs::write(
            self.base.join("next.json"),
            serde_json::to_vec(observation).unwrap(),
        )
        .unwrap();
    }

    /// The controller's desired state, set out of band.
    fn desired(&self, artifact: &str, operation: &str) {
        fs::write(
            self.base.join("desired"),
            format!("{artifact} {operation}\n"),
        )
        .unwrap();
    }

    fn set(&self, name: &str, value: &str) {
        fs::write(self.base.join(name), value).unwrap();
    }

    fn clear(&self, name: &str) {
        let _ = fs::remove_file(self.base.join(name));
    }

    /// How many applies the fake target has seen.
    fn applies(&self) -> usize {
        fs::read_to_string(self.base.join("applies"))
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    fn deliver(&self, extra: &[&str]) -> (Output, Value) {
        let mut args = vec!["deliver", "--target", "fake", "--json"];
        args.extend(extra);
        let out = self.output(&args);
        let status = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
        (out, status)
    }

    fn deliveries(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(self.repo.join(".git/sykli/deliveries")) else {
            return Vec::new();
        };
        let mut found: Vec<(u64, String)> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.len() != 64 {
                    return None;
                }
                let first: Value = serde_json::from_slice(
                    &fs::read(entry.path().join("records/00000000000000000001.json")).ok()?,
                )
                .ok()?;
                Some((
                    first["record"]["fact"]["request"]["requested_at_ms"].as_u64()?,
                    name,
                ))
            })
            .collect();
        found.sort();
        found.into_iter().map(|(_, id)| id).collect()
    }

    /// The newest delivery.
    fn newest(&self) -> String {
        self.deliveries().pop().expect("a delivery")
    }

    fn status(&self, id: &str) -> Value {
        let out = self.output(&["status", "--delivery", id, "--json"]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn resume(&self, id: &str, reapply: bool) -> (Output, Value) {
        let mut args = vec!["resume", "--delivery", id, "--json"];
        if reapply {
            args.push("--reapply");
        }
        let out = self.output(&args);
        let status = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
        (out, status)
    }

    fn observe(&self, id: &str) -> (Output, Value) {
        let out = self.output(&["observe", "--delivery", id, "--json"]);
        let status = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
        (out, status)
    }

    /// Start `deliver`, wait until it reaches `point`, and SIGKILL it there.
    fn kill_at(&self, point: &str) {
        mkfifo(&self.base.join(format!("{point}.release")));
        let child = self
            .sykli(&["deliver", "--target", "fake", "--json"])
            .env("SYKLI_TEST_PAUSE_AT", point)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = self.wait_for_pid(point);
        kill(pid);
        let out = child.wait_with_output().unwrap();
        assert_eq!(
            out.status.code(),
            None,
            "killed at {point}: {}",
            text(&out.stderr)
        );
    }

    /// Start `deliver` with the apply or the observer held on its latch,
    /// and SIGKILL the coordinator once it is held there.
    fn kill_while(&self, held: &str) {
        self.set(&format!("{held}.hold"), "");
        let mut child = self
            .sykli(&["deliver", "--target", "fake", "--json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let ready = self.base.join(format!("{held}.ready"));
        wait_until(|| ready.exists());
        fs::remove_file(ready).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        self.clear(&format!("{held}.hold"));
    }

    fn wait_for_pid(&self, point: &str) -> u32 {
        let path = self.base.join(format!("{point}.pid"));
        wait_until(|| path.exists());
        let pid = fs::read_to_string(&path).unwrap().trim().parse().unwrap();
        fs::remove_file(&path).unwrap();
        pid
    }

    /// Release a latched apply or observer.
    fn release(&self, latch: &str) {
        let path = self.base.join(latch);
        std::thread::spawn(move || {
            let _ = fs::write(path, "go\n");
        });
    }

    /// The facts in a delivery's journal, by kind, in order.
    fn facts(&self, id: &str) -> Vec<String> {
        let records = self
            .repo
            .join(".git/sykli/deliveries")
            .join(id)
            .join("records");
        let mut paths: Vec<PathBuf> = fs::read_dir(&records)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| !p.file_name().unwrap().to_string_lossy().starts_with('.'))
            .collect();
        paths.sort();
        paths
            .iter()
            .map(|p| {
                let v: Value = serde_json::from_slice(&fs::read(p).unwrap()).unwrap();
                v["record"]["fact"]["kind"].as_str().unwrap().to_string()
            })
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Nothing a test started may outlive it: a command still held on a
        // latch reads end of file once a writer opens and closes it. Opened
        // without blocking, so a latch nobody reads costs nothing.
        use std::os::unix::fs::OpenOptionsExt;
        for latch in ["apply.latch", "observe.latch"] {
            let _ = fs::OpenOptions::new()
                .write(true)
                .custom_flags(if cfg!(target_os = "macos") {
                    0x4
                } else {
                    0x800
                })
                .open(self.base.join(latch));
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn own_git_config(command: &mut Command) -> &mut Command {
    command
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "core.excludesFile")
        .env("GIT_CONFIG_VALUE_0", "/dev/null")
        .env("GIT_CONFIG_KEY_1", "init.defaultBranch")
        .env("GIT_CONFIG_VALUE_1", "main")
}

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

fn kill(pid: u32) {
    assert!(
        Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
}

fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !predicate() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn label(status: &Value) -> &str {
    status["label"]
        .as_str()
        .unwrap_or_else(|| panic!("no label in {status}"))
}

fn dim<'a>(status: &'a Value, name: &str) -> &'a Value {
    &status["dimensions"][name]
}

// ---------------------------------------------------------------------------
// The ordinary path.

/// A delivery is judged from what the target reports, dimension by
/// dimension, and the journal holds the facts in their order: requested,
/// dispatched (before the apply's shell), started, acknowledged, observed.
#[test]
fn a_healthy_delivery_is_verified_from_its_observation() {
    let f = Fixture::new("healthy");
    let (out, status) = f.deliver(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(label(&status), "VERIFIED HEALTHY", "{status}");
    assert_eq!(dim(&status, "operation")["state"], "acknowledged");
    assert_eq!(dim(&status, "artifact")["verdict"], "match");
    assert_eq!(dim(&status, "desired")["verdict"], "this-delivery");
    assert_eq!(dim(&status, "desired")["attributed"], true);
    assert_eq!(dim(&status, "observed")["verdict"], "equals-desired");
    assert_eq!(dim(&status, "health")["verdict"], "passed");
    assert_eq!(dim(&status, "health")["smoke"]["passed"], true);
    assert_eq!(dim(&status, "freshness")["fresh"], true);
    let intended = status["intended"].as_str().unwrap();
    assert_eq!(
        intended,
        format!("sha256:{}", status["artifact"]["content"].as_str().unwrap())
    );
    let id = status["delivery"].as_str().unwrap();
    assert_eq!(
        f.facts(id),
        [
            "requested",
            "dispatched",
            "started",
            "acknowledged",
            "observed"
        ]
    );
    assert_eq!(f.applies(), 1);
    // The id came first, on stderr with --json.
    assert!(text(&out.stderr).contains(&format!("delivery: {id}")));
    // The human status says the same.
    let out = f.output(&["status", "--delivery", id]);
    let human = text(&out.stdout);
    assert!(human.contains("VERIFIED HEALTHY"), "{human}");
    for line in [
        "A operation",
        "B artifact",
        "C desired",
        "D observed",
        "E health",
        "F freshness",
    ] {
        assert!(human.contains(line), "{human}");
    }
}

/// An acknowledgement is not a deployment: the apply exited 0 and the target
/// accepted the change, but the rollout is incomplete.
#[test]
fn accepted_but_rollout_incomplete_is_not_verified() {
    let f = Fixture::new("incomplete");
    f.target(&rolling_out());
    let (out, status) = f.deliver(&[]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert_eq!(label(&status), "NOT VERIFIED HEALTHY", "{status}");
    assert_eq!(dim(&status, "operation")["state"], "acknowledged");
    assert_eq!(dim(&status, "desired")["verdict"], "this-delivery");
    assert_eq!(dim(&status, "artifact")["verdict"], "partial");
    assert_eq!(dim(&status, "observed")["verdict"], "converging");
    assert_eq!(dim(&status, "health")["verdict"], "pending");
}

/// With `settle_seconds`, deliver keeps observing until the target settles,
/// recording every observation.
#[test]
fn a_delivery_observes_until_the_target_settles() {
    let f = Fixture::with("settle", |base| fake_target(base, true, 300, 30));
    f.target(&rolling_out());
    f.then(&healthy());
    let (out, status) = f.deliver(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(label(&status), "VERIFIED HEALTHY", "{status}");
    assert_eq!(status["observations"]["since_dispatch"], 2);
    // Only the settled observation carries the smoke result.
    let recent = status["observations"]["recent"].as_array().unwrap();
    assert_eq!(recent[0]["smoke_passed"], true);
    assert_eq!(recent[1]["smoke_passed"], Value::Null);
}

// ---------------------------------------------------------------------------
// Crash windows: the coordinator killed for real.

/// Killed after the request is durable, before anything is dispatched: the
/// target was never touched, and resume dispatches it without --reapply.
#[test]
fn killed_before_dispatch_nothing_was_applied() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("before-dispatch");
    f.kill_at("after-request");
    let id = f.newest();
    let status = f.status(&id);
    assert_eq!(label(&status), "NOT VERIFIED HEALTHY", "{status}");
    assert_eq!(dim(&status, "operation")["state"], "requested");
    assert_eq!(dim(&status, "operation")["effect"], "none");
    assert_eq!(f.applies(), 0);
    assert_eq!(f.facts(&id), ["requested"]);
    let (out, _) = f.resume(&id, true);
    assert_eq!(out.status.code(), Some(2), "--reapply of nothing");
    let (out, status) = f.resume(&id, false);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(label(&status), "VERIFIED HEALTHY", "{status}");
    assert_eq!(f.applies(), 1);
}

/// Killed after the apply was recorded as dispatched, before its shell
/// started: the apply may or may not have been spawned, so its effect is
/// unknown. Resume asks the target, which shows nothing of it: UNKNOWN, and
/// nothing is applied until --reapply, which supersedes the lost attempt.
#[test]
fn killed_after_dispatch_stays_unknown_until_reapplied() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("after-dispatch");
    f.kill_at("after-dispatch");
    let id = f.newest();
    let status = f.status(&id);
    assert_eq!(label(&status), "UNKNOWN", "{status}");
    assert_eq!(dim(&status, "operation")["state"], "lost");
    assert_eq!(f.applies(), 0);

    let (out, status) = f.resume(&id, false);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert_eq!(label(&status), "UNKNOWN", "{status}");
    assert_eq!(dim(&status, "operation")["effect"], "unknown");
    let attempts = dim(&status, "operation")["attempts"].as_array().unwrap();
    assert_eq!(attempts[0]["end"], "lost");
    assert_eq!(attempts[0]["reconciled"][0]["effect"], "unknown");
    assert_eq!(f.applies(), 0, "resume applied without --reapply");

    let (out, status) = f.resume(&id, true);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(label(&status), "VERIFIED HEALTHY", "{status}");
    let attempts = dim(&status, "operation")["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2, "history is kept");
    assert_eq!(attempts[1]["supersedes"], attempts[0]["id"]);
    assert_eq!(attempts[1]["reapply"], true);
    assert_eq!(f.applies(), 1);
}

/// The lost acknowledgement (invariant H): the apply ran and the target
/// accepted it, then the coordinator died before recording that. Resume
/// never runs the apply again: it observes the target, finds this
/// delivery's artifact desired, and records `acknowledgement lost, effect
/// observed`.
#[test]
fn a_lost_acknowledgement_is_reconciled_by_observing_never_by_applying() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("lost-ack");
    f.kill_at("after-apply-exit");
    let id = f.newest();
    assert_eq!(f.applies(), 1);
    let status = f.status(&id);
    assert_eq!(label(&status), "UNKNOWN", "{status}");
    assert_eq!(dim(&status, "operation")["state"], "lost");

    let (out, status) = f.resume(&id, false);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(label(&status), "VERIFIED HEALTHY", "{status}");
    let operation = dim(&status, "operation");
    assert_eq!(operation["state"], "acknowledgement-lost");
    assert_eq!(operation["effect"], "observed");
    assert!(
        operation["detail"]
            .as_str()
            .unwrap()
            .contains("acknowledgement lost, effect observed")
    );
    assert_eq!(f.applies(), 1, "the apply ran again");
    assert_eq!(
        f.facts(&id),
        [
            "requested",
            "dispatched",
            "started",
            "resumed",
            "lost",
            "observed",
            "reconciled",
            "observed"
        ]
    );
    // Nothing is applied again even when asked: its effect was observed.
    let (out, _) = f.resume(&id, true);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stdout));
    assert_eq!(f.applies(), 1);
}

/// The same crash, but the target cannot be reached when resume asks: the
/// effect stays UNKNOWN, nothing is applied, and a later resume, once the
/// target answers, reconciles it.
#[test]
fn an_unreconcilable_lost_acknowledgement_stays_unknown() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("unreconcilable");
    f.kill_at("after-apply-exit");
    let id = f.newest();
    f.target(&unreachable());
    let (out, status) = f.resume(&id, false);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert_eq!(label(&status), "UNKNOWN", "{status}");
    let reason = dim(&status, "operation")["attempts"][0]["reconciled"][0]["reason"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(reason.contains("unreachable"), "{reason}");
    assert_eq!(f.applies(), 1);
    // The observer itself failing is no better.
    f.clear("target.json");
    let (_, status) = f.resume(&id, false);
    assert_eq!(label(&status), "UNKNOWN", "{status}");
    assert_eq!(f.applies(), 1);
    // The target answers again: reconciled from what it shows.
    f.target(&healthy());
    let (out, status) = f.resume(&id, false);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(dim(&status, "operation")["effect"], "observed");
    assert_eq!(f.applies(), 1);
}

/// Without an operation id, an observation cannot say which delivery put the
/// target's desired state there. A lost acknowledgement is never recorded
/// "effect observed" from the artifact alone (invariant F), and no delivery
/// is VERIFIED HEALTHY on a desired state it cannot show is its own.
#[test]
fn an_artifact_alone_is_never_attributed_to_a_delivery() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("opless");
    let mut opless = healthy();
    opless["desired"]
        .as_object_mut()
        .unwrap()
        .remove("operation");
    f.target(&opless);
    let (out, first) = f.deliver(&[]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert_eq!(label(&first), "NOT VERIFIED HEALTHY", "{first}");
    assert_eq!(dim(&first, "desired")["verdict"], "unattributed");
    assert_eq!(dim(&first, "desired")["attributed"], false);
    assert!(
        first["reasons"].to_string().contains("not attributed"),
        "{first}"
    );
    // The same artifact again, killed after dispatch: its apply never ran,
    // while the target desires that artifact (the first delivery's).
    f.kill_at("after-dispatch");
    let second = f.newest();
    assert_eq!(f.applies(), 1);
    let (out, status) = f.resume(&second, false);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert_eq!(label(&status), "UNKNOWN", "{status}");
    let reconciled = &dim(&status, "operation")["attempts"][0]["reconciled"][0];
    assert_eq!(reconciled["effect"], "unknown", "{status}");
    assert!(
        reconciled["reason"]
            .as_str()
            .unwrap()
            .contains("artifact present, not attributed to this delivery"),
        "{status}"
    );
    assert_eq!(f.applies(), 1);
}

/// `resume --reapply` applies again only after a reconciliation that reached
/// the target. An observer that failed, or a target it could not reach, says
/// nothing of whether the lost apply took effect, so nothing is repeated.
#[test]
fn reapply_needs_a_reconciliation_that_reached_the_target() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("reapply-blind");
    // The apply ran and the target took it; only its end was lost.
    f.kill_at("after-apply-exit");
    let id = f.newest();
    assert_eq!(f.applies(), 1);
    for (case, target) in [
        ("the observer fails", None),
        ("unreachable", Some(unreachable())),
    ] {
        match target {
            None => f.clear("target.json"),
            Some(observation) => f.target(&observation),
        }
        let (out, _) = f.resume(&id, true);
        assert_eq!(out.status.code(), Some(2), "{case}: {}", text(&out.stderr));
        assert!(
            text(&out.stderr).contains("could not reach the target"),
            "{case}: {}",
            text(&out.stderr)
        );
        assert_eq!(f.applies(), 1, "{case}: re-applied blind");
    }
    let status = f.status(&id);
    let attempts = dim(&status, "operation")["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 1, "{status}");
    assert_eq!(attempts[0]["reconciled"].as_array().unwrap().len(), 2);
    assert_eq!(label(&status), "UNKNOWN");
    // The target answers: its effect is observed, and nothing is applied.
    f.target(&healthy());
    let (out, status) = f.resume(&id, true);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(dim(&status, "operation")["effect"], "observed");
    assert_eq!(f.applies(), 1);
}

/// Killed while the apply runs: the apply outlives the coordinator, holding
/// its lease, so the delivery reads as applying and resume is refused until
/// it ends. Then its effect is reconciled from the target, not re-applied.
#[test]
fn killed_during_apply_resume_waits_then_reconciles() {
    let f = Fixture::new("during-apply");
    f.kill_while("apply");
    let id = f.newest();
    let status = f.status(&id);
    assert_eq!(dim(&status, "operation")["state"], "applying", "{status}");
    assert_eq!(label(&status), "NOT VERIFIED HEALTHY");
    let (out, _) = f.resume(&id, false);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("still running"),
        "{}",
        text(&out.stderr)
    );
    // The orphaned apply finishes: the target takes the change.
    f.release("apply.latch");
    let desired = f.base.join("desired");
    wait_until(|| desired.exists());
    let resumed = loop {
        let (out, status) = f.resume(&id, false);
        if out.status.code() != Some(2) {
            break status;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(label(&resumed), "VERIFIED HEALTHY", "{resumed}");
    assert_eq!(dim(&resumed, "operation")["effect"], "observed");
    assert_eq!(f.applies(), 1);
}

/// Killed while observing, after the acknowledgement: nothing is known of
/// the target yet (UNKNOWN); resume observes again and applies nothing.
#[test]
fn killed_during_observation_resume_observes_again() {
    let f = Fixture::new("during-observe");
    f.kill_while("observe");
    let id = f.newest();
    let status = f.status(&id);
    assert_eq!(label(&status), "UNKNOWN", "{status}");
    assert_eq!(dim(&status, "operation")["state"], "acknowledged");
    assert!(
        status["reasons"][0]
            .as_str()
            .unwrap()
            .contains("not observed")
    );
    f.release("observe.latch");
    let (out, status) = f.resume(&id, false);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(label(&status), "VERIFIED HEALTHY", "{status}");
    assert_eq!(f.applies(), 1);
}

/// SIGTERM during the apply: sykli stops it and records it interrupted; its
/// effect is unknown, never failed and never passed.
#[test]
fn a_stop_during_apply_is_interrupted_and_unknown() {
    let f = Fixture::new("sigterm");
    f.set("apply.hold", "");
    let child = f
        .sykli(&["deliver", "--target", "fake", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let ready = f.base.join("apply.ready");
    wait_until(|| ready.exists());
    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(143), "{}", text(&out.stderr));
    f.clear("apply.hold");
    let id = f.newest();
    let status = f.status(&id);
    assert_eq!(label(&status), "UNKNOWN", "{status}");
    assert_eq!(
        dim(&status, "operation")["attempts"][0]["end"],
        "interrupted"
    );
    let (_, status) = f.resume(&id, false);
    assert_eq!(
        dim(&status, "operation")["attempts"][0]["reconciled"][0]["effect"],
        "unknown"
    );
    assert_eq!(f.applies(), 1);
}

/// A failed write is never a success (invariant E): an observation that
/// cannot be recorded is an error, and the delivery says nothing healthy.
#[test]
fn a_failed_record_is_not_a_delivery() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("fail-at");
    let out = f
        .sykli(&["deliver", "--target", "fake", "--json"])
        .env("SYKLI_TEST_FAIL_AT", "before-observed")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let status = f.status(&f.newest());
    assert_eq!(label(&status), "UNKNOWN", "{status}");
}

// ---------------------------------------------------------------------------
// Reconcile cases, each from what the target reports.

/// Degraded: the intended artifact cannot run (a pull back-off), while the
/// old one keeps serving and the smoke check passes. Neither the passing
/// smoke nor the old replicas make it healthy.
#[test]
fn a_bad_digest_is_degraded_though_smoke_passes() {
    let f = Fixture::new("bad-digest");
    let mut pulling = healthy();
    pulling["observed"] = json!({"revision": "6", "artifacts": ["sha256:01d"], "converged": false});
    pulling["readiness"] = json!({"ready": 2, "wanted": 2});
    pulling["checks"] = json!([
        {"name": "available", "passed": true},
        {"name": "pull", "passed": false, "detail": "ImagePullBackOff"}
    ]);
    pulling["controller"] = json!({"state": "progressing", "reason": "ReplicaSetUpdated"});
    f.target(&pulling);
    let (out, status) = f.deliver(&[]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(label(&status), "DEGRADED", "{status}");
    assert_eq!(dim(&status, "artifact")["verdict"], "mismatch");
    assert_eq!(dim(&status, "health")["smoke"]["passed"], true);
    let reasons = status["reasons"].to_string();
    assert!(reasons.contains("ImagePullBackOff"), "{reasons}");
    // Then the controller gives up: still degraded, now by its own word.
    pulling["controller"] = json!({"state": "failed", "reason": "ProgressDeadlineExceeded"});
    f.target(&pulling);
    let (_, status) = f.observe(status["delivery"].as_str().unwrap());
    assert_eq!(label(&status), "DEGRADED");
    assert!(
        status["reasons"]
            .to_string()
            .contains("ProgressDeadlineExceeded")
    );
}

/// Readiness failing: the intended artifact runs, but is not ready.
#[test]
fn readiness_failing_is_degraded() {
    let f = Fixture::new("unready");
    let mut unready = healthy();
    unready["observed"] =
        json!({"revision": "7", "artifacts": ["@INTENDED@", "sha256:01d"], "converged": false});
    unready["readiness"] = json!({"ready": 2, "wanted": 2});
    unready["checks"] = json!([{"name": "readiness", "passed": false, "detail": "Readiness probe failed: HTTP probe failed with statuscode: 503"}]);
    unready["controller"] = json!({"state": "progressing"});
    f.target(&unready);
    let (_, status) = f.deliver(&[]);
    assert_eq!(label(&status), "DEGRADED", "{status}");
    assert_eq!(dim(&status, "artifact")["verdict"], "partial");
}

/// Controller unreachable: a later observation that reaches nothing makes
/// the delivery UNKNOWN; the last usable observation is still shown, aging.
#[test]
fn an_unreachable_controller_is_unknown() {
    let f = Fixture::new("unreachable");
    let (_, status) = f.deliver(&[]);
    let id = status["delivery"].as_str().unwrap().to_string();
    f.target(&unreachable());
    let (out, status) = f.observe(&id);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(label(&status), "UNKNOWN", "{status}");
    assert!(status["reasons"].to_string().contains("unreachable"));
    assert_eq!(
        dim(&status, "artifact")["verdict"],
        "match",
        "the last usable one"
    );
    // An observer that fails outright is no better.
    f.clear("target.json");
    let (_, status) = f.observe(&id);
    assert_eq!(label(&status), "UNKNOWN");
    assert!(status["reasons"].to_string().contains("observer"));
    assert_eq!(f.applies(), 1);
}

/// The controller is the authority on whether a rollout is done: one that
/// says it is still progressing is never verified, whatever else the
/// observation claims, and an observation with no controller state at all
/// establishes nothing.
#[test]
fn a_controller_that_has_not_completed_is_never_verified() {
    let f = Fixture::new("controller");
    let (out, status) = f.deliver(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let id = status["delivery"].as_str().unwrap().to_string();
    let mut progressing = healthy();
    progressing["controller"] = json!({"state": "progressing", "reason": "ReplicaSetUpdated"});
    f.target(&progressing);
    let (out, status) = f.observe(&id);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(label(&status), "NOT VERIFIED HEALTHY", "{status}");
    assert!(
        status["reasons"].to_string().contains("progressing"),
        "{status}"
    );
    let human = text(&f.output(&["status", "--delivery", &id]).stdout);
    assert!(human.contains("controller progressing"), "{human}");
    let mut missing = healthy();
    missing.as_object_mut().unwrap().remove("controller");
    f.target(&missing);
    let (out, status) = f.observe(&id);
    assert_eq!(out.status.code(), Some(1));
    assert_ne!(label(&status), "VERIFIED HEALTHY", "{status}");
    assert!(
        status["reasons"].to_string().contains("controller"),
        "{status}"
    );
}

/// Target missing: the object is gone.
#[test]
fn a_missing_target_is_degraded() {
    let f = Fixture::new("missing");
    let (_, status) = f.deliver(&[]);
    let id = status["delivery"].as_str().unwrap().to_string();
    f.target(&json!({"schema": "sykli-observation.v1", "reachable": true, "present": false}));
    let (_, status) = f.observe(&id);
    assert_eq!(label(&status), "DEGRADED", "{status}");
    assert_eq!(dim(&status, "artifact")["verdict"], "absent");
}

/// Superseded: a later delivery to the same target makes the earlier one's
/// healthy observation no statement about now.
#[test]
fn a_later_delivery_supersedes_an_earlier_one() {
    let f = Fixture::new("superseded");
    let (_, first) = f.deliver(&[]);
    assert_eq!(label(&first), "VERIFIED HEALTHY");
    let first = first["delivery"].as_str().unwrap().to_string();
    fs::write(f.repo.join("payload.txt"), "v2\n").unwrap();
    f.produce();
    let (_, second) = f.deliver(&[]);
    assert_eq!(label(&second), "VERIFIED HEALTHY", "{second}");
    assert_eq!(second["previous"], first.as_str());
    let status = f.status(&first);
    assert_eq!(label(&status), "NOT VERIFIED HEALTHY", "{status}");
    assert_eq!(
        dim(&status, "operation")["superseded_by"],
        second["delivery"]
    );
    // Observed again, the first delivery's target asks for the second's.
    let (_, status) = f.observe(&first);
    assert_eq!(dim(&status, "desired")["verdict"], "other");
    assert!(
        dim(&status, "desired")["explanation"]
            .as_str()
            .unwrap()
            .contains("superseded")
    );
}

/// A slow observer that read the target before it changed, and is recorded
/// after a newer observation that saw the change, never decides the label:
/// observations are judged by when their observer began.
#[test]
fn a_slow_observer_recorded_late_never_overrides_a_newer_observation() {
    let f = Fixture::with("slow-observer", |base| {
        let mut target = fake_target(base, false, 300, 0);
        let b = base.display();
        // Reads the target first, then (when held) waits before printing.
        target["observe"]["run"] = json!(format!(
            "set -- $(cat '{b}/desired' 2>/dev/null) sha256:0000 none; \
             if [ -e '{b}/observe.hold' ]; then printf ready > '{b}/observe.ready'; read token < '{b}/observe.latch'; fi; \
             sed -e \"s|@INTENDED@|$1|g\" -e \"s|@DELIVERY@|$2|g\" '{b}/target.json'"
        ));
        target
    });
    let (out, status) = f.deliver(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let id = status["delivery"].as_str().unwrap().to_string();
    f.set("observe.hold", "");
    let slow = f
        .sykli(&["observe", "--delivery", &id, "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let ready = f.base.join("observe.ready");
    wait_until(|| ready.exists());
    fs::remove_file(&ready).unwrap();
    f.clear("observe.hold");
    // Out of band, after the slow observer read the target: what it asks
    // for changes, still naming this delivery.
    std::thread::sleep(Duration::from_millis(5));
    f.desired("sha256:3a4a1", &id);
    let (_, newer) = f.observe(&id);
    assert_eq!(label(&newer), "NOT VERIFIED HEALTHY", "{newer}");
    f.release("observe.latch");
    let out = slow.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let status = f.status(&id);
    assert_eq!(label(&status), "NOT VERIFIED HEALTHY", "{status}");
    assert!(
        status["reasons"]
            .to_string()
            .contains("modified out of band"),
        "{status}"
    );
    let latest = status["observations"]["latest_usable"].as_u64().unwrap();
    assert_eq!(
        status["observations"]["out_of_order"],
        json!([latest + 1]),
        "{status}"
    );
    assert_eq!(f.applies(), 1);
}

/// Manual modification: the target still names this delivery, but someone
/// changed what it asks for.
#[test]
fn a_manual_modification_is_seen() {
    let f = Fixture::new("manual");
    let (_, status) = f.deliver(&[]);
    let id = status["delivery"].as_str().unwrap().to_string();
    let mut manual = healthy();
    manual["desired"]["writer"] = json!("kubectl-set");
    f.target(&manual);
    f.desired("sha256:3a4a1", &id);
    let (out, status) = f.observe(&id);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(label(&status), "NOT VERIFIED HEALTHY", "{status}");
    let explanation = dim(&status, "desired")["explanation"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        explanation.contains("modified out of band"),
        "{explanation}"
    );
    assert!(explanation.contains("kubectl-set"), "{explanation}");
}

/// Rollback requested and observed: `--rollback-to` delivers an earlier
/// delivery's artifact again, as a new delivery that says so; an
/// out-of-band rollback is told apart.
#[test]
fn a_rollback_is_requested_then_observed() {
    let f = Fixture::new("rollback");
    let (_, first) = f.deliver(&[]);
    let first_id = first["delivery"].as_str().unwrap().to_string();
    let first_intended = first["intended"].as_str().unwrap().to_string();
    fs::write(f.repo.join("payload.txt"), "v2\n").unwrap();
    f.produce();
    let (_, second) = f.deliver(&[]);
    let second_id = second["delivery"].as_str().unwrap().to_string();
    assert_ne!(second["intended"], first["intended"]);

    // Out of band: someone puts the first delivery's state back.
    f.desired(&first_intended, &first_id);
    let (_, status) = f.observe(&second_id);
    assert_eq!(label(&status), "NOT VERIFIED HEALTHY", "{status}");
    let explanation = dim(&status, "desired")["explanation"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        explanation.contains("rolled back out of band"),
        "{explanation}"
    );

    // Requested through sykli: a new delivery of the first's artifact.
    let (out, rollback) = f.deliver(&["--rollback-to", &first_id]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(rollback["rollback_of"], first_id.as_str());
    assert_eq!(rollback["intended"], first_intended.as_str());
    assert_eq!(
        dim(&rollback, "operation")["rollback_of"],
        first_id.as_str()
    );
    assert_eq!(label(&rollback), "VERIFIED HEALTHY", "{rollback}");
    assert_eq!(
        dim(&rollback, "artifact")["running"][0],
        first_intended.as_str()
    );
    let status = f.status(&second_id);
    assert_eq!(
        dim(&status, "operation")["superseded_by"],
        rollback["delivery"]
    );
}

/// A stale observation supports no present-tense statement, however
/// healthy it was.
#[test]
fn an_old_observation_is_stale() {
    let f = Fixture::with("stale", |base| fake_target(base, false, 1, 0));
    let (_, status) = f.deliver(&[]);
    assert_eq!(label(&status), "VERIFIED HEALTHY");
    std::thread::sleep(Duration::from_millis(1300));
    let status = f.status(status["delivery"].as_str().unwrap());
    assert_eq!(label(&status), "STALE", "{status}");
    assert_eq!(dim(&status, "freshness")["fresh"], false);
    assert_eq!(
        dim(&status, "operation")["as_last_observed"]["label"],
        "VERIFIED HEALTHY"
    );
}

/// An apply that reports failure is acknowledged as a refusal: not
/// verified, and applied again only on --reapply.
#[test]
fn a_failed_apply_is_refused_and_reapplied_only_when_asked() {
    let f = Fixture::new("refused");
    f.set("apply.exit", "1");
    let (out, status) = f.deliver(&[]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(label(&status), "NOT VERIFIED HEALTHY", "{status}");
    assert_eq!(dim(&status, "operation")["state"], "apply-failed");
    let id = status["delivery"].as_str().unwrap().to_string();
    let (_, status) = f.resume(&id, false);
    assert_eq!(dim(&status, "operation")["state"], "apply-failed");
    assert_eq!(f.applies(), 1);
    f.set("apply.exit", "0");
    let (out, status) = f.resume(&id, true);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(label(&status), "VERIFIED HEALTHY");
    assert_eq!(f.applies(), 2);
}

/// Nothing is delivered that was not completely produced.
#[test]
fn only_a_complete_production_is_delivered() {
    let f = Fixture::new("unproduced");
    fs::write(f.repo.join("payload.txt"), "v3\n").unwrap();
    let (out, _) = f.deliver(&[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("sykli produce app"),
        "{}",
        text(&out.stderr)
    );
    assert!(f.deliveries().is_empty());
    assert_eq!(f.applies(), 0);
    f.produce();
    let (out, _) = f.deliver(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}

/// A delivery to a target whose last delivery is still being carried out
/// is refused, never raced.
#[test]
fn a_target_takes_one_delivery_at_a_time() {
    let f = Fixture::new("busy");
    f.set("apply.hold", "");
    let first = f
        .sykli(&["deliver", "--target", "fake", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let ready = f.base.join("apply.ready");
    wait_until(|| ready.exists());
    let (out, _) = f.deliver(&[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("target-busy"),
        "{}",
        text(&out.stderr)
    );
    f.clear("apply.hold");
    f.release("apply.latch");
    let out = first.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(f.deliveries().len(), 1);
    assert_eq!(f.applies(), 1);
}

/// An apply that outlives its coordinator still holds its target: another
/// delivery to it is refused while that apply runs, and once it has ended
/// unrecorded, until its effect is reconciled with the target.
#[test]
fn an_orphaned_apply_keeps_its_target_busy() {
    let f = Fixture::new("orphan-busy");
    f.kill_while("apply");
    let orphaned = f.newest();
    let (out, _) = f.deliver(&[]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stdout));
    assert!(
        text(&out.stderr).contains("target-busy"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(f.deliveries().len(), 1);
    assert_eq!(f.applies(), 1);
    // The orphaned apply finishes with no one to record its end: the target
    // stays held until that end is reconciled (invariant H), never re-applied.
    f.release("apply.latch");
    let deadline = Instant::now() + Duration::from_secs(30);
    let refused = loop {
        let (out, _) = f.deliver(&[]);
        assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
        let stderr = text(&out.stderr);
        if !stderr.contains("target-busy") {
            break stderr;
        }
        assert!(Instant::now() < deadline, "{stderr}");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(refused.contains("target-unreconciled"), "{refused}");
    assert!(
        refused.contains(&format!("sykli resume --delivery {orphaned}")),
        "{refused}"
    );
    assert_eq!(f.applies(), 1);
    let (out, status) = f.resume(&orphaned, false);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(dim(&status, "operation")["effect"], "observed");
    let (out, _) = f.deliver(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(f.applies(), 2);
}

/// Pruning never removes what a resume still needs: a delivery whose
/// apply's effect is unknown stays, however old, while older settled ones go.
#[test]
fn pruning_keeps_a_delivery_whose_effect_is_unknown() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("prune");
    // Its apply's end is lost; the target, asked, shows nothing of it: the
    // effect stays unknown, and the target is free for the next delivery.
    f.kill_at("after-dispatch");
    let unknown = f.newest();
    let (_, status) = f.resume(&unknown, false);
    assert_eq!(dim(&status, "operation")["effect"], "unknown", "{status}");
    let deliver = || {
        let out = f
            .sykli(&["deliver", "--target", "fake", "--json"])
            .env("SYKLI_DELIVERY_BUDGET", "2")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        let status: Value = serde_json::from_slice(&out.stdout).unwrap();
        status["delivery"].as_str().unwrap().to_string()
    };
    let second = deliver();
    let third = deliver();
    assert!(f.deliveries().contains(&unknown), "pruned while unknown");
    let fourth = deliver();
    let kept = f.deliveries();
    assert!(kept.contains(&unknown), "pruned while unknown");
    assert!(
        !kept.contains(&second),
        "the oldest settled one stays: {kept:?}"
    );
    assert!(kept.contains(&third) && kept.contains(&fourth));
    // And it can still be reconciled.
    let (_, status) = f.resume(&unknown, false);
    assert_eq!(
        dim(&status, "operation")["attempts"][0]["reconciled"][1]["effect"],
        "unknown",
        "{status}"
    );
}

/// Invariant H across deliveries: a new delivery never applies over an
/// earlier apply whose end is unknown and that no observation of the target
/// has reconciled. It is refused, naming that delivery and the remedy; once
/// the earlier one is reconciled, it goes ahead.
#[test]
fn a_new_delivery_never_applies_over_an_unreconciled_lost_apply() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("unreconciled");
    f.kill_at("after-apply-exit");
    let lost = f.newest();
    assert_eq!(f.applies(), 1);
    for extra in [&[][..], &["--detach"][..]] {
        let (out, _) = f.deliver(extra);
        assert_eq!(out.status.code(), Some(2), "{}", text(&out.stdout));
        let stderr = text(&out.stderr);
        assert!(stderr.contains("target-unreconciled"), "{stderr}");
        assert!(
            stderr.contains(&format!("sykli resume --delivery {lost}")),
            "{stderr}"
        );
    }
    assert_eq!(f.applies(), 1, "applied over a lost apply");
    assert_eq!(f.deliveries(), vec![lost.clone()]);
    // Reconciled: the target shows the lost apply's effect.
    let (out, status) = f.resume(&lost, false);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(dim(&status, "operation")["effect"], "observed");
    let (out, status) = f.deliver(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(label(&status), "VERIFIED HEALTHY", "{status}");
    assert_eq!(f.applies(), 2);
}

/// When the reconciliation could not reach the target, the effect stays
/// unknown and the target stays held. The only way past it is
/// `deliver --reapply-over ID`, which names the delivery it applies over and
/// is recorded in the new delivery's request.
#[test]
fn applying_over_an_unreachable_unknown_takes_an_explicit_recorded_override() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("reapply-over");
    f.kill_at("after-apply-exit");
    let lost = f.newest();
    f.target(&unreachable());
    let (_, status) = f.resume(&lost, false);
    assert_eq!(label(&status), "UNKNOWN", "{status}");
    let (out, _) = f.deliver(&[]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = text(&out.stderr);
    assert!(stderr.contains("target-unreconciled"), "{stderr}");
    assert!(stderr.contains("could not reach the target"), "{stderr}");
    assert!(
        stderr.contains(&format!("--reapply-over {lost}")),
        "{stderr}"
    );
    // An override names what it overrides: anything else is refused.
    let other = "0".repeat(64);
    let (out, _) = f.deliver(&["--reapply-over", &other]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(f.applies(), 1);
    // Asked for by name: applied, and recorded.
    let (out, status) = f.deliver(&["--reapply-over", &lost]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains(&format!("applying over delivery {lost}")),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(f.applies(), 2);
    let over = status["reapply_over"].as_array().unwrap();
    assert_eq!(over.len(), 1, "{status}");
    assert_eq!(over[0]["delivery"], lost.as_str());
    assert!(
        over[0]["reason"].as_str().unwrap().contains("unreachable"),
        "{status}"
    );
    let human = text(
        &f.output(&["status", "--delivery", status["delivery"].as_str().unwrap()])
            .stdout,
    );
    assert!(
        human.contains(&format!("applied over delivery {lost}")),
        "{human}"
    );
    // Once applied over, the lost delivery holds the target no more.
    f.target(&healthy());
    let (out, _) = f.deliver(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(f.applies(), 3);
}

/// Resuming a delivery that was never dispatched dispatches its apply: that
/// too is refused while another delivery's apply to the target has an
/// unknown, unreconciled effect.
#[test]
fn resuming_an_undispatched_delivery_never_applies_over_an_unknown_effect() {
    if hooks_missing() {
        return;
    }
    let f = Fixture::new("resume-held");
    f.kill_at("after-request");
    let undispatched = f.newest();
    f.kill_at("after-apply-exit");
    let lost = f.newest();
    assert_ne!(lost, undispatched);
    assert_eq!(f.applies(), 1);
    let (out, _) = f.resume(&undispatched, false);
    assert_eq!(out.status.code(), Some(2));
    let stderr = text(&out.stderr);
    assert!(stderr.contains("target-unreconciled"), "{stderr}");
    assert!(stderr.contains(&lost), "{stderr}");
    assert_eq!(f.applies(), 1);
    assert_eq!(f.facts(&undispatched), ["requested"]);
    let (out, _) = f.resume(&lost, false);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let (out, status) = f.resume(&undispatched, false);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(f.applies(), 2);
    // Requested first, applied last: it is the one on the target, and it
    // supersedes the one requested after it, whose apply came before.
    assert_eq!(label(&status), "VERIFIED HEALTHY", "{status}");
    assert_eq!(dim(&status, "operation")["superseded_by"], Value::Null);
    let status = f.status(&lost);
    assert_eq!(
        dim(&status, "operation")["superseded_by"],
        undispatched.as_str(),
        "{status}"
    );
}

/// A rollback delivers an earlier artifact again only under the same
/// product and the same way of identifying it.
#[test]
fn a_rollback_across_a_changed_identity_is_refused() {
    let f = Fixture::new("rollback-identity");
    let (_, first) = f.deliver(&[]);
    let first = first["delivery"].as_str().unwrap().to_string();
    let path = f.repo.join("sykli.delivery.json");
    let mut contract: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    contract["targets"]["fake"]["identity"] =
        json!({"kind": "command", "run": "echo sha256:0123456789abcdef0123456789abcdef"});
    fs::write(
        f.repo.join("other.json"),
        serde_json::to_vec(&contract).unwrap(),
    )
    .unwrap();
    let out = f.output(&[
        "deliver",
        "other.json",
        "--target",
        "fake",
        "--rollback-to",
        &first,
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("identified by a command"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(f.applies(), 1);
}

/// `--detach` returns once the request is durable; the coordinator carries
/// it out.
#[test]
fn a_detached_delivery_is_carried_out_by_its_coordinator() {
    let f = Fixture::new("detach");
    let out = f.output(&["deliver", "--target", "fake", "--detach", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let handed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(handed["schema"], "sykli-delivery.v1");
    let id = handed["delivery"].as_str().unwrap().to_string();
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        let status = f.status(&id);
        if status["coordinator_live"] == false {
            break status;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(label(&status), "VERIFIED HEALTHY", "{status}");
    assert_eq!(f.applies(), 1);
}

/// The contract's commands never see what they did not declare: a variable
/// of the caller reaches a command only when it is inherited, and then only
/// its digest reaches the journal.
#[test]
fn credentials_reach_only_the_command_that_inherits_them() {
    let f = Fixture::with("secrets", |base| {
        let mut target = fake_target(base, false, 300, 0);
        let b = base.display();
        let apply = target["apply"]["run"].as_str().unwrap().to_string();
        target["apply"] = json!({
            "run": format!("echo \"token=$DEPLOY_TOKEN\" > '{b}/apply.env'; {apply}"),
            "inherit": ["DEPLOY_TOKEN"],
        });
        let observe = target["observe"]["run"].as_str().unwrap().to_string();
        target["observe"]["run"] = json!(format!(
            "echo \"token=$DEPLOY_TOKEN\" > '{b}/observe.env'; {observe}"
        ));
        target
    });
    let out = f
        .sykli(&["deliver", "--target", "fake", "--json"])
        .env("DEPLOY_TOKEN", "s3cr3t-value")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        fs::read_to_string(f.base.join("apply.env")).unwrap(),
        "token=s3cr3t-value\n"
    );
    assert_eq!(
        fs::read_to_string(f.base.join("observe.env")).unwrap(),
        "token=\n"
    );
    let id = f.newest();
    let directory = f.repo.join(".git/sykli/deliveries").join(&id);
    for entry in walk(&directory) {
        let bytes = fs::read(&entry).unwrap_or_default();
        assert!(
            !String::from_utf8_lossy(&bytes).contains("s3cr3t-value"),
            "the credential is in {}",
            entry.display()
        );
    }
}

fn walk(directory: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(directory).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// A contract is refused when it is not one: reserved variables, an empty
/// command, no freshness bound.
#[test]
fn an_invalid_delivery_contract_is_refused() {
    let f = Fixture::new("invalid");
    for (change, says) in [
        (json!({"freshness_seconds": 0}), "freshness_seconds"),
        (json!({"apply": {"run": " "}}), "empty command"),
        (
            json!({"apply": {"run": "true", "env": {"SYKLI_INTENDED": "x"}}}),
            "reserved",
        ),
        (
            json!({"observe": {"run": "true", "inherit": ["A", "A"]}}),
            "declared twice",
        ),
        (
            json!({"apply": {"run": "true", "env": {"A": "${worktree_cache}"}}}),
            "literal",
        ),
    ] {
        let path = f.repo.join("sykli.delivery.json");
        let mut contract: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for (key, value) in change.as_object().unwrap() {
            contract["targets"]["fake"][key] = value.clone();
        }
        fs::write(
            f.repo.join("bad.json"),
            serde_json::to_vec(&contract).unwrap(),
        )
        .unwrap();
        let out = f.output(&["deliver", "bad.json", "--target", "fake"]);
        assert_eq!(out.status.code(), Some(2));
        assert!(
            text(&out.stderr).contains(says),
            "{says}: {}",
            text(&out.stderr)
        );
    }
    assert_eq!(f.applies(), 0);
}

// ---------------------------------------------------------------------------
// The real target (opt-in): Experiment E.

/// Experiment E against a REAL target the caller names, opt-in:
/// `SYKLI_TEST_REAL_TARGET=<repository>` holding a `sykli.production.json`
/// and a `sykli.delivery.json`, and `SYKLI_TEST_REAL_TARGET_NAME=<target>`.
/// The caller's own commands do everything target-specific, including every
/// safety check before they touch it; this test only drives sykli:
/// produce the pinned artifact, request the delivery, let the apply run and
/// kill the coordinator before the acknowledgement is recorded, then resume,
/// which reconciles by querying the actual target. Then it observes again.
/// Every status is printed (`--nocapture`), for the record.
#[test]
fn real_target_experiment_e() {
    let (Some(repo), Ok(target)) = (
        std::env::var_os("SYKLI_TEST_REAL_TARGET").map(PathBuf::from),
        std::env::var("SYKLI_TEST_REAL_TARGET_NAME"),
    ) else {
        eprintln!("skipped: set SYKLI_TEST_REAL_TARGET and SYKLI_TEST_REAL_TARGET_NAME");
        return;
    };
    if hooks_missing() {
        return;
    }
    let pause = std::env::temp_dir().join(format!("sykli-real-e-{}", std::process::id()));
    fs::create_dir_all(&pause).unwrap();
    let sykli = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sykli"));
        command
            .args(args)
            .current_dir(&repo)
            .env("SYKLI_TEST_PAUSE_DIR", &pause)
            .env_remove("SYKLI_TEST_PAUSE_AT")
            .stdin(Stdio::null());
        command
    };
    let contract: Value =
        serde_json::from_slice(&fs::read(repo.join("sykli.delivery.json")).unwrap()).unwrap();
    let production = contract["targets"][&target]["artifact"]["target"]
        .as_str()
        .unwrap()
        .to_string();
    eprintln!("== produce {production}");
    let out = sykli(&["produce", &production, "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let produced: Value = serde_json::from_slice(&out.stdout).unwrap();
    eprintln!("production {}", produced["production"]);

    eprintln!(
        "== deliver --target {target}, killed after the apply exits, before its acknowledgement is recorded"
    );
    mkfifo(&pause.join("after-apply-exit.release"));
    let child = sykli(&["deliver", "--target", &target, "--json"])
        .env("SYKLI_TEST_PAUSE_AT", "after-apply-exit")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid_file = pause.join("after-apply-exit.pid");
    let deadline = Instant::now() + Duration::from_secs(600);
    while !pid_file.exists() {
        assert!(Instant::now() < deadline, "the apply never exited");
        std::thread::sleep(Duration::from_millis(50));
    }
    kill(
        fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap(),
    );
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), None);
    let stderr = text(&out.stderr);
    eprintln!("{stderr}");
    let id = stderr
        .lines()
        .find_map(|line| line.strip_prefix("delivery: "))
        .expect("the delivery id")
        .trim()
        .to_string();
    let status = |id: &str| -> Value {
        let out = sykli(&["status", "--delivery", id, "--json"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let after_kill = status(&id);
    eprintln!("== status after the kill\n{after_kill:#}");
    assert_eq!(label(&after_kill), "UNKNOWN");

    eprintln!("== resume --delivery {id} (reconcile by querying the target)");
    let out = sykli(&["resume", "--delivery", &id, "--json"])
        .output()
        .unwrap();
    eprintln!("{}", text(&out.stderr));
    let resumed: Value = serde_json::from_slice(&out.stdout).unwrap();
    eprintln!("{resumed:#}");
    let operation = dim(&resumed, "operation");
    assert_eq!(operation["effect"], "observed", "{resumed}");
    assert_eq!(
        operation["attempts"].as_array().unwrap().len(),
        1,
        "re-applied"
    );
    assert_eq!(label(&resumed), "VERIFIED HEALTHY", "{resumed}");
    assert_eq!(dim(&resumed, "artifact")["verdict"], "match");
    assert_eq!(dim(&resumed, "observed")["verdict"], "equals-desired");
    assert_eq!(dim(&resumed, "freshness")["fresh"], true);

    eprintln!("== observe --delivery {id}");
    let out = sykli(&["observe", "--delivery", &id, "--json"])
        .output()
        .unwrap();
    let observed: Value = serde_json::from_slice(&out.stdout).unwrap();
    eprintln!("{observed:#}");
    assert_eq!(label(&observed), "VERIFIED HEALTHY", "{observed}");
    let _ = fs::remove_dir_all(&pause);
}
