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
    assert_eq!(
        explain_json(&root, &[]).1["explanations"][0]["cache"]["status"],
        "available"
    );
    let receipt = root.join(".sykli/mode-receipt.json");
    fs::write(&receipt, first.stdout).unwrap();
    fs::set_permissions(root.join("script"), fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        explain_json(&root, &[]).1["explanations"][0]["cache"]["status"],
        "missing"
    );
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

    // An honest failed record (outcome, exit code and error agree) is exit 1;
    // an outcome that disagrees with its own records is not a receipt (exit 2).
    failed["tasks"][0]["importable"] = true.into();
    failed["outcome"] = "failed".into();
    fs::write(&receipt, serde_json::to_vec(&failed).unwrap()).unwrap();
    assert_eq!(verify(&root).status.code(), Some(2));
    failed["tasks"][0]["outcome"] = "failed".into();
    failed["tasks"][0]["exit_code"] = 1.into();
    failed["tasks"][0]["error"] = "exit status: 1".into();
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
    assert_eq!(plan.as_object().unwrap().len(), 3);
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
    let cached = run("alpha");
    assert_eq!(cached["tasks"][0]["outcome"], "cached");
    assert_eq!(cached["tasks"][0]["inherited_digests"], *digests);
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

#[test]
fn a_receipt_that_contradicts_itself_cannot_verify() {
    let root = graph_repo(
        "forge",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"test -f present"}]}"#,
    );
    let sykli = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_sykli"))
            .args(args)
            .current_dir(&root)
            .output()
            .unwrap()
    };
    let out = sykli(&["run", "sykli.json", "--json"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "the file is absent, so the task fails"
    );
    let mut receipt: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let outside = std::env::temp_dir().join(format!("sykli-forged-{}.json", std::process::id()));
    fs::write(&outside, receipt.to_string()).unwrap();
    let honest = sykli(&[
        "verify",
        outside.to_str().unwrap(),
        "--contract",
        "sykli.json",
    ]);
    assert_eq!(honest.status.code(), Some(1));
    // Relabel the failure as a pass without touching anything else.
    receipt["tasks"][0]["outcome"] = serde_json::json!("passed");
    receipt["outcome"] = serde_json::json!("passed");
    fs::write(&outside, receipt.to_string()).unwrap();
    let forged = sykli(&[
        "verify",
        outside.to_str().unwrap(),
        "--contract",
        "sykli.json",
    ]);
    assert_eq!(
        forged.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&forged.stdout)
    );
    assert!(String::from_utf8_lossy(&forged.stdout).contains("records"));
    // A record whose command differs from the contract is not this contract's receipt.
    receipt["tasks"][0]["outcome"] = serde_json::json!("failed");
    receipt["outcome"] = serde_json::json!("failed");
    receipt["tasks"][0]["command"] = serde_json::json!("true");
    fs::write(&outside, receipt.to_string()).unwrap();
    let swapped = sykli(&[
        "verify",
        outside.to_str().unwrap(),
        "--contract",
        "sykli.json",
    ]);
    assert_eq!(swapped.status.code(), Some(2));
    let _ = fs::remove_file(outside);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_fully_cached_receipt_verifies() {
    let root = graph_repo(
        "cachedverify",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"true","inputs":["sykli.json"]}]}"#,
    );
    let (code, first) = run_json(&root);
    assert_eq!(code, Some(0));
    assert_eq!(first["tasks"][0]["outcome"], "passed");
    let (code, cached) = run_json(&root);
    assert_eq!(code, Some(0));
    assert_eq!(cached["tasks"][0]["outcome"], "cached");
    assert!(
        cached["tasks"][0]["exit_code"].is_null(),
        "restored records carry no exit code"
    );
    let outside = std::env::temp_dir().join(format!("sykli-cached-{}.json", std::process::id()));
    fs::write(&outside, cached.to_string()).unwrap();
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
fn running_outside_a_repository_says_so() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("sykli-nogit-{nonce}"));
    fs::create_dir(&root).unwrap();
    fs::write(
        root.join("sykli.json"),
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"true"}]}"#,
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["run", "sykli.json"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not inside a git repository"));
    fs::remove_dir_all(root).unwrap();
}

fn explain_json(root: &std::path::Path, args: &[&str]) -> (Option<i32>, serde_json::Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["plan", "--explain", "--json"])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    let value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&out.stderr)));
    (out.status.code(), value)
}

#[test]
fn explain_inspects_cache_without_executing_or_restoring_outputs() {
    let root = graph_repo(
        "explain-cache",
        r#"{"schema":"sykli-contract.v1","tasks":[
            {"name":"build","run":"cp seed artifact","inputs":["seed"],"outputs":["artifact"]},
            {"name":"check","run":"cat artifact","inputs":["artifact"],"after":["build"]}
        ]}"#,
    );
    fs::write(root.join("seed"), "one").unwrap();
    // An emitter must never be selected, even when it exists beside JSON.
    fs::write(root.join("sykli.rs"), "this emitter must not execute").unwrap();
    let (code, plan) = explain_json(&root, &[]);
    assert_eq!(code, Some(0));
    assert_eq!(plan["explanations"][0]["selection"][0]["code"], "all_tasks");
    assert_eq!(plan["explanations"][0]["cache"]["code"], "entry_missing");
    assert_eq!(plan["explanations"][1]["cache"]["status"], "deferred");
    assert!(!root.join("artifact").exists());
    assert!(!root.join(".sykli").exists());

    assert_eq!(run_json(&root).0, Some(0));
    fs::remove_file(root.join("artifact")).unwrap();
    let (code, plan) = explain_json(&root, &[]);
    assert_eq!(code, Some(0));
    assert_eq!(plan["explanations"][0]["cache"]["status"], "available");
    assert!(
        plan["explanations"][0]["cache"]["receipt"]
            .as_str()
            .is_some()
    );
    assert_eq!(plan["explanations"][1]["cache"]["status"], "deferred");
    assert!(
        !root.join("artifact").exists(),
        "inspection must not restore"
    );
    assert_eq!(plan, explain_json(&root, &[]).1, "deterministic output");

    // A display filter must not hide the dependency's pending restoration.
    let (_, selected) = explain_json(&root, &["--changed", "./artifact"]);
    assert_eq!(selected["tasks"], serde_json::json!(["check"]));
    assert_eq!(
        selected["explanations"][0]["cache"]["dependencies"],
        serde_json::json!(["build"])
    );
    let (_, selected) = explain_json(&root, &["--changed", "./seed"]);
    assert_eq!(selected["explanations"][0]["selection"][0]["path"], "seed");
    assert_eq!(
        selected["explanations"][1]["selection"][0]["code"],
        "affected_dependency"
    );

    let human = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["plan", "--explain"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(human.status.success());
    assert!(String::from_utf8_lossy(&human.stdout).contains("restoration not attempted"));
    assert!(!root.join("artifact").exists());
    let (code, receipt) = run_json(&root);
    assert_eq!(code, Some(0));
    assert_eq!(receipt["tasks"][0]["outcome"], "cached");
    assert_eq!(receipt["tasks"][1]["outcome"], "cached");

    fs::write(root.join("seed"), "two").unwrap();
    assert_eq!(
        explain_json(&root, &[]).1["explanations"][0]["cache"]["status"],
        "missing"
    );
    assert_eq!(run_json(&root).1["tasks"][0]["outcome"], "passed");
    fs::remove_file(root.join("seed")).unwrap();
    let (code, plan) = explain_json(&root, &[]);
    assert_eq!(code, Some(2));
    assert_eq!(plan["explanations"][0]["cache"]["status"], "input_error");
    assert_eq!(plan["explanations"][1]["cache"]["status"], "deferred");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn explain_and_execution_reject_the_same_damaged_cache_evidence() {
    let root = graph_repo(
        "explain-invalid",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"build","run":"printf ok > artifact","outputs":["artifact"]}]}"#,
    );
    assert_eq!(run_json(&root).0, Some(0));
    let directory = fs::read_dir(root.join(".sykli/cache"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let entry_path = directory.join("entry.json");
    let entry_bytes = fs::read(&entry_path).unwrap();
    let entry: serde_json::Value = serde_json::from_slice(&entry_bytes).unwrap();
    let receipt_path = root
        .join(".sykli/receipts")
        .join(entry["receipt"].as_str().unwrap());
    let receipt_bytes = fs::read(&receipt_path).unwrap();
    let artifact = directory.join("outputs/0");
    for (path, bytes, expected) in [
        (&entry_path, b"{}".as_slice(), "entry_invalid"),
        (&receipt_path, b"{}".as_slice(), "provenance_invalid"),
        (&artifact, b"bad".as_slice(), "artifact_invalid"),
    ] {
        fs::write(path, bytes).unwrap();
        let (code, plan) = explain_json(&root, &[]);
        assert_eq!(
            code,
            Some(0),
            "invalid cache is a fallback, not a tool error"
        );
        assert_eq!(plan["explanations"][0]["cache"]["status"], "invalid");
        assert_eq!(plan["explanations"][0]["cache"]["code"], expected);
        assert_eq!(run_json(&root).1["tasks"][0]["outcome"], "passed");
        // Restore the original evidence for the next independent corruption.
        fs::write(&entry_path, &entry_bytes).unwrap();
        fs::write(&receipt_path, &receipt_bytes).unwrap();
    }
    fs::remove_file(&artifact).unwrap();
    assert_eq!(
        explain_json(&root, &[]).1["explanations"][0]["cache"]["code"],
        "artifact_missing"
    );
    assert_eq!(run_json(&root).1["tasks"][0]["outcome"], "passed");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn explain_rejects_emitters_and_defaults_to_json_without_creating_it() {
    let root = graph_repo(
        "explain-json",
        r#"{"schema":"sykli-contract.v1","tasks":[]}"#,
    );
    fs::write(root.join("sykli.rs"), "must not execute").unwrap();
    for args in [
        vec!["plan", "sykli.rs", "--explain"],
        vec!["plan", "--explain", "--target", "build"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
            .args(args)
            .current_dir(&root)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
    }
    fs::remove_file(root.join("sykli.json")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["plan", "--explain"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("sykli init"));
    assert!(!root.join("sykli.json").exists());
    assert!(!root.join(".sykli").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn explain_tracks_inherited_values_and_cached_dependencies_without_outputs() {
    let root = graph_repo(
        "explain-env",
        r#"{"schema":"sykli-contract.v1","tasks":[
            {"name":"a","run":"true","inherit":["SYKLI_EXPLAIN_VALUE"]},
            {"name":"b","run":"true","after":["a"]}
        ]}"#,
    );
    let invoke = |command: &str, value: Option<&str>| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_sykli"));
        cmd.args([command, "--json"])
            .current_dir(&root)
            .env_remove("SYKLI_EXPLAIN_VALUE");
        if command == "plan" {
            cmd.arg("--explain");
        }
        if let Some(value) = value {
            cmd.env("SYKLI_EXPLAIN_VALUE", value);
        }
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()
    };
    invoke("run", None);
    let plan = invoke("plan", None);
    assert_eq!(plan["explanations"][0]["cache"]["status"], "available");
    assert_eq!(plan["explanations"][1]["cache"]["status"], "available");
    assert_eq!(invoke("run", None)["tasks"][1]["outcome"], "cached");
    for value in ["", "private-value"] {
        let plan = invoke("plan", Some(value));
        assert_eq!(plan["explanations"][0]["cache"]["status"], "missing");
        assert_eq!(plan["explanations"][1]["cache"]["status"], "deferred");
        assert!(!plan.to_string().contains("private-value"));
        assert_eq!(invoke("run", Some(value))["tasks"][0]["outcome"], "passed");
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn explain_preserves_filtered_ancestor_input_errors() {
    let root = graph_repo(
        "explain-ancestor-errors",
        r#"{"schema":"sykli-contract.v1","tasks":[
            {"name":"build","run":"true","inputs":["missing-source"]},
            {"name":"other","run":"true","inputs":["missing-config"]},
            {"name":"middle","run":"true","inputs":["middle.txt"],"after":["build"]},
            {"name":"test","run":"true","inputs":["test.txt"],"after":["middle","other"]},
            {"name":"independent","run":"true","inputs":["independent.txt"]}
        ]}"#,
    );
    for path in ["middle.txt", "test.txt", "independent.txt"] {
        fs::write(root.join(path), "input").unwrap();
    }
    let (code, plan) = explain_json(&root, &["--changed", "test.txt"]);
    assert_eq!(code, Some(2));
    assert_eq!(plan["tasks"], serde_json::json!(["test"]));
    let cache = &plan["explanations"][0]["cache"];
    assert_eq!(cache["status"], "deferred");
    assert_eq!(
        cache["input_errors"]["build"],
        "declared input \"missing-source\" is missing or not a file"
    );
    assert_eq!(
        cache["input_errors"]["other"],
        "declared input \"missing-config\" is missing or not a file"
    );
    assert_eq!(cache["input_errors"].as_object().unwrap().len(), 2);

    let (code, direct) = explain_json(&root, &["--changed", "middle.txt"]);
    assert_eq!(code, Some(2));
    assert_eq!(
        direct["explanations"][0]["cache"]["input_errors"]["build"],
        cache["input_errors"]["build"]
    );
    let human = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["plan", "--explain", "--changed", "test.txt"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(human.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&human.stdout).contains("missing-source"));
    assert!(String::from_utf8_lossy(&human.stdout).contains("missing-config"));

    // Errors in unrelated tasks must not poison a selected independent task.
    let (code, independent) = explain_json(&root, &["--changed", "independent.txt"]);
    assert_eq!(code, Some(0));
    assert_eq!(independent["tasks"], serde_json::json!(["independent"]));
    assert_eq!(independent["explanations"][0]["cache"]["status"], "missing");
    assert!(!root.join(".sykli").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn explain_warns_about_undeclared_rust_even_when_every_task_is_selected() {
    let root = graph_repo(
        "coverage-rust",
        r#"{"schema":"sykli-contract.v1","tasks":[
            {"name":"fmt","run":"cargo fmt --check","inputs":["Cargo.toml","src/main.rs"]},
            {"name":"check","run":"true","after":["fmt"]}
        ]}"#,
    );
    fs::create_dir(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"coverage-probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    assert_eq!(run_json(&root).0, Some(0));
    fs::create_dir(root.join("src/bin")).unwrap();
    fs::write(root.join("src/bin/worker.rs"), "fn main( ){ }\n").unwrap();
    assert!(
        !Command::new("cargo")
            .args(["fmt", "--check"])
            .current_dir(&root)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        run_json(&root).0,
        Some(2),
        "missing source must stop before cache reuse"
    );
    let (code, plan) = explain_json(
        &root,
        &["--changed", "src/main.rs", "--changed", "src/bin/worker.rs"],
    );
    assert_eq!(code, Some(2));
    assert_eq!(plan["tasks"], serde_json::json!(["fmt", "check"]));
    assert_eq!(plan["explanations"][0]["cache"]["status"], "input_error");
    let uncovered = |plan: &serde_json::Value| {
        plan["input_coverage"]["paths"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["path"].as_str().unwrap().ends_with("worker.rs") && p["kind"] == "unmapped")
    };
    assert!(uncovered(&plan));
    assert!(
        uncovered(&explain_json(&root, &[]).1),
        "no diff scripting needed for untracked files"
    );
    let human = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["plan", "--explain"])
        .current_dir(&root)
        .output()
        .unwrap();
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(text.contains("warning: no task declares input"));
    assert!(text.contains("worker.rs"));

    // Staging and committing the new file do not hide it from an explicit base comparison.
    assert!(
        Command::new("git")
            .args(["add", "src", "Cargo.toml"])
            .current_dir(&root)
            .status()
            .unwrap()
            .success()
    );
    assert!(uncovered(&explain_json(&root, &[]).1));
    assert!(
        Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "new source"
            ])
            .current_dir(&root)
            .status()
            .unwrap()
            .success()
    );
    assert!(!uncovered(&explain_json(&root, &[]).1));
    assert_eq!(
        run_json(&root).0,
        Some(2),
        "committing an undeclared file must not hide it from run"
    );
    let (code, empty_selection) = explain_json(&root, &["--changed", "unrelated-doc.md"]);
    assert_eq!(code, Some(2));
    assert_eq!(empty_selection["tasks"], serde_json::json!([]));
    assert!(
        !empty_selection["input_coverage"]["cargo_missing_inputs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(uncovered(&explain_json(&root, &["--base", "HEAD~1"]).1));

    // Declaring the dependency both explains its coverage and invalidates the cached pass.
    let mut contract: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("sykli.json")).unwrap()).unwrap();
    contract["tasks"][0]["inputs"]
        .as_array_mut()
        .unwrap()
        .push("src/bin/worker.rs".into());
    fs::write(
        root.join("sykli.json"),
        serde_json::to_vec(&contract).unwrap(),
    )
    .unwrap();
    let (_, fixed) = explain_json(&root, &["--base", "HEAD~1"]);
    assert!(!uncovered(&fixed));
    assert_eq!(fixed["explanations"][0]["cache"]["status"], "missing");
    assert_eq!(run_json(&root).0, Some(1));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn explain_coverage_separates_metadata_and_preserves_deleted_and_hint_paths() {
    let root = graph_repo(
        "coverage-paths",
        r#"{"schema":"sykli-contract.v1","tasks":[
        {"name":"check","run":"true","workdir":"sub","inputs":["input"]}
    ]}"#,
    );
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("sub/input"), "input").unwrap();
    fs::write(root.join("old name"), "old").unwrap();
    assert!(
        Command::new("git")
            .args(["add", "."])
            .current_dir(&root)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "paths"
            ])
            .current_dir(&root)
            .status()
            .unwrap()
            .success()
    );
    fs::rename(root.join("old name"), root.join("new name")).unwrap();
    #[cfg(unix)]
    fs::write(root.join("line\nbreak"), "new").unwrap();
    let (_, plan) = explain_json(
        &root,
        &[
            "--changed",
            "sub/../sub/input",
            "--changed",
            "sykli.json",
            "--changed",
            "sykli.lock",
        ],
    );
    let paths = plan["input_coverage"]["paths"].as_array().unwrap();
    #[cfg(unix)]
    assert!(paths.iter().any(|p| p["path"] == "line\nbreak"));
    for path in ["sykli.json", "sykli.lock"] {
        assert!(
            paths
                .iter()
                .any(|p| p["path"] == path && p["kind"] == "evaluation_metadata")
        );
    }
    for path in ["old name", "new name"] {
        assert!(
            paths
                .iter()
                .any(|p| p["path"] == path && p["kind"] == "unmapped")
        );
    }
    assert!(
        paths
            .iter()
            .any(|p| p["kind"] == "declared_input" && p["tasks"] == serde_json::json!(["check"]))
    );
    let invalid = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["plan", "--explain", "--base", "not-a-ref"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
    assert!(
        Command::new("git")
            .args(["update-ref", "-d", "HEAD"])
            .current_dir(&root)
            .status()
            .unwrap()
            .success()
    );
    let (code, unborn) = explain_json(&root, &[]);
    assert_eq!(code, Some(0));
    assert!(unborn["input_coverage"]["base_commit"].is_null());
    assert!(
        !unborn["input_coverage"]["paths"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn contract_preview_does_not_accept_drift_or_execute_commands() {
    let root = graph_repo(
        "preview",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"check","run":"true"}]}"#,
    );
    assert!(
        Command::new(env!("CARGO_BIN_EXE_sykli"))
            .args(["lock", "sykli.json"])
            .current_dir(&root)
            .output()
            .unwrap()
            .status
            .success()
    );
    let lock = fs::read(root.join("sykli.lock")).unwrap();
    fs::write(
        root.join("sykli.json"),
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"check","run":"touch executed"}]}"#,
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["plan", "--explain", "--json"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let (code, preview) = explain_json(&root, &["--preview"]);
    assert_eq!(code, Some(0));
    assert_eq!(preview["contract_preview"]["matches_lock"], false);
    assert!(preview["contract_preview"]["pinned_hash"].is_string());
    assert_eq!(fs::read(root.join("sykli.lock")).unwrap(), lock);
    assert!(!root.join("executed").exists());
    assert_eq!(run_json(&root).0, Some(2));
    assert!(!root.join("executed").exists());
    fs::remove_dir_all(root).unwrap();
}
