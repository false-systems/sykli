use std::{fs, process::Command};

#[test]
fn help_states_the_identity() {
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .arg("--help")
        .output()
        .expect("binary runs");
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("utf8");
    assert!(text.contains("declared work graphs"));
}

#[test]
fn validate_reports_invalid_contracts() {
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["validate", "nonexistent.json"])
        .output()
        .expect("binary runs");
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8(out.stderr).expect("utf8");
    assert!(err.contains("invalid nonexistent.json"));
}

#[test]
fn run_json_prints_one_receipt() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("sykli-json-{nonce}"));
    fs::create_dir_all(&root).unwrap();
    let contract = root.join("contract.json");
    let output = format!("task-output-{nonce}");
    fs::write(&contract, format!(
        r#"{{"schema":"sykli-contract.v1","tasks":[{{"name":"hello","run":"test -z \"$SYKLI_TEST_SECRET\" && test \"$DECLARED\" = visible && printf {output}","env":{{"DECLARED":"visible"}}}}]}}"#
    ))
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["run", "--json"])
        .arg(&contract)
        .env("SYKLI_TEST_SECRET", "must-not-leak")
        .output()
        .expect("binary runs");
    fs::remove_dir_all(root).unwrap();

    assert!(out.status.success());
    let receipt: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON value");
    assert_eq!(receipt["schema"], "sykli-receipt.v1");
    assert_eq!(receipt["tasks"][0]["name"], "hello");
    assert_eq!(receipt["tasks"][0]["stdout_truncated"], false);
    assert!(String::from_utf8(out.stderr).unwrap().contains(&output));
}

#[test]
fn verify_accepts_fresh_receipts_and_rejects_stale_trees() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("sykli-verify-{nonce}"));
    fs::create_dir_all(&root).unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?} failed");
    };
    git(&["init", "--quiet"]);
    fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
    fs::write(root.join("tracked.txt"), "one").unwrap();
    fs::write(root.join("ignored.txt"), "one").unwrap();
    let contract_text = r#"{"schema":"sykli-contract.v1","tasks":[{"name":"noop","run":"true","inputs":["ignored.txt"]}]}"#;
    fs::write(root.join("contract.json"), contract_text).unwrap();
    let lock = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["lock", "contract.json"])
        .current_dir(&root)
        .output()
        .expect("binary runs");
    assert!(lock.status.success());
    git(&["add", "--all"]);
    git(&["commit", "--quiet", "-m", "init"]);

    let run = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["run", "contract.json", "--json"])
        .current_dir(&root)
        .output()
        .expect("binary runs");
    assert!(run.status.success());
    // The receipt lives outside the repo so it cannot perturb the tree it
    // describes; in-repo state written by the run (.sykli) is excluded by run.
    let receipt = std::env::temp_dir().join(format!("sykli-verify-{nonce}.receipt.json"));
    fs::write(&receipt, &run.stdout).unwrap();

    let verify = |root: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_sykli"))
            .arg("verify")
            .arg(&receipt)
            .args(["--contract", "contract.json"])
            .current_dir(root)
            .output()
            .expect("binary runs")
    };
    let fresh = verify(&root);
    assert!(
        fresh.status.success(),
        "fresh verify failed: {}",
        String::from_utf8_lossy(&fresh.stdout)
    );
    assert!(String::from_utf8_lossy(&fresh.stdout).contains("verified"));

    // Ignored declared inputs are still part of the execution identity.
    fs::write(root.join("ignored.txt"), "two").unwrap();
    let stale_input = verify(&root);
    assert_eq!(stale_input.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&stale_input.stdout).contains("mismatch: inputs"));
    fs::write(root.join("ignored.txt"), "one").unwrap();

    // Exit codes are stages: a stale tree is 3, contract drift is 4, and a
    // bad or incomplete outcome is 1.
    fs::write(root.join("tracked.txt"), "two").unwrap();
    let stale = verify(&root);
    assert_eq!(stale.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&stale.stdout).contains("mismatch: tree"));
    fs::write(root.join("tracked.txt"), "one").unwrap();

    fs::write(
        root.join("contract.json"),
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"noop","run":"false","inputs":["ignored.txt"]}]}"#,
    )
    .unwrap();
    assert_eq!(verify(&root).status.code(), Some(4));
    fs::write(root.join("contract.json"), contract_text).unwrap();

    let receipt_bytes = fs::read(&receipt).unwrap();
    let mut incomplete: serde_json::Value = serde_json::from_slice(&receipt_bytes).unwrap();
    incomplete.as_object_mut().unwrap().remove("tasks");
    fs::write(&receipt, serde_json::to_vec(&incomplete).unwrap()).unwrap();
    assert_eq!(verify(&root).status.code(), Some(2));

    let mut failed: serde_json::Value = serde_json::from_slice(&receipt_bytes).unwrap();
    failed["tasks"][0]["importable"] = false.into();
    fs::write(&receipt, serde_json::to_vec(&failed).unwrap()).unwrap();
    assert_eq!(verify(&root).status.code(), Some(1));

    failed["tasks"][0]["importable"] = true.into();
    failed["outcome"] = "failed".into();
    fs::write(&receipt, serde_json::to_vec(&failed).unwrap()).unwrap();
    assert_eq!(verify(&root).status.code(), Some(1));

    fs::remove_file(receipt).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn plan_json_identifies_the_graph_and_affected_tasks() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("sykli-plan-{nonce}"));
    fs::create_dir_all(&root).unwrap();
    let input = root.join("input.txt");
    fs::write(&input, "changed").unwrap();
    let contract = root.join("contract.json");
    fs::write(
        &contract,
        format!(
            r#"{{"schema":"sykli-contract.v1","tasks":[{{"name":"build","run":"true","workdir":{},"inputs":["input.txt"]}},{{"name":"test","run":"true","after":["build"]}}]}}"#,
            serde_json::to_string(&root).unwrap()
        ),
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["plan", "--json"])
        .arg(&contract)
        .arg("--changed")
        .arg(&input)
        .output()
        .expect("binary runs");
    fs::remove_dir_all(root).unwrap();

    assert!(out.status.success());
    let plan: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(plan["schema"], "sykli-plan.v1");
    assert_eq!(plan["contract_hash"].as_str().unwrap().len(), 64);
    assert_eq!(plan["tasks"], serde_json::json!(["build", "test"]));
}

#[test]
fn plan_json_without_changed_selects_the_whole_graph() {
    let contract = std::env::temp_dir().join(format!("sykli-plan-all-{}.json", std::process::id()));
    fs::write(
        &contract,
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"build","run":"true"},{"name":"test","run":"true","after":["build"]}]}"#,
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["plan", "--json"])
        .arg(&contract)
        .output()
        .expect("binary runs");
    fs::remove_file(&contract).unwrap();

    assert!(out.status.success());
    let plan: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(plan["tasks"], serde_json::json!(["build", "test"]));
}

#[test]
fn a_json_contract_is_the_default_when_there_is_no_emitter() {
    // `sykli init` writes sykli.json and no sykli.rs; every command must then
    // work without naming the contract, as the README shows.
    let dir = std::env::temp_dir().join(format!(
        "sykli-default-json-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("sykli.json"),
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"true"}]}"#,
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_sykli"))
        .current_dir(&dir)
        .args(["validate", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let verdict: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(verdict["contract"], "sykli.json");
    assert_eq!(verdict["valid"], true);
    // An explicit path is never rewritten.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_sykli"))
        .current_dir(&dir)
        .args(["validate", "missing.json", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    std::fs::remove_dir_all(&dir).unwrap();
}
