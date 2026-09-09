//! `assess`/`inspect` behaviour every supported platform must share, driven by
//! a committed real evidence bundle (false-systems/sykli#25) and no `gh`.
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn temp(name: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("sykli-portable-{name}-{nonce}"));
    fs::create_dir_all(&root).unwrap();
    root
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// The committed bundle, copied so `assess` may save records beside it.
fn bundle_copy(root: &Path) -> PathBuf {
    let source = fs::read_dir(fixtures().join("bundle"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.join("manifest.json").is_file())
        .expect("committed bundle");
    let target = root.join(source.file_name().unwrap());
    copy_dir(&source, &target);
    target
}

fn sykli(args: &[&str], cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("binary runs")
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "not JSON: {e}\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn a_committed_bundle_replays_identically_on_every_platform() {
    let root = temp("replay");
    let bundle = bundle_copy(&root);
    let requirements = fixtures().join("requirements/review-readiness.json");
    let args = [
        "assess",
        bundle.to_str().unwrap(),
        "--requirements",
        requirements.to_str().unwrap(),
        "--json",
    ];
    let out = sykli(&args, &root);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc = json(&out);
    assert_eq!(doc["schema"], "sykli-assessment.v1");
    assert_eq!(doc["result"], "unproven");
    assert_eq!(doc["obligations"]["ci"]["result"], "satisfied");
    assert_eq!(doc["obligations"]["review"]["reason"], "approval-missing");
    assert_eq!(
        doc["candidate"]["head"]["commit"],
        "0e1982a239a0bd02299ab17de29bd52d17842763"
    );
    assert_eq!(doc["evaluation_basis"], "collection-end");
    assert!(
        bundle.join("assessments").is_dir(),
        "records saved beside the copy"
    );
    // Human rows, Mermaid and explanation come from the same result.
    let text = sykli(&args[..4], &root);
    assert_eq!(text.status.code(), Some(3));
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("UNPROVEN"), "{text}");
    assert!(text.contains("? review"), "{text}");
    let graph = sykli(&[&args[..4], &["--graph", "mermaid"]].concat(), &root);
    assert!(String::from_utf8_lossy(&graph.stdout).starts_with("flowchart BT"));
    let why = sykli(&[&args[..4], &["--why", "review"]].concat(), &root);
    assert!(String::from_utf8_lossy(&why.stdout).contains("approval-missing"));
    // Same bundle, second copy, same document (minus nothing: the id is content-derived).
    let again = json(&sykli(&args, &root));
    assert_eq!(doc, again);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_tampered_or_torn_copy_is_rejected() {
    let root = temp("tamper");
    let bundle = bundle_copy(&root);
    let requirements = fixtures().join("requirements/review-readiness.json");
    let object = fs::read_dir(bundle.join("objects"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| fs::read(p).unwrap().trim_ascii() == b"[]")
        .expect("empty review listing object");
    fs::write(&object, b"[{\"id\":1}]").unwrap();
    let out = sykli(
        &[
            "assess",
            bundle.to_str().unwrap(),
            "--requirements",
            requirements.to_str().unwrap(),
            "--json",
        ],
        &root,
    );
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json(&out)["code"], "invalid-bundle");
    let torn = root.join("torn");
    fs::create_dir_all(&torn).unwrap();
    fs::copy(bundle.join("manifest.json"), torn.join("manifest.json")).unwrap();
    let out = sykli(
        &[
            "assess",
            torn.to_str().unwrap(),
            "--requirements",
            requirements.to_str().unwrap(),
            "--json",
        ],
        &root,
    );
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json(&out)["code"], "invalid-bundle");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn inspect_without_gh_is_a_tool_error_not_a_verdict() {
    let root = temp("nogh");
    let store = root.join("evidence");
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args([
            "inspect",
            "--repo",
            "false-systems/sykli",
            "--pr",
            "25",
            "--store",
        ])
        .arg(&store)
        .arg("--json")
        .env("PATH", root.join("empty").as_os_str())
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc = json(&out);
    assert_eq!(doc["schema"], "sykli-error.v1");
    assert_eq!(doc["code"], "provider-unavailable");
    assert!(
        doc["message"].as_str().unwrap().contains("gh not found"),
        "{doc}"
    );
    assert!(
        !store.exists(),
        "no bundle is published when nothing was read"
    );
    let _ = fs::remove_dir_all(root);
}
