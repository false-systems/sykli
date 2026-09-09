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

#[cfg(unix)]
#[test]
fn executable_mode_invalidates_cache_and_receipt_inputs() {
    use std::os::unix::fs::PermissionsExt;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("sykli-mode-{nonce}"));
    fs::create_dir(&root).unwrap();
    // Ignore the input so receipt verification cannot rely on Git's mode tracking.
    fs::write(root.join(".gitignore"), "script\n").unwrap();
    fs::write(root.join("script"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::write(root.join("sykli.json"), r#"{"schema":"sykli-contract.v1","tasks":[{"name":"mode","run":"./script","inputs":["script"]}]}"#).unwrap();
    for args in [
        vec!["init", "--quiet"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=test",
            "-c",
            "user.email=t@t",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_sykli"))
            .args(["run", "sykli.json", "--json"])
            .current_dir(&root)
            .output()
            .unwrap()
    };
    fs::set_permissions(root.join("script"), fs::Permissions::from_mode(0o755)).unwrap();
    let first = run();
    assert!(first.status.success());
    let cached: serde_json::Value = serde_json::from_slice(&run().stdout).unwrap();
    assert_eq!(cached["tasks"][0]["outcome"], "cached");
    let receipt = root.join(".sykli/mode-receipt.json");
    fs::write(&receipt, first.stdout).unwrap();
    fs::set_permissions(root.join("script"), fs::Permissions::from_mode(0o644)).unwrap();
    let verified = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .arg("verify")
        .arg(&receipt)
        .args(["--contract", "sykli.json"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(verified.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&verified.stdout).contains("mismatch: inputs"));
    let changed = run();
    assert_eq!(changed.status.code(), Some(1));
    let failed: serde_json::Value = serde_json::from_slice(&changed.stdout).unwrap();
    assert_eq!(failed["tasks"][0]["outcome"], "failed");
    fs::set_permissions(root.join("script"), fs::Permissions::from_mode(0o755)).unwrap();
    let restored = run();
    assert!(restored.status.success());
    let restored: serde_json::Value = serde_json::from_slice(&restored.stdout).unwrap();
    assert_eq!(restored["tasks"][0]["outcome"], "cached");
    fs::remove_dir_all(root).unwrap();
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

/// A git repository with one committed contract, ready for `sykli run`.
fn graph_repo(name: &str, contract: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("sykli-{name}-{nonce}"));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("sykli.json"), contract).unwrap();
    for args in [
        vec!["init", "--quiet"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    root
}

fn run_json(root: &std::path::Path) -> (Option<i32>, serde_json::Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["run", "sykli.json", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    let receipt = serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
    (out.status.code(), receipt)
}

#[test]
fn a_downstream_task_is_not_cached_against_outputs_it_never_saw() {
    let root = graph_repo(
        "after",
        r#"{"schema":"sykli-contract.v1","tasks":[
            {"name":"a","run":"cp seed a.txt","inputs":["seed"],"outputs":["a.txt"]},
            {"name":"b","run":"cat a.txt","after":["a"]}]}"#,
    );
    fs::write(root.join("seed"), "v1").unwrap();
    fs::write(root.join(".gitignore"), "a.txt\n").unwrap();
    let (code, first) = run_json(&root);
    assert_eq!(code, Some(0));
    assert_eq!(first["tasks"][1]["stdout"], "v1");
    let (_, second) = run_json(&root);
    assert_eq!(second["tasks"][1]["outcome"], "cached");
    fs::write(root.join("seed"), "v2").unwrap();
    let (code, third) = run_json(&root);
    assert_eq!(code, Some(0));
    assert_eq!(third["tasks"][0]["outcome"], "passed", "upstream re-ran");
    assert_eq!(
        third["tasks"][1]["outcome"], "passed",
        "downstream must not be served from cache"
    );
    assert_eq!(third["tasks"][1]["stdout"], "v2");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn tasks_get_no_stdin_and_dash_commands_are_commands() {
    let root = graph_repo(
        "stdin",
        r#"{"schema":"sykli-contract.v1","tasks":[
            {"name":"reads","run":"cat; echo done"},
            {"name":"dash","run":"-n 2>/dev/null || echo ran-as-command"}]}"#,
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["run", "sykli.json", "--json"])
        .current_dir(&root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"secret input\n")
            .unwrap();
    }
    let out = child.wait_with_output().unwrap();
    let receipt: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(receipt["tasks"][0]["outcome"], "passed");
    assert_eq!(
        receipt["tasks"][0]["stdout"], "done\n",
        "stdin must be empty for tasks"
    );
    assert_eq!(receipt["tasks"][1]["outcome"], "passed");
    assert!(
        receipt["tasks"][1]["stdout"]
            .as_str()
            .unwrap()
            .contains("ran-as-command")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn run_and_plan_exit_2_when_they_cannot_evaluate() {
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["run", "missing.json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["plan", "missing.json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let root = graph_repo(
        "notes",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"true"}]}"#,
    );
    fs::write(root.join("notes.txt"), "not a contract").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["run", "notes.txt"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("neither a .json contract nor a .rs emitter")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_clean_checkout_is_not_dirty_when_tracked_files_are_ignored_or_under_sykli() {
    let root = graph_repo(
        "dirty",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"true"}]}"#,
    );
    fs::write(root.join("gen.txt"), "generated").unwrap();
    fs::write(root.join(".gitignore"), "gen.txt\n").unwrap();
    fs::create_dir_all(root.join(".sykli/evidence")).unwrap();
    fs::write(root.join(".sykli/evidence/bundle.json"), "{}").unwrap();
    for args in [
        vec![
            "add",
            "-f",
            "gen.txt",
            ".gitignore",
            ".sykli/evidence/bundle.json",
        ],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--quiet",
            "-m",
            "tracked",
        ],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let (code, receipt) = run_json(&root);
    assert_eq!(code, Some(0));
    assert_eq!(receipt["subject"]["dirty"], false, "{}", receipt["subject"]);
    fs::write(root.join("gen.txt"), "changed").unwrap();
    let (_, receipt) = run_json(&root);
    assert_eq!(
        receipt["subject"]["dirty"], true,
        "a modified tracked file is dirty even if ignored"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_truncated_capture_still_verifies() {
    let root = graph_repo(
        "truncate",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"loud","run":"head -c 1200000 /dev/zero | tr '\\0' x"}]}"#,
    );
    let (code, receipt) = run_json(&root);
    assert_eq!(code, Some(0));
    assert_eq!(receipt["tasks"][0]["stdout_truncated"], true);
    assert_eq!(receipt["tasks"][0]["importable"], true);
    let path = root.join("receipt.json");
    fs::write(&path, receipt.to_string()).unwrap();
    // The receipt lives outside the tree it describes.
    let outside = std::env::temp_dir().join(format!("sykli-receipt-{}.json", std::process::id()));
    fs::rename(&path, &outside).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .arg("verify")
        .arg(&outside)
        .args(["--contract", "sykli.json"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let _ = fs::remove_file(outside);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn inherited_variables_reach_the_task_but_only_their_digests_reach_the_receipt() {
    let root = graph_repo(
        "inherit",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"show","run":"printf %s \"$SYKLI_INHERIT_TEST\"","inherit":["SYKLI_INHERIT_TEST","SYKLI_UNSET_TEST"]}]}"#,
    );
    let run = |value: &str| {
        let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
            .args(["run", "sykli.json", "--json"])
            .env("SYKLI_INHERIT_TEST", value)
            .env_remove("SYKLI_UNSET_TEST")
            .current_dir(&root)
            .output()
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()
    };
    let first = run("alpha");
    assert_eq!(first["tasks"][0]["stdout"], "alpha");
    let digests = &first["tasks"][0]["inherited_digests"];
    assert_eq!(digests["SYKLI_UNSET_TEST"], "absent");
    let digest = digests["SYKLI_INHERIT_TEST"].as_str().unwrap();
    assert_eq!(digest.len(), 64);
    assert!(
        !first.to_string().contains("alpha\"") || first["tasks"][0]["stdout"] == "alpha",
        "value appears only as task output"
    );
    assert!(
        !digests.to_string().contains("alpha"),
        "receipt records the digest, never the value"
    );
    assert_eq!(run("alpha")["tasks"][0]["outcome"], "cached");
    let changed = run("beta");
    assert_eq!(
        changed["tasks"][0]["outcome"], "passed",
        "a different inherited value is a different task"
    );
    assert_eq!(changed["tasks"][0]["stdout"], "beta");
    // A name cannot be both declared and inherited.
    fs::write(
        root.join("bad.json"),
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"x","run":"true","env":{"A":"1"},"inherit":["A"]}]}"#,
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["validate", "bad.json"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("inherits"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn one_lock_file_pins_every_contract_in_its_directory() {
    let root = graph_repo(
        "locks",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"a","run":"true"}]}"#,
    );
    fs::write(
        root.join("release.json"),
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"b","run":"true"}]}"#,
    )
    .unwrap();
    let sykli = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_sykli"))
            .args(args)
            .current_dir(&root)
            .output()
            .unwrap()
    };
    assert_eq!(sykli(&["lock", "sykli.json"]).status.code(), Some(0));
    assert_eq!(sykli(&["lock", "release.json"]).status.code(), Some(0));
    let lock: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("sykli.lock")).unwrap()).unwrap();
    assert_eq!(lock["schema"], "sykli-lock.v2");
    assert!(lock["contracts"]["sykli.json"].is_string());
    assert!(lock["contracts"]["release.json"].is_string());
    assert_eq!(sykli(&["validate", "sykli.json"]).status.code(), Some(0));
    assert_eq!(sykli(&["validate", "release.json"]).status.code(), Some(0));
    // Drift in one contract is caught for that contract only.
    fs::write(
        root.join("release.json"),
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"b","run":"false"}]}"#,
    )
    .unwrap();
    assert_eq!(sykli(&["validate", "release.json"]).status.code(), Some(1));
    assert_eq!(sykli(&["validate", "sykli.json"]).status.code(), Some(0));
    // A v1 lock is still honoured and upgraded on the next lock.
    let hash = lock["contracts"]["sykli.json"].as_str().unwrap();
    fs::write(
        root.join("sykli.lock"),
        format!(r#"{{"schema":"sykli-lock.v1","contract_hash":"{hash}"}}"#),
    )
    .unwrap();
    assert_eq!(sykli(&["validate", "sykli.json"]).status.code(), Some(0));
    assert_eq!(sykli(&["lock", "sykli.json"]).status.code(), Some(0));
    let upgraded: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("sykli.lock")).unwrap()).unwrap();
    assert_eq!(upgraded["schema"], "sykli-lock.v2");
    fs::remove_dir_all(root).unwrap();
}
