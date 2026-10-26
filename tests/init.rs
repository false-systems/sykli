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

#[cfg(unix)]
#[test]
fn symlinks_are_never_inputs_and_a_link_cycle_does_not_hang() {
    let root = temp_root("symlinks");
    write(&root, "go.mod", "module example.com/x\n");
    write(&root, "main.go", "package main\n");
    write(&root, "pkg/a.go", "package pkg\n");
    std::os::unix::fs::symlink(&root, root.join("pkg/loop")).unwrap();
    std::os::unix::fs::symlink(root.join("main.go"), root.join("pkg/linked.go")).unwrap();
    let (code, _, stderr) = sykli(&root, &["init", "--no-lock"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        inputs(task(&contract(&root), "test")),
        vec!["go.mod", "main.go", "pkg/a.go"]
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cargo_skips_only_the_root_target_directory() {
    let root = temp_root("target");
    write(
        &root,
        "Cargo.toml",
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    );
    write(
        &root,
        "src/target/mod.rs",
        "// a module called target is source\n",
    );
    write(&root, "src/lib.rs", "mod target;\n");
    write(&root, "target/debug/build.rs", "// never\n");
    let (code, _, stderr) = sykli(&root, &["init", "--no-lock"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        inputs(task(&contract(&root), "fmt")),
        vec!["Cargo.toml", "src/lib.rs", "src/target/mod.rs"]
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn workspace_member_globs_expand_to_crates_and_paths_are_normalized() {
    let root = temp_root("globs");
    write(
        &root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"./tools/xtask/\", \"crates/*\"]\n",
    );
    write(
        &root,
        "tools/xtask/Cargo.toml",
        "[package]\nname = \"xtask\"\nversion = \"0.0.0\"\n",
    );
    write(&root, "tools/xtask/src/main.rs", "fn main() {}\n");
    write(
        &root,
        "crates/a/Cargo.toml",
        "[package]\nname = \"a\"\nversion = \"0.0.0\"\n",
    );
    write(&root, "crates/a/src/lib.rs", "");
    write(&root, "crates/notes.md", "not a crate\n");
    let (code, _, stderr) = sykli(&root, &["init", "--no-lock"]);
    assert_eq!(code, Some(0), "{stderr}");
    let fmt = inputs(task(&contract(&root), "fmt"));
    for expected in [
        "tools/xtask/Cargo.toml",
        "tools/xtask/src/main.rs",
        "crates/a/Cargo.toml",
        "crates/a/src/lib.rs",
    ] {
        assert!(
            fmt.contains(&expected.to_string()),
            "{expected} missing from {fmt:?}"
        );
    }
    assert!(!fmt.iter().any(|input| input.starts_with("./")), "{fmt:?}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_contract_outside_the_current_directory_is_refused_with_the_reason() {
    let root = temp_root("elsewhere");
    write(&root, "sub/go.mod", "module example.com/x\n");
    write(&root, "sub/main.go", "package main\n");
    let (code, _, stderr) = sykli(&root, &["init", "sub/sykli.json"]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.contains("relative to where sykli runs"), "{stderr}");
    assert!(!root.join("sub/sykli.json").exists());
    let (code, _, _) = sykli(&root.join("sub"), &["init", "--no-lock"]);
    assert_eq!(code, Some(0));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_package_json_without_usable_scripts_explains_the_empty_result() {
    let root = temp_root("noscripts");
    write(
        &root,
        "package.json",
        r#"{"name":"x","scripts":{"dev":"vite"}}"#,
    );
    let (code, _, stderr) = sykli(&root, &["init"]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("none of the scripts lint, test, build"),
        "{stderr}"
    );
    fs::remove_dir_all(root).unwrap();
}

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(root)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A crate whose build and tests read more than `src/`: a vendored crate it
/// depends on by path, the pin a test reads beside it, a file the binary
/// includes, a guide the documentation includes, a README the vendored
/// crate's documentation includes from its manifest directory, test data in
/// a top-level `fixtures/` (Markdown cases a test reads with `read_dir`
/// among them), and a README and a guide in `docs/` a test reads. Beside
/// them, documents, licences and CI configuration nothing reads.
const CRATE: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ndep = { path = \"vendor/dep\" }\n",
    ),
    ("Cargo.lock", "# lock\n"),
    (".gitignore", "/target\n"),
    (
        "src/main.rs",
        "#![doc = include_str!(\"../GUIDE.md\")]\nconst LAUNCH: &str = include_str!(\"launch.zsh\");\nfn main() {\n    print!(\"{LAUNCH}\");\n}\n",
    ),
    ("src/launch.zsh", "print launched\n"),
    (
        "vendor/dep/Cargo.toml",
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\n",
    ),
    (
        "vendor/dep/src/lib.rs",
        "#![doc = include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/README.md\"))]\npub fn f() {}\n",
    ),
    ("vendor/dep/README.md", "# dep\n"),
    ("vendor/dep.contract", "tag v1\n"),
    (
        "tests/pins.rs",
        "#[test]\nfn pinned() {\n    std::fs::read(\"vendor/dep.contract\").unwrap();\n    std::fs::read(\"fixtures/case/input.txt\").unwrap();\n    std::fs::read(\"README.md\").unwrap();\n    std::fs::read(\"docs/guide.md\").unwrap();\n    for case in std::fs::read_dir(\"fixtures/cases\").unwrap() {\n        std::fs::read(case.unwrap().path()).unwrap();\n    }\n}\n",
    ),
    ("fixtures/case/input.txt", "input\n"),
    ("fixtures/cases/a.md", "# case a\n"),
    ("fixtures/cases/b.md", "# case b\n"),
    ("docs/guide.md", "guide\n"),
    ("docs/adr/0001.md", "# decided\n"),
    (".github/workflows/ci.yml", "on: push\n"),
    ("README.md", "# x\n"),
    ("GUIDE.md", "# how\n"),
    ("CHANGELOG.md", "# changes\n"),
    ("LICENSE", "MIT\n"),
    ("target/debug/x", "built, and ignored\n"),
];

/// What `sykli init` declares for every cargo task of `CRATE`, with
/// `tests/new.txt` untracked: every file git lists, documents included, but
/// the ignored build directory and untracked scratch.
const DECLARED: &[&str] = &[
    ".github/workflows/ci.yml",
    ".gitignore",
    "CHANGELOG.md",
    "Cargo.lock",
    "Cargo.toml",
    "GUIDE.md",
    "LICENSE",
    "README.md",
    "docs/adr/0001.md",
    "docs/guide.md",
    "fixtures/case/input.txt",
    "fixtures/cases/a.md",
    "fixtures/cases/b.md",
    "src/launch.zsh",
    "src/main.rs",
    "tests/new.txt",
    "tests/pins.rs",
    "vendor/dep.contract",
    "vendor/dep/Cargo.toml",
    "vendor/dep/README.md",
    "vendor/dep/src/lib.rs",
];

/// `CRATE`, committed, plus two untracked files that are not ignored: one
/// under `tests/` (which the Cargo completeness rule counts) and one scratch
/// file elsewhere (which nothing reads).
fn crate_repository(tag: &str) -> PathBuf {
    let root = temp_root(tag);
    for (path, contents) in CRATE {
        write(&root, path, contents);
    }
    git(&root, &["init", "--quiet"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "fixture"]);
    write(&root, "tests/new.txt", "a test's new data\n");
    write(&root, "notes/scratch.txt", "untracked, read by nothing\n");
    root
}

fn every_task_declares(contract: &serde_json::Value, expected: &[&str]) {
    for name in ["fmt", "clippy", "test"] {
        assert_eq!(inputs(task(contract, name)), expected, "task {name}");
    }
}

/// A stand-in toolchain beside `root`, `cargo` and `rustc` reporting a
/// version and passing, and the `PATH` that finds it first.
#[cfg(unix)]
fn toolchain(root: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let bin = root.with_extension("bin");
    fs::create_dir_all(&bin).unwrap();
    for tool in ["cargo", "rustc"] {
        let path = bin.join(tool);
        fs::write(&path, format!("#!/bin/sh\necho '{tool} 1.98.0'\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    format!("{}:{}", bin.display(), env::var("PATH").unwrap())
}

/// Each task's outcome (`run --json`) or cache status (`plan --explain
/// --json`), with `path` as `PATH`.
#[cfg(unix)]
fn statuses(root: &Path, path: &str, args: &[&str]) -> Vec<String> {
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(args)
        .env("PATH", path)
        .current_dir(root)
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&out.stderr)));
    let (list, pointer) = if args[0] == "run" {
        ("tasks", "/outcome")
    } else {
        ("explanations", "/cache/status")
    };
    value[list]
        .as_array()
        .unwrap_or_else(|| panic!("{value}"))
        .iter()
        .map(|task| task.pointer(pointer).unwrap().as_str().unwrap().to_string())
        .collect()
}

/// Found on real repositories: `init` left out a vendored crate (and `run`
/// then refused the contract `init` wrote), a file the binary includes, a pin
/// a test reads and a top-level fixture tree, so a change to any of them kept
/// its cached pass.
#[test]
fn in_a_git_repository_init_declares_every_file_a_cargo_build_or_test_can_read() {
    let root = crate_repository("reads");
    let (code, stdout, stderr) = sykli(&root, &["init"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.contains(&format!("(3 tasks, {} inputs)", DECLARED.len())),
        "{stdout}"
    );
    every_task_declares(&contract(&root), DECLARED);

    // What init writes, validate and the completeness rule accept.
    let (code, _, stderr) = sykli(&root, &["validate", "sykli.json"]);
    assert_eq!(code, Some(0), "{stderr}");
    let (code, stdout, stderr) = sykli(&root, &["plan", "--explain", "--json"]);
    assert_eq!(code, Some(0), "{stderr}");
    let plan: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        plan["input_coverage"]["cargo_missing_inputs"],
        serde_json::json!([])
    );
    assert!(plan["input_coverage"]["folder_missing_inputs"].is_null());

    // A contract outside the repository declares the same.
    let gate = temp_root("reads-gate").join("gate/sykli.json");
    let gate_path = gate.to_str().unwrap();
    let (code, _, stderr) = sykli(&root, &["init", "--contract", gate_path]);
    assert_eq!(code, Some(0), "{stderr}");
    let outside: serde_json::Value = serde_json::from_slice(&fs::read(&gate).unwrap()).unwrap();
    every_task_declares(&outside, DECLARED);
    let (code, _, stderr) = sykli(&root, &["validate", gate_path]);
    assert_eq!(code, Some(0), "{stderr}");
    fs::remove_dir_all(gate.parent().unwrap().parent().unwrap()).unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// A contract that leaves out a file a source includes, or a vendored
/// crate's Cargo files, is refused by `validate` as `run` refuses it, before
/// any task, and with the same exit code (2). A document a test names in a
/// string only warns, naming where. A contract its lock does not pin is
/// still exit 1.
#[test]
fn validate_and_run_refuse_a_contract_that_leaves_out_a_file_its_sources_read() {
    let root = crate_repository("refuse-includes");
    let (code, _, stderr) = sykli(&root, &["init"]);
    assert_eq!(code, Some(0), "{stderr}");
    let written = contract(&root);
    for left_out in [
        // Included beside its source, and through `..`.
        "src/launch.zsh",
        "GUIDE.md",
        // Included from the vendored crate's manifest directory.
        "vendor/dep/README.md",
        // The vendored crate's own Cargo files.
        "vendor/dep/Cargo.toml",
        "vendor/dep/src/lib.rs",
    ] {
        let mut edited = written.clone();
        for task in edited["tasks"].as_array_mut().unwrap() {
            task["inputs"]
                .as_array_mut()
                .unwrap()
                .retain(|input| input != left_out);
        }
        fs::write(
            root.join("sykli.json"),
            serde_json::to_vec_pretty(&edited).unwrap(),
        )
        .unwrap();
        let (code, _, stderr) = sykli(&root, &["lock", "sykli.json"]);
        assert_eq!(code, Some(0), "{stderr}");
        let names = |text: &str| {
            let text = text.replace("\\\\", "/");
            text.contains("Cargo inputs are undeclared") && text.contains(left_out)
        };

        let (code, _, stderr) = sykli(&root, &["validate", "sykli.json"]);
        assert_eq!(code, Some(2), "validate without {left_out}: {stderr}");
        assert!(names(&stderr), "{stderr}");
        let (code, stdout, _) = sykli(&root, &["validate", "sykli.json", "--json"]);
        assert_eq!(code, Some(2));
        let verdict: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(verdict["schema"], "sykli-validate.v1");
        assert_eq!(verdict["valid"], false);
        assert!(names(verdict["errors"][0].as_str().unwrap()), "{verdict}");

        let (code, _, stderr) = sykli(&root, &["run", "sykli.json"]);
        assert_eq!(code, Some(2), "run without {left_out}: {stderr}");
        assert!(names(&stderr), "{stderr}");
        assert!(!root.join(".sykli/receipts").exists(), "no task ran");
    }
    // A document a test names in a string warns, with where, and passes.
    for (left_out, at) in [
        ("README.md", "tests/pins.rs:5"),
        ("docs/guide.md", "tests/pins.rs:6"),
    ] {
        let mut edited = written.clone();
        for task in edited["tasks"].as_array_mut().unwrap() {
            task["inputs"]
                .as_array_mut()
                .unwrap()
                .retain(|input| input != left_out);
        }
        fs::write(
            root.join("sykli.json"),
            serde_json::to_vec_pretty(&edited).unwrap(),
        )
        .unwrap();
        let (code, _, stderr) = sykli(&root, &["lock", "sykli.json"]);
        assert_eq!(code, Some(0), "{stderr}");
        let (code, _, stderr) = sykli(&root, &["validate", "sykli.json"]);
        assert_eq!(code, Some(0), "validate without {left_out}: {stderr}");
        let warning = format!(
            "warning: {left_out} is named in {at} but not declared; declare it if a task reads it"
        );
        assert!(stderr.contains(&warning), "{stderr}");
    }
    // Lock drift keeps its own exit code.
    fs::write(
        root.join("sykli.json"),
        serde_json::to_vec_pretty(&written).unwrap(),
    )
    .unwrap();
    let (code, _, stderr) = sykli(&root, &["validate", "sykli.json"]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.contains("sykli lock"), "{stderr}");
    fs::remove_dir_all(root).unwrap();
}

/// Each file init declares, documents included, is in every cargo task's
/// key: a change to any of them misses the cache, a Markdown case a test
/// reads with `read_dir` too. `plan --explain` reads the keys `run` would,
/// without running, so each file is changed, judged and restored; `run`
/// confirms both ends.
#[cfg(unix)]
#[test]
fn a_change_to_any_file_init_declares_misses_the_cache() {
    let root = crate_repository("misses");
    let (code, _, stderr) = sykli(&root, &["init"]);
    assert_eq!(code, Some(0), "{stderr}");
    let path = toolchain(&root);
    let run = || statuses(&root, &path, &["run", "sykli.json", "--json"]);
    let explain = || statuses(&root, &path, &["plan", "--explain", "--json"]);
    assert_eq!(run(), ["passed", "passed", "passed"]);
    assert_eq!(run(), ["cached", "cached", "cached"]);
    assert_eq!(explain(), ["available", "available", "available"]);
    let declared = inputs(task(&contract(&root), "test"));
    assert_eq!(declared, DECLARED);
    for input in &declared {
        let file = root.join(input);
        let original = fs::read(&file).unwrap();
        let mut changed = original.clone();
        changed.extend_from_slice(b"\n");
        fs::write(&file, changed).unwrap();
        // `fmt` has no entry for its new key, and `clippy` and `test`, which
        // run after it, wait for it: none is served from the cache.
        assert_eq!(
            explain(),
            ["missing", "deferred", "deferred"],
            "a change to {input} kept a cached pass"
        );
        fs::write(&file, original).unwrap();
    }
    assert_eq!(explain(), ["available", "available", "available"]);
    // A new file that no task declares is in no key (and a new document is
    // never asked for: `a_commit_of_documents_keeps_the_contract_init_wrote`).
    write(&root, "docs/adr/0002.md", "# decided again\n");
    assert_eq!(explain(), ["available", "available", "available"]);
    assert_eq!(run(), ["cached", "cached", "cached"]);
    write(&root, "fixtures/cases/a.md", "# case a, edited\n");
    assert_eq!(run(), ["passed", "passed", "passed"]);
    fs::remove_dir_all(root.with_extension("bin")).unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// After `init`, a commit that only adds documents, licences or CI
/// configuration, anywhere, never makes the contract `init` wrote refuse:
/// the undeclared-sibling check exempts exactly what `init` leaves out.
#[test]
fn a_commit_of_documents_keeps_the_contract_init_wrote() {
    let root = crate_repository("docs-only");
    let (code, _, stderr) = sykli(&root, &["init"]);
    assert_eq!(code, Some(0), "{stderr}");
    let gate = temp_root("docs-only-gate").join("gate/sykli.json");
    let gate_path = gate.to_str().unwrap().to_string();
    let (code, _, stderr) = sykli(&root, &["init", "--contract", &gate_path]);
    assert_eq!(code, Some(0), "{stderr}");
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "contract"]);
    for (path, contents) in [
        ("docs/adr/0002.md", "# decided again\n"),
        ("docs/diagram.svg", "<svg/>\n"),
        (".github/workflows/release.yml", "on: release\n"),
        (".github/CODEOWNERS", "* owners\n"),
        ("src/NOTES.md", "notes beside the sources\n"),
        ("fixtures/case/README.md", "what the case is\n"),
        // Beside declared Markdown cases: never asked for, so a test that
        // reads the folder with `read_dir` keeps its pass until the contract
        // declares it (`init --force`), the gap the README names.
        ("fixtures/cases/c.md", "# case c\n"),
        ("vendor/dep/LICENSE-APACHE", "Apache\n"),
        ("vendor/dep/docs/usage.txt", "usage\n"),
    ] {
        write(&root, path, contents);
    }
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "documents only"]);
    for contract in ["sykli.json", gate_path.as_str()] {
        let (code, _, stderr) = sykli(&root, &["validate", contract]);
        assert_eq!(code, Some(0), "validate {contract}: {stderr}");
        let (code, stdout, stderr) = sykli(&root, &["plan", contract, "--explain", "--json"]);
        assert_eq!(code, Some(0), "plan {contract}: {stderr}");
        let plan: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert!(
            plan["input_coverage"]["folder_missing_inputs"].is_null(),
            "{plan}"
        );
        assert_eq!(
            plan["input_coverage"]["cargo_missing_inputs"],
            serde_json::json!([])
        );
    }
    #[cfg(unix)]
    {
        let path = toolchain(&root);
        // Both contracts declare the same tasks, so the second reuses the
        // first's passes.
        for (contract, outcome) in [("sykli.json", "passed"), (gate_path.as_str(), "cached")] {
            assert_eq!(
                statuses(&root, &path, &["run", contract, "--json"]),
                [outcome, outcome, outcome],
                "run {contract}"
            );
        }
        fs::remove_dir_all(root.with_extension("bin")).unwrap();
    }
    fs::remove_dir_all(gate.parent().unwrap().parent().unwrap()).unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// `sykli ARGS` in `root` with `path` as `PATH`: its exit code and stderr.
#[cfg(unix)]
fn sykli_on(root: &Path, path: &str, args: &[&str]) -> (Option<i32>, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(args)
        .env("PATH", path)
        .current_dir(root)
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A run's receipt kept as `.sykli/NAME`, and its outcome.
#[cfg(unix)]
fn kept_receipt(root: &Path, path: &str, contract: &str, name: &str) -> String {
    let (_, stdout, stderr) = sykli_on(root, path, &["run", contract, "--json"]);
    fs::write(root.join(".sykli").join(name), &stdout).unwrap();
    let receipt: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|_| panic!("{stderr}"));
    receipt["outcome"].as_str().unwrap().to_string()
}

/// `verify .sykli/NAME --contract CONTRACT --commit HEAD`: its exit code.
#[cfg(unix)]
fn verified(root: &Path, contract: &str, name: &str) -> Option<i32> {
    let receipt = format!(".sykli/{name}");
    sykli(
        root,
        &[
            "verify",
            &receipt,
            "--contract",
            contract,
            "--commit",
            "HEAD",
        ],
    )
    .0
}

/// A repository with `files` and `links` (each `(link, target)`) committed,
/// `contract` (when given) written and locked first.
#[cfg(unix)]
fn linked_repository(
    tag: &str,
    files: &[(&str, &str)],
    links: &[(&str, &str)],
    contract: Option<&str>,
) -> PathBuf {
    let root = temp_root(tag);
    for (path, contents) in files {
        write(&root, path, contents);
    }
    for (link, target) in links {
        fs::create_dir_all(root.join(link).parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target, root.join(link)).unwrap();
    }
    fs::write(root.join(".gitignore"), ".sykli/\n").unwrap();
    fs::create_dir_all(root.join(".sykli")).unwrap();
    if let Some(contract) = contract {
        fs::write(root.join("sykli.json"), contract).unwrap();
        let (code, _, stderr) = sykli(&root, &["lock", "sykli.json"]);
        assert_eq!(code, Some(0), "{stderr}");
    }
    git(&root, &["init", "--quiet"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "links"]);
    root
}

/// A link to a directory in the repository, or to nothing, is declared by
/// its own text, as git stores it: pointing it elsewhere misses the cache.
/// The files under the directory are declared too, so editing one misses
/// it as well. A link to a file is read through, as before. The contract
/// `init` writes validates, runs and verifies against its commit.
#[cfg(unix)]
#[test]
fn init_declares_links_to_a_directory_or_to_nothing_by_their_text() {
    let root = linked_repository(
        "links",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
            ),
            ("src/main.rs", "fn main() {}\n"),
            ("assets/v1/logo.txt", "one\n"),
            ("assets/v2/logo.txt", "two\n"),
            ("assets/notes.txt", "beside the link\n"),
        ],
        &[
            ("assets/current", "v1"),
            ("tests/shared", "../assets"),
            ("tests/dangling.txt", "missing.txt"),
            ("src/logo.txt", "../assets/v1/logo.txt"),
        ],
        None,
    );
    let (code, _, stderr) = sykli(&root, &["init"]);
    assert_eq!(code, Some(0), "{stderr}");
    let declared = inputs(task(&contract(&root), "test"));
    for input in [
        "assets/current",
        "assets/v1/logo.txt",
        "assets/v2/logo.txt",
        "tests/shared",
        "tests/dangling.txt",
        "src/logo.txt",
    ] {
        assert!(
            declared.contains(&input.to_string()),
            "{input}: {declared:?}"
        );
    }
    let (code, _, stderr) = sykli(&root, &["validate", "sykli.json"]);
    assert_eq!(code, Some(0), "{stderr}");
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "contract"]);

    let path = toolchain(&root);
    assert_eq!(
        kept_receipt(&root, &path, "sykli.json", "first.json"),
        "passed"
    );
    assert_eq!(verified(&root, "sykli.json", "first.json"), Some(0));
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join(".sykli/first.json")).unwrap()).unwrap();
    let records = &receipt["tasks"][0]["inputs"];
    for (input, link) in [
        ("assets/current", true),
        ("tests/dangling.txt", true),
        ("src/logo.txt", false),
    ] {
        assert_eq!(records[input]["link"] == true, link, "{input}: {records}");
    }

    let explain = || statuses(&root, &path, &["plan", "--explain", "--json"]);
    assert_eq!(explain(), ["available", "available", "available"]);
    for (link, target) in [
        ("assets/current", "v2"),
        ("tests/shared", "../assets/v1"),
        ("tests/dangling.txt", "gone.txt"),
    ] {
        let file = root.join(link);
        let before = fs::read_link(&file).unwrap();
        fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink(target, &file).unwrap();
        assert_eq!(
            explain(),
            ["missing", "deferred", "deferred"],
            "pointing {link} to {target} kept a cached pass"
        );
        fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink(before, &file).unwrap();
    }
    write(&root, "assets/v1/logo.txt", "one, edited\n");
    assert_eq!(explain(), ["missing", "deferred", "deferred"]);
    write(&root, "assets/v1/logo.txt", "one\n");

    fs::remove_file(root.join("assets/current")).unwrap();
    std::os::unix::fs::symlink("v2", root.join("assets/current")).unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "current is v2"]);
    assert_eq!(verified(&root, "sykli.json", "first.json"), Some(3));
    assert_eq!(
        kept_receipt(&root, &path, "sykli.json", "second.json"),
        "passed"
    );
    assert_eq!(verified(&root, "sykli.json", "second.json"), Some(0));
    fs::remove_dir_all(root.with_extension("bin")).unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// A link to a directory is declared by its text, so the files a run reads
/// through it are in no key unless the task declares them: a contract that
/// declares the link without every tracked file under the directory is
/// refused, naming them, by `validate` and `run`. Declared, an edit to one
/// misses the cache.
#[cfg(unix)]
#[test]
fn a_link_to_a_directory_requires_the_files_under_it() {
    let link_only = r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"grep -q one current/x.txt","inputs":["current"]}]}"#;
    let root = linked_repository(
        "directory-link",
        &[("v1/x.txt", "one\n")],
        &[("current", "v1")],
        Some(link_only),
    );
    let path = env::var("PATH").unwrap();
    let names = |text: &str| text.contains("v1/x.txt (task t, through current)");
    let (code, _, stderr) = sykli(&root, &["validate", "sykli.json"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(names(&stderr), "{stderr}");
    let (code, _, stderr) = sykli_on(&root, &path, &["run", "sykli.json"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(names(&stderr), "{stderr}");

    fs::write(
        root.join("sykli.json"),
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"grep -q one current/x.txt","inputs":["current","v1/x.txt"]}]}"#,
    )
    .unwrap();
    let (code, _, stderr) = sykli(&root, &["lock", "sykli.json"]);
    assert_eq!(code, Some(0), "{stderr}");
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "declared"]);
    let (code, _, stderr) = sykli(&root, &["validate", "sykli.json"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        kept_receipt(&root, &path, "sykli.json", "first.json"),
        "passed"
    );
    assert_eq!(verified(&root, "sykli.json", "first.json"), Some(0));
    write(&root, "v1/x.txt", "two\n");
    assert_eq!(
        kept_receipt(&root, &path, "sykli.json", "second.json"),
        "failed"
    );
    fs::remove_dir_all(root).unwrap();
}

/// A link that leads out of the repository, to a file, a directory or
/// nothing there, by an absolute path or by climbing out, leads where no
/// commit can show, so its receipts could never verify: `init` leaves it
/// out with a note, the rules that ask for files under `tests/` or beside
/// declared ones do not ask for it, and a contract that declares it is
/// refused by `validate` and `run` before any task (on main, `run` passed a
/// link to an outside file and `verify --commit` refused every receipt).
#[cfg(unix)]
#[test]
fn a_link_out_of_the_repository_cannot_be_declared() {
    let outside = temp_root("outside");
    fs::write(outside.join("o.txt"), "ext\n").unwrap();
    let name = outside.file_name().unwrap().to_str().unwrap().to_string();
    let absolute_missing = outside.join("missing.txt");
    let links = [
        ("tests/ext", outside.to_str().unwrap().to_string()),
        (
            "tests/ext.txt",
            outside.join("o.txt").to_str().unwrap().to_string(),
        ),
        ("tests/dabs", absolute_missing.to_str().unwrap().to_string()),
        ("tests/up.txt", format!("../../{name}/o.txt")),
        ("tests/up-missing", format!("../../{name}/gone.txt")),
    ];
    let root = linked_repository(
        "outside-links",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
            ),
            ("src/main.rs", "fn main() {}\n"),
            ("tests/it.rs", "#[test]\nfn it() {}\n"),
        ],
        &links
            .iter()
            .map(|(link, target)| (*link, target.as_str()))
            .collect::<Vec<_>>(),
        None,
    );
    let (code, _, stderr) = sykli(&root, &["init"]);
    assert_eq!(code, Some(0), "{stderr}");
    let written = contract(&root);
    let declared = inputs(task(&written, "test"));
    for (link, _) in &links {
        assert!(
            !declared.contains(&link.to_string()),
            "{link}: {declared:?}"
        );
        assert!(
            stderr.contains(&format!("{link} is a symbolic link out of the repository")),
            "{stderr}"
        );
    }
    let (code, _, stderr) = sykli(&root, &["validate", "sykli.json"]);
    assert_eq!(code, Some(0), "{stderr}");

    for (link, _) in &links {
        let mut edited = written.clone();
        for task in edited["tasks"].as_array_mut().unwrap() {
            task["inputs"].as_array_mut().unwrap().push((*link).into());
        }
        fs::write(
            root.join("sykli.json"),
            serde_json::to_vec_pretty(&edited).unwrap(),
        )
        .unwrap();
        let (code, _, stderr) = sykli(&root, &["lock", "sykli.json"]);
        assert_eq!(code, Some(0), "{stderr}");
        let refused = |text: &str| {
            text.contains(&format!("{link}, a symbolic link to"))
                && text.contains("outside the repository")
        };
        let (code, _, stderr) = sykli(&root, &["validate", "sykli.json"]);
        assert_eq!(code, Some(2), "validate with {link}: {stderr}");
        assert!(refused(&stderr), "{stderr}");
        let (code, _, stderr) = sykli(&root, &["run", "sykli.json"]);
        assert_eq!(code, Some(2), "run with {link}: {stderr}");
        assert!(refused(&stderr), "{stderr}");
        assert!(!root.join(".sykli/receipts").exists(), "no task ran");
    }
    fs::remove_dir_all(outside).unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// sykli#54: a declared path that is relative and has no `..` can still
/// leave the repository through a symbolic link on its way. With `ext`
/// linked to a directory outside, the input `ext/file.txt` validated, was
/// hashed into the key and the receipt from outside, and the run passed; an
/// output `ext/bundle.js` or a workdir `ext` was written outside. Each is
/// refused by `validate`, `plan --explain` and `run` (exit 2) before any task
/// runs, naming the path; a link on the way that stays inside the repository
/// is not.
#[cfg(unix)]
#[test]
fn a_declared_path_through_a_link_out_of_the_repository_is_refused() {
    let outside = temp_root("through");
    fs::write(outside.join("file.txt"), "outside\n").unwrap();
    let root = linked_repository(
        "through-links",
        &[("file.txt", "one\n"), ("inside/real/file.txt", "inside\n")],
        &[("ext", outside.to_str().unwrap()), ("alias", "inside/real")],
        None,
    );
    let refusals = [
        (
            "inputs",
            serde_json::json!(["file.txt", "ext/file.txt"]),
            "ext/file.txt",
        ),
        (
            "inputs",
            serde_json::json!(["file.txt", "ext/new/gone.txt"]),
            "ext/new/gone.txt",
        ),
        (
            "outputs",
            serde_json::json!(["ext/bundle.js"]),
            "ext/bundle.js",
        ),
        ("workdir", serde_json::json!("ext"), "\"ext\""),
    ];
    for (field, value, named) in refusals {
        let mut task = serde_json::json!({
            "name": "t",
            "run": "echo built > bundle.js; touch ran",
            "inputs": ["file.txt"],
        });
        task[field] = value;
        fs::write(
            root.join("sykli.json"),
            serde_json::json!({"schema": "sykli-contract.v1", "tasks": [task]}).to_string(),
        )
        .unwrap();
        let (code, _, stderr) = sykli(&root, &["lock", "sykli.json"]);
        assert_eq!(code, Some(0), "{stderr}");
        let refused = |text: &str| text.contains(named) && text.contains("outside the repository");
        let (code, _, stderr) = sykli(&root, &["validate", "sykli.json"]);
        assert_eq!(code, Some(2), "validate, {field} {named}: {stderr}");
        assert!(refused(&stderr), "{stderr}");
        let (code, stdout, _) = sykli(&root, &["plan", "--explain", "--json"]);
        assert_eq!(code, Some(2), "plan, {field} {named}");
        assert!(refused(&stdout), "{stdout}");
        let (code, _, stderr) = sykli(&root, &["run", "sykli.json"]);
        assert_eq!(code, Some(2), "run, {field} {named}: {stderr}");
        assert!(refused(&stderr), "{stderr}");
        assert!(!root.join(".sykli/receipts").exists(), "no task ran");
        assert!(!outside.join("ran").exists() && !outside.join("bundle.js").exists());
    }

    // A link on the way that stays in the repository is not refused.
    fs::write(
        root.join("sykli.json"),
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"cat alias/file.txt","inputs":["alias/file.txt"]}]}"#,
    )
    .unwrap();
    let (code, _, stderr) = sykli(&root, &["lock", "sykli.json"]);
    assert_eq!(code, Some(0), "{stderr}");
    let (code, _, stderr) = sykli(&root, &["validate", "sykli.json"]);
    assert_eq!(code, Some(0), "{stderr}");
    fs::remove_dir_all(outside).unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// A document a string literal in a source names, which no task declares,
/// warns with where it is named, and `validate`, `plan --explain` and `run`
/// go on: a help text that says "see AGENTS.md" reads nothing. A comment or
/// a doc comment that mentions a document says nothing at all, and a path
/// that ends in a document's name does not name the top-level one. An
/// include of an undeclared file still refuses.
#[test]
fn a_document_a_string_names_warns_and_an_include_still_refuses() {
    let root = temp_root("mentions");
    for (path, contents) in [
        (
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
        ),
        (
            "src/lib.rs",
            "//! How to work here is in AGENTS.md.\n/// See README.md and docs/setup.md.\npub fn help() {\n    // CLAUDE.md says so too.\n    println!(\"see AGENTS.md\");\n    let _ = \"home/.claude/CLAUDE.md\";\n}\n",
        ),
        (
            "tests/readme.rs",
            "#[test]\nfn readme() {\n    std::fs::read(\"README.md\").unwrap();\n}\n",
        ),
        ("AGENTS.md", "# agents\n"),
        ("CLAUDE.md", "# claude\n"),
        ("README.md", "# x\n"),
        ("docs/setup.md", "# setup\n"),
    ] {
        write(&root, path, contents);
    }
    write(
        &root,
        "sykli.json",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"test","run":"true","inputs":["Cargo.toml","src/lib.rs","tests/readme.rs"]}]}"#,
    );
    fs::write(root.join(".gitignore"), ".sykli/\n").unwrap();
    git(&root, &["init", "--quiet"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "mentions"]);
    let warns = |text: &str| {
        let named = |path: &str, at: &str| {
            text.contains(&format!(
                "warning: {path} is named in {at} but not declared; declare it if a task reads it"
            ))
        };
        named("AGENTS.md", "src/lib.rs:5")
            && named("README.md", "tests/readme.rs:3")
            && !text.contains("CLAUDE.md")
            && !text.contains("docs/setup.md")
    };
    let (code, _, stderr) = sykli(&root, &["validate", "sykli.json"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(warns(&stderr), "{stderr}");
    let (code, stdout, stderr) = sykli(&root, &["plan", "--explain", "--json"]);
    assert_eq!(code, Some(0), "{stderr}");
    let plan: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        plan["input_coverage"]["named_inputs"],
        serde_json::json!([
            {"path": "AGENTS.md", "named_at": "src/lib.rs:5"},
            {"path": "README.md", "named_at": "tests/readme.rs:3"},
        ])
    );
    let (code, stdout, _) = sykli(&root, &["plan", "--explain"]);
    assert_eq!(code, Some(0));
    assert!(warns(&stdout), "{stdout}");
    let (code, _, stderr) = sykli(&root, &["run", "sykli.json"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(warns(&stderr), "{stderr}");

    // An include of a file no task declares still refuses.
    write(
        &root,
        "src/lib.rs",
        "pub const TEXT: &str = include_str!(\"text.txt\");\n",
    );
    write(&root, "src/text.txt", "text\n");
    let (code, _, stderr) = sykli(&root, &["validate", "sykli.json"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.replace("\\\\", "/").contains("src/text.txt"),
        "{stderr}"
    );
    let (code, _, stderr) = sykli(&root, &["run", "sykli.json"]);
    assert_eq!(code, Some(2), "{stderr}");
    fs::remove_dir_all(root).unwrap();
}

/// An include whose path is not a string literal cannot be required, so it
/// is refused, never passed over: with its file and line, by `init`,
/// `validate` and `run` alike. A path generated at build time
/// (`OUT_DIR`) is not in git and is not refused.
#[test]
fn an_include_whose_path_is_not_a_literal_is_refused_with_its_file_and_line() {
    let root = temp_root("opaque");
    write(
        &root,
        "Cargo.toml",
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    );
    write(
        &root,
        "src/lib.rs",
        "include!(concat!(env!(\"OUT_DIR\"), \"/generated.rs\"));\nmacro_rules! text {\n    ($p:literal) => {\n        include_str!($p)\n    };\n}\npub const A: &str = text!(\"a.txt\");\n",
    );
    write(&root, "src/a.txt", "a\n");
    git(&root, &["init", "--quiet"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "opaque"]);
    let names = |text: &str| {
        let text = text.replace('\\', "/");
        text.contains("src/lib.rs:4") && text.contains("not a string literal")
    };
    let (code, _, stderr) = sykli(&root, &["init"]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(names(&stderr), "{stderr}");
    assert!(!root.join("sykli.json").exists());
    write(
        &root,
        "sykli.json",
        r#"{"schema":"sykli-contract.v1","tasks":[{"name":"test","run":"cargo test","inputs":["Cargo.toml","src/a.txt","src/lib.rs"]}]}"#,
    );
    let (code, _, stderr) = sykli(&root, &["validate", "sykli.json"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(names(&stderr), "{stderr}");
    let (code, _, stderr) = sykli(&root, &["run", "sykli.json"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(names(&stderr), "{stderr}");
    fs::remove_dir_all(root).unwrap();
}

/// A submodule is a directory in git's index, never a file a task can
/// declare. Beside a file init declares, neither the undeclared-sibling
/// check nor the Cargo rule may require it, or init's contract is refused.
#[test]
fn a_submodule_beside_declared_files_is_not_required() {
    let root = temp_root("submodule");
    write(
        &root,
        "Cargo.toml",
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    );
    write(&root, "src/main.rs", "fn main() {}\n");
    write(&root, "vendor/notes.txt", "vendored by hand\n");
    write(&root, "tests/data/case.txt", "case\n");
    for sub in ["vendor/sub", "tests/data/sub"] {
        write(&root, &format!("{sub}/file.txt"), "inside\n");
        git(&root.join(sub), &["init", "--quiet"]);
        git(&root.join(sub), &["add", "."]);
        git(&root.join(sub), &["commit", "--quiet", "-m", "sub"]);
    }
    git(&root, &["init", "--quiet"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "with submodules"]);
    let (code, _, stderr) = sykli(&root, &["init"]);
    assert_eq!(code, Some(0), "{stderr}");
    let declared = inputs(task(&contract(&root), "test"));
    assert!(
        declared.contains(&"vendor/notes.txt".to_string()),
        "{declared:?}"
    );
    assert!(
        !declared.iter().any(|input| input.ends_with("sub")),
        "{declared:?}"
    );
    let (code, _, stderr) = sykli(&root, &["validate", "sykli.json"]);
    assert_eq!(code, Some(0), "{stderr}");
    fs::remove_dir_all(root).unwrap();
}
