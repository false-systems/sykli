#![cfg(unix)]

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NONCE: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "sykli-production-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("main.rs"),
            include_str!("../examples/production/main.rs"),
        )
        .unwrap();
        let fixture = Self(root);
        fixture.call(&["init", "--production"], 0);
        fixture
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_sykli"));
        cmd.current_dir(&self.0).args(args);
        cmd
    }
    fn call(&self, args: &[&str], expected: i32) -> Value {
        let out = self.command(args).output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(expected),
            "{args:?}: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn edit(&self, change: impl FnOnce(&mut Value)) {
        let path = self.0.join("sykli.production.json");
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        change(&mut value);
        fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    }
    fn plan(&self) -> Value {
        self.call(
            &["plan", "sykli.production.json", "--target", "app", "--json"],
            0,
        )
    }
    fn records(&self, id: &str) -> PathBuf {
        self.0
            .join(".sykli/production/requests")
            .join(id)
            .join("records")
    }
    fn write(&self, path: &str, contents: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn id(v: &Value) -> &str {
    v["production"].as_str().unwrap()
}
fn state<'a>(v: &'a Value, operation: &str) -> &'a str {
    v["work"][operation]["state"]["kind"].as_str().unwrap()
}
fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !predicate() {
        assert!(Instant::now() < deadline, "fixture latch timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn fresh_worker_continues_exact_source_and_changed_source_cannot_inherit_success() {
    let f = Fixture::new();
    let discover = f.call(&["targets", "--json"], 0);
    assert_eq!(
        discover["targets"]["app"]["required_checks"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let planned = f.plan();
    assert!(!f.0.join(".sykli").exists(), "plan must be read-only");
    let first = f.call(&["produce", "app", "--stop-after", "build", "--json"], 1);
    assert_eq!(id(&planned), id(&first));
    assert_eq!(state(&first, "build"), "satisfied");
    let build_attempt = first["work"]["build"]["state"]["attempt"].clone();
    // Use an unequivocally different source and a failing unit assertion.
    fs::write(
        f.0.join("main.rs"),
        "fn main() { println!(\"43\"); }\n#[test] fn failing() { assert_eq!(43, 42); }\n",
    )
    .unwrap();
    let status = f.call(&["status", id(&first), "--json"], 0);
    assert_eq!(status["through_sequence"], 2);
    let completed = f.call(&["resume", id(&first), "--json"], 0);
    assert_eq!(
        completed["work"]["build"]["state"]["attempt"],
        build_attempt
    );
    assert_eq!(completed["assessment"]["kind"], "complete");
    let executable = completed["delivery"]["app"]["availability"]["locations"][0]
        .as_str()
        .unwrap();
    assert_eq!(Command::new(executable).output().unwrap().stdout, b"42\n");
    f.call(&["verify-production", id(&first), "--json"], 0);
    let second = f.call(&["produce", "app", "--json"], 1);
    assert_ne!(id(&first), id(&second));
    assert_ne!(first["inputs"]["source"], second["inputs"]["source"]);
    assert_eq!(state(&second, "unit_tests"), "failed");
    assert_eq!(
        f.call(&["status", id(&first), "--json"], 0)["assessment"]["kind"],
        "complete"
    );
    for envelope in second["records"].as_array().unwrap() {
        assert_eq!(envelope["record"]["production"], id(&second));
    }
    // Even a correctly rehashed late envelope from A is not evidence for B.
    let late = f.records(id(&second)).join("00000000000000000007.json");
    let mut envelope = completed["records"][5].clone();
    envelope["record"]["sequence"] = 7.into();
    fs::write(&late, serde_json::to_vec(&envelope).unwrap()).unwrap();
    rehash_records(&f.records(id(&second)));
    assert!(
        f.call(&["status", id(&second), "--json"], 2)["error"]
            .as_str()
            .unwrap()
            .contains("reference mismatch")
    );
    fs::remove_file(late).unwrap();
    fs::remove_file(executable).unwrap();
    let missing = f.call(&["resume", id(&first), "--json"], 1);
    assert_eq!(missing["assessment"]["kind"], "complete");
    assert_eq!(
        missing["delivery"]["app"]["availability"]["kind"],
        "unavailable"
    );
    f.call(&["verify-production", id(&first), "--json"], 1);
}

#[test]
fn invalid_contracts_are_rejected_before_any_execution_and_unrelated_work_is_not_selected() {
    let f = Fixture::new();
    let original = fs::read(f.0.join("sykli.production.json")).unwrap();
    for (case, expected) in [
        (0, "type mismatch"),
        (1, "unresolved binding"),
        (2, "required check"),
        (3, "cycle"),
        (4, "check subject"),
        (5, "no product"),
        (6, "unsupported"),
        (7, "unsupported"),
        (8, "unsupported"),
    ] {
        fs::write(f.0.join("sykli.production.json"), &original).unwrap();
        f.edit(|c| {
            let t = &mut c["targets"]["app"];
            match case {
                0 => {
                    t["operations"]["smoke_test"]["inputs"]["executable"]["expects"] =
                        json!({"kind":"directory"})
                }
                1 => {
                    t["operations"]["smoke_test"]["inputs"]["executable"]["from"]["operation"] =
                        "absent".into()
                }
                2 => t["required_checks"] = json!(["build"]),
                3 => {
                    t["operations"]["build"]["inputs"]["cycle"] =
                        t["operations"]["smoke_test"]["inputs"]["executable"].clone()
                }
                4 => t["operations"]["unit_tests"]["subject_input"] = "absent".into(),
                5 => t["products"] = json!({}),
                6 => t["operations"]["build"]["reuse"] = "exact-invocation".into(),
                7 => t["profile"]["kind"] = "remote".into(),
                8 => {
                    t["operations"]["build"]["outputs"]["executable"]["validator"] =
                        "trust-me".into()
                }
                _ => unreachable!(),
            }
        });
        let result = f.call(&["targets", "--json"], 2);
        assert!(
            result["error"].as_str().unwrap().contains(expected),
            "{case}: {result}"
        );
        assert!(!f.0.join(".sykli").exists());
    }
    fs::write(f.0.join("sykli.production.json"), &original).unwrap();
    let old = f.plan()["contract"].clone();
    f.edit(|c| {
        c["targets"]["app"]["operations"]["unrelated"] =
            c["targets"]["app"]["operations"]["unit_tests"].clone();
        c["targets"]["app"]["operations"]["unrelated"]["run"] = "exit 99".into();
    });
    assert_eq!(f.plan()["selected"].as_array().unwrap().len(), 3);
    f.edit(|c| c["targets"]["app"]["required_checks"] = json!(["smoke_test"]));
    assert_ne!(f.plan()["contract"], old);
    let without_check = f.plan()["contract"].clone();
    f.edit(|c| c["targets"]["app"]["operations"]["build"]["run"] = "false".into());
    assert_ne!(f.plan()["contract"], without_check);
    fs::write(
        f.0.join("sykli.production.json"),
        b"{\"schema\":\"a\",\"schema\":\"b\",\"targets\":{}}",
    )
    .unwrap();
    assert!(
        f.call(&["targets", "--json"], 2)["error"]
            .as_str()
            .unwrap()
            .contains("duplicate key")
    );
}

#[test]
fn zero_exit_missing_stale_wrong_format_and_wrong_architecture_outputs_fail() {
    for case in ["missing", "format", "architecture"] {
        let f = Fixture::new();
        f.edit(|c| {
            if case == "missing" {
                c["targets"]["app"]["operations"]["build"]["run"] = "true".into();
            } else {
                let key = if case == "format" {
                    "format"
                } else {
                    "architecture"
                };
                let value = if case == "format" {
                    if cfg!(target_os = "macos") {
                        "elf"
                    } else {
                        "macho"
                    }
                } else if cfg!(target_arch = "aarch64") {
                    "x86_64"
                } else {
                    "aarch64"
                };
                c["targets"]["app"]["operations"]["build"]["outputs"]["executable"]["type"][key] =
                    value.into();
                c["targets"]["app"]["operations"]["smoke_test"]["inputs"]["executable"]["expects"]
                    [key] = value.into();
            }
        });
        let result = f.call(&["produce", "app", "--stop-after", "build", "--json"], 1);
        assert_eq!(state(&result, "build"), "failed", "{case}");
        assert_eq!(
            result["records"][1]["record"]["fact"]["result"]["code"],
            "output-validation-failed"
        );
        if case == "missing" {
            let previous = result["work"]["build"]["state"]["attempt"]
                .as_str()
                .unwrap();
            let stale = f
                .records(id(&result))
                .parent()
                .unwrap()
                .join("attempts")
                .join(previous)
                .join("outputs/app");
            fs::copy(env!("CARGO_BIN_EXE_sykli"), stale).unwrap();
            let retry = f.call(
                &[
                    "resume",
                    id(&result),
                    "--retry",
                    "build",
                    "--stop-after",
                    "build",
                    "--json",
                ],
                1,
            );
            assert_eq!(state(&retry, "build"), "failed");
            assert_ne!(retry["work"]["build"]["state"]["attempt"], previous);
        }
    }
}

#[test]
fn explicit_retry_preserves_failure_and_success_lineage() {
    let f = Fixture::new();
    let marker = f.0.join("allow");
    f.edit(|c| {
        c["targets"]["app"]["operations"]["unit_tests"]["run"] =
            format!("test -f '{}'", marker.display()).into()
    });
    let failed = f.call(&["produce", "app", "--json"], 1);
    let old = failed["work"]["unit_tests"]["state"]["attempt"].clone();
    fs::write(marker, "allowed").unwrap();
    let unchanged = f.call(&["resume", id(&failed), "--json"], 1);
    assert_eq!(unchanged["through_sequence"], failed["through_sequence"]);
    let passed = f.call(
        &["resume", id(&failed), "--retry", "unit_tests", "--json"],
        0,
    );
    assert_eq!(passed["records"][6]["record"]["fact"]["supersedes"], old);
    assert_eq!(
        passed["records"][3]["record"]["fact"]["result"]["outcome"],
        "failed"
    );
}

fn latched(f: &Fixture) -> (Value, Child) {
    let fifo = f.0.join("latch");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    f.edit(|c| {
        let run = c["targets"]["app"]["operations"]["build"]["run"]
            .as_str()
            .unwrap();
        c["targets"]["app"]["operations"]["build"]["run"] = format!(
            "printf ready > '{}'; read token < '{}'; {run}; printf done > '{}'",
            f.0.join("ready").display(),
            fifo.display(),
            f.0.join("done").display()
        )
        .into();
    });
    let plan = f.plan();
    let child = f
        .command(&["produce", "app", "--stop-after", "build", "--json"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until(|| f.0.join("ready").exists());
    (plan, child)
}

#[test]
fn controller_loss_live_reconciliation_writer_exclusion_and_snapshot_binding() {
    let f = Fixture::new();
    let (plan, mut controller) = latched(&f);
    assert_eq!(
        state(&f.call(&["status", id(&plan), "--json"], 0), "build"),
        "running"
    );
    assert!(
        f.call(&["resume", id(&plan), "--json"], 2)["error"]
            .as_str()
            .unwrap()
            .contains("production-busy")
    );
    controller.kill().unwrap();
    controller.wait().unwrap();
    fs::write(f.0.join("main.rs"), "this cannot compile").unwrap();
    assert_eq!(
        state(&f.call(&["status", id(&plan), "--json"], 0), "build"),
        "running"
    );
    fs::write(f.0.join("latch"), "continue\n").unwrap();
    wait_until(|| f.0.join("done").exists());
    wait_until(|| state(&f.call(&["status", id(&plan), "--json"], 0), "build") == "satisfied");
    let completed = f.call(&["resume", id(&plan), "--json"], 0);
    assert_eq!(completed["inputs"], plan["resolved_inputs"]);
    let app = completed["delivery"]["app"]["availability"]["locations"][0]
        .as_str()
        .unwrap();
    assert_eq!(Command::new(app).output().unwrap().stdout, b"42\n");
}

#[test]
fn executor_loss_remains_indeterminate_and_cannot_retry_unknown_side_effects() {
    let f = Fixture::new();
    let (plan, mut controller) = latched(&f);
    let out = Command::new("pgrep")
        .args(["-P", &controller.id().to_string()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let executor = String::from_utf8(out.stdout).unwrap();
    assert!(
        Command::new("kill")
            .args(["-KILL", executor.trim()])
            .status()
            .unwrap()
            .success()
    );
    controller.wait().unwrap();
    let status = f.call(&["status", id(&plan), "--json"], 0);
    assert_eq!(state(&status, "build"), "indeterminate");
    fs::write(f.0.join("latch"), "continue\n").unwrap();
    wait_until(|| f.0.join("done").exists());
    let resumed = f.call(&["resume", id(&plan), "--retry", "build", "--json"], 1);
    assert_eq!(state(&resumed, "build"), "indeterminate");
    assert_eq!(resumed["through_sequence"], 2);
    assert_eq!(
        resumed["records"][1]["record"]["fact"]["kind"],
        "contact-lost"
    );
    assert_eq!(
        resumed["records"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["record"]["fact"]["kind"] == "finished")
            .count(),
        0
    );
}

fn rehash_records(directory: &Path) {
    let mut paths: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    paths.sort();
    let mut previous = Value::Null;
    for path in paths {
        let mut envelope: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        envelope["record"]["previous"] = previous;
        let mut bytes = b"sykli-production-record.v1\0".to_vec();
        bytes.extend(serde_json::to_vec(&envelope["record"]).unwrap());
        envelope["id"] = format!("{:x}", Sha256::digest(bytes)).into();
        previous = envelope["id"].clone();
        fs::write(path, serde_json::to_vec(&envelope).unwrap()).unwrap();
    }
}

#[test]
fn record_recovery_rejects_wrong_subjects_conflicts_and_torn_commits() {
    let f = Fixture::new();
    let completed = f.call(&["produce", "app", "--json"], 0);
    let directory = f.records(id(&completed));
    let originals: Vec<_> = (1..=6)
        .map(|i| {
            (
                directory.join(format!("{i:020}.json")),
                fs::read(directory.join(format!("{i:020}.json"))).unwrap(),
            )
        })
        .collect();
    for index in [4, 6] {
        let path = directory.join(format!("{index:020}.json"));
        let mut envelope: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        envelope["record"]["fact"]["result"]["subject"]["content"] = "0".repeat(64).into();
        fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();
        rehash_records(&directory);
        assert!(
            f.call(&["status", id(&completed), "--json"], 2)["error"]
                .as_str()
                .unwrap()
                .contains("subject")
        );
        for (path, bytes) in &originals {
            fs::write(path, bytes).unwrap();
        }
    }
    let conflict = directory.join("00000000000000000007.json");
    let mut envelope: Value = serde_json::from_slice(&originals[5].1).unwrap();
    envelope["record"]["sequence"] = 7.into();
    envelope["record"]["fact"]["result"] =
        json!({"kind":"execution-failed","code":"conflicting-result"});
    fs::write(&conflict, serde_json::to_vec(&envelope).unwrap()).unwrap();
    rehash_records(&directory);
    assert!(
        f.call(&["status", id(&completed), "--json"], 2)["error"]
            .as_str()
            .unwrap()
            .contains("conflicting terminal")
    );
    fs::remove_file(conflict).unwrap();
    for (path, bytes) in &originals {
        fs::write(path, bytes).unwrap();
    }
    fs::write(directory.join(".tmp-incomplete"), b"{\"kind\":").unwrap();
    f.call(&["verify-production", id(&completed), "--json"], 0);
    fs::write(&originals[5].0, b"{\"kind\":").unwrap();
    f.call(&["status", id(&completed), "--json"], 2);
    fs::remove_file(&originals[5].0).unwrap();
    let unfinished = f.call(&["status", id(&completed), "--json"], 0);
    assert_eq!(state(&unfinished, "smoke_test"), "indeterminate");
    assert_eq!(unfinished["delivery_success"], false);
}

#[test]
fn source_paths_prerequisites_and_request_boundary_are_explicit() {
    let f = Fixture::new();
    let outside = f
        .command(&["init", "elsewhere/production.json", "--production"])
        .output()
        .unwrap();
    assert_eq!(outside.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&outside.stderr).contains("current directory"));
    f.edit(|c| c["targets"]["app"]["profile"]["tools"] = json!(["sykli_missing_tool_123456789"]));
    assert_eq!(f.plan()["blockers"][0]["kind"], "missing-tool");
    let blocked = f.call(&["produce", "app", "--json"], 1);
    assert_eq!(state(&blocked, "build"), "blocked");
    f.edit(|c| c["targets"]["app"]["inputs"]["source"]["paths"] = json!(["../credential"]));
    f.call(&["targets", "--json"], 2);
    f.edit(|c| c["targets"]["app"]["inputs"]["source"]["paths"] = json!(["link.rs"]));
    std::os::unix::fs::symlink(f.0.join("main.rs"), f.0.join("link.rs")).unwrap();
    assert!(
        f.plan()["blockers"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("symlink")
    );
    let out = f
        .command(&["produce", "app", "--complete", "true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn rebuilding_new_bytes_invalidates_the_old_smoke_check() {
    let f = Fixture::new();
    let marker = f.0.join("alternate");
    f.edit(|c| {
        let ops = &mut c["targets"]["app"]["operations"];
        let original = ops["build"]["run"].as_str().unwrap();
        ops["build"]["run"] = format!("if test -f '{}'; then printf 'fn main() {{ println!(\"43\"); }}' > \"$SYKLI_OUTPUT/generated.rs\"; rustc \"$SYKLI_OUTPUT/generated.rs\" -o \"$SYKLI_OUTPUT/app\"; else {original}; fi", marker.display()).into();
        ops["smoke_test"]["run"] = "test \"$(\"$SYKLI_INPUT_executable\")\" = 42".into();
        ops["smoke_test"]["assertion"] = "executable prints 42".into();
    });
    let first = f.call(&["produce", "app", "--json"], 0);
    fs::write(marker, "change nondeterministic build input").unwrap();
    let rebuilt = f.call(
        &[
            "resume",
            id(&first),
            "--retry",
            "build",
            "--stop-after",
            "build",
            "--json",
        ],
        1,
    );
    assert_ne!(
        first["delivery"]["app"]["artifact"],
        rebuilt["delivery"]["app"]["artifact"]
    );
    assert_eq!(state(&rebuilt, "smoke_test"), "ready");
    let checked = f.call(&["resume", id(&first), "--json"], 1);
    assert_eq!(state(&checked, "smoke_test"), "failed");
    assert_eq!(
        checked["records"][8]["record"]["fact"]["supersedes"],
        first["work"]["smoke_test"]["state"]["attempt"]
    );
}

#[test]
fn known_signal_is_terminal_and_directory_outputs_have_canonical_manifests() {
    let f = Fixture::new();
    f.edit(|c| c["targets"]["app"]["operations"]["build"]["run"] = "kill -TERM $$".into());
    let signalled = f.call(&["produce", "app", "--stop-after", "build", "--json"], 1);
    assert_eq!(
        signalled["records"][1]["record"]["fact"]["result"]["kind"],
        "interrupted"
    );
    f.edit(|c| {
        let t = &mut c["targets"]["app"];
        t["operations"]["build"]["run"] = "mkdir \"$SYKLI_OUTPUT/app\"; printf content > \"$SYKLI_OUTPUT/app/file\"; mkdir \"$SYKLI_OUTPUT/app/empty\"".into();
        t["operations"]["build"]["outputs"]["executable"]["type"] = json!({"kind":"directory"});
        t["operations"]["smoke_test"]["inputs"]["executable"]["expects"] = json!({"kind":"directory"});
        t["operations"]["smoke_test"]["run"] = "test \"$(cat \"$SYKLI_INPUT_executable/file\")\" = content && test -d \"$SYKLI_INPUT_executable/empty\"".into();
    });
    let tree = f.call(&["produce", "app", "--json"], 0);
    let path = tree["delivery"]["app"]["availability"]["locations"][0]
        .as_str()
        .unwrap();
    let manifest: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(manifest["schema"], "sykli-tree.v1");
    assert_eq!(manifest["entries"]["empty"]["content"], Value::Null);
    assert!(manifest["entries"]["file"]["content"].is_string());
}

#[test]
fn changed_tool_image_cannot_continue_under_the_old_context() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let tool = f.0.join("tool");
    fs::write(&tool, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    f.edit(|c| c["targets"]["app"]["profile"]["tools"] = json!(["tool", "rustc"]));
    let path = std::env::join_paths(
        std::iter::once(f.0.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let output = f
        .command(&["produce", "app", "--stop-after", "build", "--json"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let first: Value = serde_json::from_slice(&output.stdout).unwrap();
    fs::write(tool, "#!/bin/sh\nexit 1\n").unwrap();
    let output = f
        .command(&["resume", id(&first), "--json"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stdout).contains("context changed"));
}

#[test]
fn cargo_discovery_builds_captured_sources_and_resumes_bound_unit_and_smoke_checks() {
    let f = Fixture::new();
    f.write(
        "Cargo.toml",
        "[package]\nname = 'tiny-cli'\nversion = '0.1.0'\nedition = '2024'\n",
    );
    f.write("src/main.rs", "fn main() { print!(\"{}\", include_str!(\"message.txt\")); }\n#[test] fn unit() { assert_eq!(include_str!(\"message.txt\"), \"42\\n\"); }\n");
    f.write("src/message.txt", "42\n");
    f.write("build.rs", "fn main() {}\n");
    f.write(
        ".cargo/config.toml",
        "[build]\ntarget-dir = 'custom-output'\n",
    );
    f.write(".env", "PRIVATE=test-only\n");
    let generated = f.call(
        &[
            "init",
            "--production",
            "--force",
            "--smoke",
            "test \"$(\"$SYKLI_INPUT_executable\")\" = 42",
        ],
        0,
    );
    assert_eq!(generated["cargo"]["binary"], "tiny-cli");
    assert!(f.0.join("Cargo.lock").is_file());
    let discovered = f.call(&["targets", "--json"], 0);
    let inputs = discovered["targets"]["app"]["inputs"]["source"]["paths"]
        .as_array()
        .unwrap();
    for path in [
        "src/main.rs",
        "src/message.txt",
        "build.rs",
        "Cargo.lock",
        ".cargo/config.toml",
    ] {
        assert!(inputs.contains(&json!(path)), "missing {path}: {inputs:?}");
    }
    assert!(!inputs.contains(&json!(".env")));
    assert!(!inputs.contains(&json!("sykli.production.json")));
    let first = f.call(&["produce", "app", "--stop-after", "build", "--json"], 1);
    assert_eq!(state(&first, "build"), "satisfied");
    f.write("src/message.txt", "43\n");
    let resumed = f.call(&["resume", id(&first), "--json"], 0);
    assert_eq!(state(&resumed, "unit_tests"), "satisfied");
    assert_eq!(state(&resumed, "smoke_test"), "satisfied");
    assert_ne!(id(&f.plan()), id(&first));
}

#[test]
fn cargo_workspace_discovery_uses_default_members_and_local_library_inputs() {
    let f = Fixture::new();
    f.write(
        "Cargo.toml",
        "[workspace]\nmembers = ['app', 'support']\ndefault-members = ['app']\nresolver = '3'\n",
    );
    f.write("app/Cargo.toml", "[package]\nname='app'\nversion='0.1.0'\nedition='2024'\n[dependencies]\nsupport={path='../support'}\n");
    f.write("app/src/main.rs", "fn main() { println!(\"{}\", support::answer()); }\n#[test] fn unit() { assert_eq!(support::answer(),42); }\n");
    f.write(
        "support/Cargo.toml",
        "[package]\nname='support'\nversion='0.1.0'\nedition='2024'\n",
    );
    f.write("support/src/lib.rs", "pub fn answer() -> u32 { 42 }\n");
    f.call(
        &[
            "init",
            "--production",
            "--force",
            "--smoke",
            "test \"$(\"$SYKLI_INPUT_executable\")\" = 42",
        ],
        0,
    );
    let paths = f.plan()["inputs"]["source"]["paths"].clone();
    assert!(
        paths
            .as_array()
            .unwrap()
            .contains(&json!("support/src/lib.rs"))
    );
    f.call(&["produce", "app", "--json"], 0);
    f.write("app/src/bin/second.rs", "fn main() {}\n");
    let ambiguous = f
        .command(&["init", "--production", "--force", "--smoke", "true"])
        .output()
        .unwrap();
    assert_eq!(ambiguous.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("Select --package"));
    let selected = f.call(
        &[
            "init",
            "--production",
            "--force",
            "--package",
            "app",
            "--bin",
            "second",
            "--smoke",
            "\"$SYKLI_INPUT_executable\"",
        ],
        0,
    );
    assert_eq!(selected["cargo"]["binary"], "second");
}

#[test]
fn cargo_discovery_requires_a_smoke_check_and_does_not_invent_a_library_product() {
    let f = Fixture::new();
    f.write(
        "Cargo.toml",
        "[package]\nname='library'\nversion='0.1.0'\nedition='2024'\n",
    );
    f.write("src/lib.rs", "pub fn answer() -> u32 { 42 }\n");
    let original = fs::read(f.0.join("sykli.production.json")).unwrap();
    let missing = f
        .command(&["init", "--production", "--force"])
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("--smoke"));
    let library = f
        .command(&["init", "--production", "--force", "--smoke", "true"])
        .output()
        .unwrap();
    assert_eq!(library.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&library.stderr).contains("expected one Cargo binary"));
    assert_eq!(
        fs::read(f.0.join("sykli.production.json")).unwrap(),
        original
    );
}
