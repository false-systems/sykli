use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn help_states_the_identity() {
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .arg("--help")
        .output()
        .expect("binary runs");
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("utf8");
    assert!(text.contains("declared graphs"));
}

#[test]
fn validate_accepts_a_valid_contract() {
    let dir = temp_dir("valid");
    let contract = dir.join("sykli.json");
    std::fs::write(
        &contract,
        r#"{
          "schema": "sykli-contract.v1",
          "tasks": [
            { "name": "build", "run": "true" },
            { "name": "test", "run": "true", "after": ["build"] }
          ]
        }"#,
    )
    .expect("write contract");

    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["validate", contract.to_str().expect("utf8 path"), "--json"])
        .output()
        .expect("binary runs");
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("utf8");
    assert!(text.contains(r#""schema": "sykli-plan.v1""#));
    assert!(text.contains(r#""contract_hash""#));
    assert!(text.contains(r#""build""#));
}

#[test]
fn validate_rejects_unknown_keys() {
    let dir = temp_dir("unknown");
    let contract = dir.join("sykli.json");
    std::fs::write(
        &contract,
        r#"{
          "schema": "sykli-contract.v1",
          "tasks": [
            { "name": "build", "run": "true", "surprise": true }
          ]
        }"#,
    )
    .expect("write contract");

    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["validate", contract.to_str().expect("utf8 path")])
        .output()
        .expect("binary runs");
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8(out.stderr).expect("utf8");
    assert!(err.contains("unknown field"));
}

#[test]
fn plan_outputs_dependency_levels() {
    let dir = temp_dir("plan");
    let contract = dir.join("sykli.json");
    std::fs::write(
        &contract,
        r#"{
          "schema": "sykli-contract.v1",
          "tasks": [
            { "name": "a", "run": "true" },
            { "name": "b", "run": "true" },
            { "name": "c", "run": "true", "after": ["a", "b"] }
          ]
        }"#,
    )
    .expect("write contract");

    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["plan", contract.to_str().expect("utf8 path")])
        .output()
        .expect("binary runs");
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("utf8");
    assert!(text.contains("level 1: a, b"));
    assert!(text.contains("level 2: c"));
}

#[test]
fn run_executes_shell_tasks_and_blocks_dependents() {
    let dir = temp_dir("run");
    let contract = dir.join("sykli.json");
    std::fs::write(
        &contract,
        r#"{
          "schema": "sykli-contract.v1",
          "tasks": [
            { "name": "ok", "run": "printf ok", "outputs": ["made.txt"] },
            { "name": "bad", "run": "exit 7" },
            { "name": "blocked", "run": "printf nope", "after": ["bad"] }
          ]
        }"#,
    )
    .expect("write contract");
    std::fs::write(dir.join("made.txt"), "").expect("write output");

    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["run", contract.to_str().expect("utf8 path"), "--json"])
        .output()
        .expect("binary runs");
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8(out.stdout).expect("utf8");
    assert!(text.contains(r#""outcome": "passed""#));
    assert!(text.contains(r#""outcome": "failed""#));
    assert!(text.contains(r#""outcome": "blocked""#));
    assert!(text.contains(r#""stdout": "ok""#));
    assert!(text.contains(r#""schema": "sykli-receipt.v1""#));
    assert!(text.contains(r#""stdout_sha256""#));
    assert!(text.contains(r#""receipt_path""#));

    let json: serde_json::Value = serde_json::from_str(&text).expect("json");
    let receipt_path = json["receipt_path"]
        .as_str()
        .expect("receipt path is string");
    assert!(std::path::Path::new(receipt_path).exists());
    assert!(receipt_path.contains(".sykli/receipts/rcpt_"));
}

#[test]
fn run_reuses_cache_hits_with_receipt_provenance() {
    let dir = temp_dir("cache");
    let contract = dir.join("sykli.json");
    std::fs::write(dir.join("in.txt"), "one").expect("write input");
    std::fs::write(
        &contract,
        r#"{
          "schema": "sykli-contract.v1",
          "tasks": [
            {
              "name": "build",
              "run": "printf run >> marker.txt; printf cached > out.txt",
              "inputs": ["in.txt"],
              "outputs": ["out.txt"]
            }
          ]
        }"#,
    )
    .expect("write contract");

    let first = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["run", contract.to_str().expect("utf8 path"), "--json"])
        .output()
        .expect("binary runs");
    assert!(first.status.success());
    let first_text = String::from_utf8(first.stdout).expect("utf8");
    let first_json: serde_json::Value = serde_json::from_str(&first_text).expect("json");
    let first_receipt_path = first_json["receipt_path"]
        .as_str()
        .expect("receipt path is string")
        .to_string();
    assert_eq!(
        std::fs::read_to_string(dir.join("marker.txt")).expect("marker"),
        "run"
    );

    std::fs::remove_file(dir.join("out.txt")).expect("remove output");
    let second = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["run", contract.to_str().expect("utf8 path"), "--json"])
        .output()
        .expect("binary runs");
    assert!(second.status.success());
    assert_eq!(
        std::fs::read_to_string(dir.join("out.txt")).expect("output"),
        "cached"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("marker.txt")).expect("marker"),
        "run"
    );
    let text = String::from_utf8(second.stdout).expect("utf8");
    assert!(text.contains(r#""outcome": "cached""#));
    assert!(text.contains(r#""cache_provenance""#));

    std::fs::write(&first_receipt_path, "tampered").expect("tamper receipt");
    std::fs::remove_file(dir.join("out.txt")).expect("remove output");
    let tampered = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["run", contract.to_str().expect("utf8 path"), "--json"])
        .output()
        .expect("binary runs");
    assert!(tampered.status.success());
    assert_eq!(
        std::fs::read_to_string(dir.join("marker.txt")).expect("marker"),
        "runrun"
    );

    std::fs::write(dir.join("in.txt"), "two").expect("change input");
    std::fs::remove_file(dir.join("out.txt")).expect("remove output");
    let third = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["run", contract.to_str().expect("utf8 path"), "--json"])
        .output()
        .expect("binary runs");
    assert!(third.status.success());
    assert_eq!(
        std::fs::read_to_string(dir.join("marker.txt")).expect("marker"),
        "runrunrun"
    );
}

#[test]
fn run_errors_when_declared_input_cannot_be_read() {
    let dir = temp_dir("input-error");
    let contract = dir.join("sykli.json");
    std::fs::create_dir(dir.join("input-dir")).expect("create input dir");
    std::fs::write(
        &contract,
        r#"{
          "schema": "sykli-contract.v1",
          "tasks": [
            {
              "name": "build",
              "run": "printf no",
              "inputs": ["input-dir"],
              "outputs": ["out.txt"]
            }
          ]
        }"#,
    )
    .expect("write contract");

    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["run", contract.to_str().expect("utf8 path"), "--json"])
        .output()
        .expect("binary runs");
    assert_eq!(out.status.code(), Some(2));
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("sykli-{name}-{unique}"));
    std::fs::create_dir(&path).expect("create temp dir");
    path
}
