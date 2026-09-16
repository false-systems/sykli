//! The experiment is Linux CI glue; exercise it with real Git points and Sykli.
#![cfg(unix)]

use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn commit(root: &Path) -> String {
    git(root, &["add", "."]);
    git(
        root,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "fixture",
        ],
    );
    git(root, &["rev-parse", "HEAD"])
}

fn full_run(root: &Path) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["run", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    fs::write(root.join(".sykli/full.json"), &out.stdout).unwrap();
    serde_json::from_slice(&out.stdout).unwrap()
}

fn shadow(root: &Path, base: &str, name: &str, expected_code: i32) -> Value {
    let output = root.join(".sykli").join(name);
    let out = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/ci_shadow.py"))
        .args(["--base", base, "--binary", env!("CARGO_BIN_EXE_sykli")])
        .arg("--receipt")
        .arg(root.join(".sykli/full.json"))
        .arg("--output")
        .arg(&output)
        .env_remove("GITHUB_STEP_SUMMARY")
        .current_dir(root)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(expected_code),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&fs::read(output.join("report.json")).unwrap()).unwrap()
}

#[test]
fn shadow_reports_real_changes_false_reuse_and_unproven_comparisons() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("sykli-shadow-test-{nonce}"));
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q"]);
    fs::write(root.join(".gitignore"), "/.sykli/\n/artifact\n").unwrap();
    fs::write(root.join("input"), "before").unwrap();
    fs::write(root.join("hidden"), "ok").unwrap();
    fs::write(root.join("old name"), "renamed").unwrap();
    fs::write(root.join("line\nbreak"), "before").unwrap();
    fs::write(root.join("README.md"), "before").unwrap();
    fs::write(
        root.join("sykli.json"),
        json!({
            "schema": "sykli-contract.v1",
            "tasks": [
                {"name":"declared","run":"cat input","inputs":["input"]},
                {"name":"underdeclared","run":"test \"$(cat hidden)\" = ok"},
                {"name":"artifact","run":"cat hidden > artifact","outputs":["artifact"]},
                {"name":"stable","run":"true"},
                {"name":"downstream","run":"true","after":["declared"]}
            ]
        })
        .to_string(),
    )
    .unwrap();
    let base = commit(&root);
    fs::write(root.join("input"), "after").unwrap();
    fs::write(root.join("hidden"), "bad").unwrap();
    fs::write(root.join("line\nbreak"), "after").unwrap();
    fs::rename(root.join("old name"), root.join("new name")).unwrap();
    fs::create_dir(root.join(".cargo")).unwrap();
    fs::write(root.join(".cargo/config.toml"), "[build]\n").unwrap();
    let candidate = commit(&root);
    let full = full_run(&root);
    assert_eq!(full["tasks"][3]["source"], "task");
    let artifact = fs::read(root.join("artifact")).unwrap();
    let worktrees = git(&root, &["worktree", "list", "--porcelain"]);
    let report = shadow(&root, &base, "observation", 0);
    assert_eq!(report["status"], "observed");
    assert_eq!(report["base_commit"], base);
    assert_eq!(report["candidate_commit"], candidate);
    assert_eq!(
        report["disagreements"],
        json!(["underdeclared", "artifact"])
    );
    let tasks = report["tasks"].as_array().unwrap();
    let row = |name: &str| tasks.iter().find(|t| t["task"] == name).unwrap();
    assert_eq!(row("declared")["affected"], true);
    assert_eq!(row("declared")["cache"]["status"], "missing");
    assert_eq!(row("downstream")["cache"]["status"], "deferred");
    assert_eq!(row("stable")["comparison"], "agrees");
    assert_eq!(report["coverage"]["status"], "unresolved");
    assert_eq!(report["coverage_qualified_reused_task_ms"], 0);
    let unresolved = report["coverage"]["unresolved_paths"].as_array().unwrap();
    for path in [
        ".cargo/config.toml",
        "hidden",
        "old name",
        "new name",
        "line\nbreak",
    ] {
        assert!(unresolved.contains(&json!(path)));
    }
    assert!(!unresolved.contains(&json!("input")));
    assert_eq!(
        report["potential_reused_task_ms"],
        row("stable")["full_duration_ms"]
    );
    assert!(
        report["unmapped_paths"]
            .as_array()
            .unwrap()
            .contains(&json!("hidden"))
    );
    let changes = report["changes"].as_array().unwrap();
    assert!(changes.contains(&json!({"status":"D","path":"old name"})));
    assert!(changes.contains(&json!({"status":"A","path":"new name"})));
    assert!(changes.contains(&json!({"status":"M","path":"line\nbreak"})));
    assert_eq!(
        fs::read(root.join("artifact")).unwrap(),
        artifact,
        "shadow never restores into the full-run checkout"
    );
    assert_eq!(git(&root, &["worktree", "list", "--porcelain"]), worktrees);

    // A cached reference is not an independent full execution.
    full_run(&root);
    let cached = shadow(&root, &base, "cached-reference", 0);
    let stable = cached["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["task"] == "stable")
        .unwrap();
    assert_eq!(stable["comparison"], "not_independently_executed");
    assert_eq!(cached["potential_reused_task_ms"], 0);
    assert_eq!(
        cached["independent_comparisons"], 1,
        "the failing task still executed independently"
    );
    let summary = fs::read_to_string(root.join(".sykli/cached-reference/summary.md")).unwrap();
    assert!(summary.contains("baseline cache evidence"));
    assert!(summary.contains("candidate source"));

    // Moving the candidate makes the old reference unusable, not an agreement.
    fs::write(root.join("new file"), "new").unwrap();
    commit(&root);
    let stale = shadow(&root, &base, "stale-reference", 2);
    assert_eq!(stale["status"], "unavailable");
    assert!(stale["error"].as_str().unwrap().contains("does not verify"));
    assert_eq!(
        git(&root, &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        1
    );

    // A reviewed README edit is exempt; other Markdown files are not.
    let docs_base = git(&root, &["rev-parse", "HEAD"]);
    fs::write(root.join("README.md"), "after").unwrap();
    // Editing metadata must not make every ordinary contract update unresolved.
    let mut declaration = fs::read_to_string(root.join("sykli.json")).unwrap();
    declaration.push('\n');
    fs::write(root.join("sykli.json"), declaration).unwrap();
    assert!(
        Command::new(env!("CARGO_BIN_EXE_sykli"))
            .args(["lock", "sykli.json"])
            .current_dir(&root)
            .output()
            .unwrap()
            .status
            .success()
    );
    commit(&root);
    fs::remove_dir_all(root.join(".sykli/cache")).unwrap();
    full_run(&root);
    let docs = shadow(&root, &docs_base, "readme-only", 0);
    assert_eq!(docs["coverage"]["status"], "accounted");
    assert_eq!(docs["coverage"]["unresolved_paths"], json!([]));
    assert_eq!(
        docs["coverage"]["exempted_paths"],
        json!([
            {"path": "README.md", "reason": "repository-readme-only"},
            {"path": "sykli.json", "reason": "evaluation-metadata"},
            {"path": "sykli.lock", "reason": "evaluation-metadata"}
        ])
    );
    assert_eq!(
        docs["coverage_qualified_reused_task_ms"],
        docs["potential_reused_task_ms"]
    );
    full_run(&root);
    let docs_cached = shadow(&root, &docs_base, "docs-cached", 0);
    assert_eq!(docs_cached["independent_comparisons"], 0);
    assert!(
        fs::read_to_string(root.join(".sykli/docs-cached/summary.md"))
            .unwrap()
            .contains("No independent comparison")
    );
    let rename_base = git(&root, &["rev-parse", "HEAD"]);
    fs::rename(root.join("README.md"), root.join("other.md")).unwrap();
    commit(&root);
    full_run(&root);
    let renamed = shadow(&root, &rename_base, "renamed-readme", 0);
    assert_eq!(renamed["coverage"]["exempted_paths"], json!([]));
    assert_eq!(
        renamed["coverage"]["unresolved_paths"],
        json!(["README.md", "other.md"])
    );
    fs::remove_dir_all(root).unwrap();
}
