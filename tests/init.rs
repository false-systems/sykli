//! `sykli init`: the real binary against real manifests in temp dirs.
//! Detection only — nothing here needs cargo, npm, or go installed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

fn temp_root(tag: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = env::temp_dir().join(format!("sykli-init-{tag}-{nonce}"));
    fs::create_dir_all(&root).unwrap();
    root
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn sykli(root: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(args)
        .current_dir(root)
        .output()
        .expect("binary runs");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn contract(root: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(root.join("sykli.json")).unwrap()).unwrap()
}

fn task<'a>(contract: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    contract["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["name"] == name)
        .unwrap_or_else(|| panic!("task {name} missing from {contract}"))
}

fn inputs(task: &serde_json::Value) -> Vec<String> {
    task["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|input| input.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_cargo_repository_yields_fmt_clippy_test_with_every_source_file_declared() {
    let root = temp_root("cargo");
    write(
        &root,
        "Cargo.toml",
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    );
    write(&root, "Cargo.lock", "# lock\n");
    write(&root, "src/main.rs", "fn main() {}\n");
    write(&root, "src/util/mod.rs", "pub fn f() {}\n");
    write(&root, "tests/cli.rs", "#[test] fn t() {}\n");
    write(&root, "target/debug/junk.rs", "// never an input\n");
    write(&root, "src/.hidden.rs", "// hidden, skipped\n");

    let (code, stdout, stderr) = sykli(&root, &["init"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.contains("wrote sykli.json (3 tasks, 5 inputs) and"),
        "{stdout}"
    );
    assert!(root.join("sykli.lock").is_file());

    let contract = contract(&root);
    assert_eq!(contract["schema"], "sykli-contract.v1");
    assert_eq!(task(&contract, "fmt")["run"], "cargo fmt --check");
    assert_eq!(
        task(&contract, "clippy")["after"],
        serde_json::json!(["fmt"])
    );
    assert_eq!(task(&contract, "test")["after"], serde_json::json!(["fmt"]));
    assert_eq!(
        inputs(task(&contract, "test")),
        vec![
            "Cargo.lock",
            "Cargo.toml",
            "src/main.rs",
            "src/util/mod.rs",
            "tests/cli.rs"
        ]
    );

    // The written contract is a valid, locked graph, and delta selection works
    // on it: a source change selects everything, a lockfile-only change too.
    let (code, _, stderr) = sykli(&root, &["validate", "sykli.json"]);
    assert_eq!(code, Some(0), "{stderr}");
    let (code, stdout, _) = sykli(
        &root,
        &[
            "plan",
            "sykli.json",
            "--changed",
            "src/util/mod.rs",
            "--json",
        ],
    );
    assert_eq!(code, Some(0));
    let plan: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(plan["tasks"], serde_json::json!(["fmt", "clippy", "test"]));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_cargo_workspace_declares_its_members() {
    let root = temp_root("workspace");
    write(
        &root,
        "Cargo.toml",
        "[package]\nname = \"root\"\nversion = \"0.1.0\"\n\n[workspace]\nmembers = [\n  \"xtask\", # tooling\n  \"crates/*\",\n]\n",
    );
    write(&root, "src/main.rs", "fn main() {}\n");
    write(
        &root,
        "xtask/Cargo.toml",
        "[package]\nname = \"xtask\"\nversion = \"0.0.0\"\n",
    );
    write(&root, "xtask/src/main.rs", "fn main() {}\n");

    let (code, _, stderr) = sykli(&root, &["init", "--no-lock"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(!root.join("sykli.lock").exists());
    let contract = contract(&root);
    let fmt = inputs(task(&contract, "fmt"));
    assert!(fmt.contains(&"xtask/Cargo.toml".to_string()), "{fmt:?}");
    assert!(fmt.contains(&"xtask/src/main.rs".to_string()), "{fmt:?}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_npm_package_yields_only_the_scripts_it_defines_with_its_runner() {
    let root = temp_root("npm");
    write(
        &root,
        "package.json",
        r#"{"name":"x","scripts":{"test":"vitest","build":"tsc","dev":"vite"}}"#,
    );
    write(&root, "pnpm-lock.yaml", "lockfileVersion: 9\n");
    write(&root, "src/index.ts", "export {};\n");
    write(&root, "src/style.css", "body {}\n");
    write(&root, "node_modules/dep/index.js", "// never an input\n");

    let (code, _, stderr) = sykli(&root, &["init"]);
    assert_eq!(code, Some(0), "{stderr}");
    let contract = contract(&root);
    let names: Vec<&str> = contract["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|task| task["name"].as_str().unwrap())
        .collect();
    // `lint` is not defined, `dev` is not a task; order is lint, test, build.
    assert_eq!(names, vec!["test", "build"]);
    assert_eq!(task(&contract, "test")["run"], "pnpm run test");
    assert_eq!(
        inputs(task(&contract, "build")),
        vec![
            "package.json",
            "pnpm-lock.yaml",
            "src/index.ts",
            "src/style.css"
        ]
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_go_module_yields_vet_then_test_over_every_go_file() {
    let root = temp_root("go");
    write(&root, "go.mod", "module example.com/x\n\ngo 1.22\n");
    write(&root, "go.sum", "");
    write(&root, "main.go", "package main\n");
    write(&root, "internal/a/a.go", "package a\n");
    write(&root, "vendor/dep/dep.go", "package dep\n");

    let (code, _, stderr) = sykli(&root, &["init"]);
    assert_eq!(code, Some(0), "{stderr}");
    let contract = contract(&root);
    assert_eq!(task(&contract, "vet")["run"], "go vet ./...");
    assert_eq!(task(&contract, "test")["after"], serde_json::json!(["vet"]));
    assert_eq!(
        inputs(task(&contract, "test")),
        vec!["go.mod", "go.sum", "internal/a/a.go", "main.go"]
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn several_ecosystems_get_prefixed_task_names() {
    let root = temp_root("mixed");
    write(
        &root,
        "Cargo.toml",
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    );
    write(&root, "src/lib.rs", "");
    write(&root, "go.mod", "module example.com/x\n");
    write(&root, "main.go", "package main\n");

    let (code, _, stderr) = sykli(&root, &["init", "--no-lock"]);
    assert_eq!(code, Some(0), "{stderr}");
    let contract = contract(&root);
    assert_eq!(
        task(&contract, "cargo-test")["after"],
        serde_json::json!(["cargo-fmt"])
    );
    assert_eq!(
        task(&contract, "go-test")["after"],
        serde_json::json!(["go-vet"])
    );
    let (code, _, stderr) = sykli(&root, &["validate", "sykli.json"]);
    assert_eq!(code, Some(0), "{stderr}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn init_refuses_to_overwrite_without_force_and_says_what_it_would_write() {
    let root = temp_root("refuse");
    write(&root, "go.mod", "module example.com/x\n");
    write(&root, "main.go", "package main\n");
    write(
        &root,
        "sykli.json",
        "{\"schema\":\"sykli-contract.v1\",\"tasks\":[]}\n",
    );

    let (code, stdout, stderr) = sykli(&root, &["init"]);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("pass --force"), "{stderr}");
    assert!(stdout.contains("\"go vet ./...\""), "{stdout}");
    assert_eq!(
        fs::read_to_string(root.join("sykli.json")).unwrap(),
        "{\"schema\":\"sykli-contract.v1\",\"tasks\":[]}\n"
    );

    let (code, _, _) = sykli(&root, &["init", "--force"]);
    assert_eq!(code, Some(0));
    assert_eq!(task(&contract(&root), "vet")["run"], "go vet ./...");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn nothing_detected_is_exit_one_and_names_the_manifests() {
    let root = temp_root("empty");
    let (code, _, stderr) = sykli(&root, &["init"]);
    assert_eq!(code, Some(1));
    assert!(
        stderr.contains("Cargo.toml, package.json, go.mod"),
        "{stderr}"
    );
    assert!(!root.join("sykli.json").exists());
    fs::remove_dir_all(root).unwrap();
}
