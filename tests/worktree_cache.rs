//! `${worktree_cache}`: a directory of each worktree's own, outside it.
//!
//! With one Cargo target directory for every worktree of a clone (`[build]
//! target-dir` in Cargo's own configuration, as many machines set it), the
//! final artifacts land in shared `debug/<name>` paths: a run in one
//! worktree can run a binary another worktree built, and its receipt then
//! claims a pass for code that never ran. These tests reproduce that with a
//! real Cargo build, then show that a contract declaring
//! `CARGO_TARGET_DIR=${worktree_cache}/cargo-target` runs each worktree's own
//! build; that the directory lies outside the work tree; what a cache key
//! covers; that an unknown placeholder is refused; and that a gone
//! worktree's directory is pruned, never while a run in it is live.
#![cfg(unix)]

use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NONCE: AtomicU64 = AtomicU64::new(0);

/// A clone: its main checkout `repo`, its worktrees beside it in `base`,
/// and what tasks write outside every tree, in `base` too.
struct Fixture {
    base: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "sykli-wtc-{name}-{}-{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("repo")).unwrap();
        // One spelling (macOS's /var is /private/var), as sykli reports paths.
        let base = fs::canonicalize(&base).unwrap();
        let fixture = Fixture {
            repo: base.join("repo"),
            base,
        };
        fixture.git(&fixture.repo, &["init", "--quiet"]);
        fixture.write(".gitignore", ".sykli/\n");
        fixture
    }

    fn write(&self, path: &str, content: &str) {
        let path = self.repo.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    /// `sykli.json` with `tasks`, given where tasks may write (`base`).
    fn contract(&self, tasks: impl Fn(&str) -> Value) {
        let tasks = tasks(self.base.to_str().unwrap());
        fs::write(
            self.repo.join("sykli.json"),
            serde_json::to_vec_pretty(&json!({"schema": "sykli-contract.v1", "tasks": tasks}))
                .unwrap(),
        )
        .unwrap();
    }

    /// Lock every contract named, and commit everything on `main`.
    fn commit_locked(&self, contracts: &[&str]) {
        for contract in contracts {
            let lock = self
                .sykli(&self.repo, &["lock", contract])
                .output()
                .unwrap();
            assert!(lock.status.success(), "{}", text(&lock.stderr));
        }
        self.git(&self.repo, &["add", "--all"]);
        self.git(
            &self.repo,
            &["commit", "--quiet", "--no-verify", "-m", "c1"],
        );
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = own_git_config(&mut Command::new("git"))
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", text(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn sykli(&self, dir: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sykli"));
        own_git_config(&mut command)
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null());
        command
    }

    /// A linked worktree `base/NAME` on a new branch NAME.
    fn worktree(&self, name: &str) -> PathBuf {
        let path = self.base.join(name);
        self.git(
            &self.repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                name,
                path.to_str().unwrap(),
            ],
        );
        path
    }

    /// `run --json` of `contract` in `dir`, which must exit `code`: its receipt.
    fn run(&self, dir: &Path, contract: &str, code: i32) -> Value {
        receipt(
            self.sykli(dir, &["run", contract, "--json"])
                .output()
                .unwrap(),
            code,
        )
    }

    /// Where every worktree's directory lives: `<common>/sykli/worktrees`.
    fn caches(&self) -> PathBuf {
        self.repo.join(".git/sykli/worktrees")
    }

    fn caches_left(&self) -> Vec<String> {
        let mut names = fs::read_dir(self.caches())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    /// What a task wrote to `base/NAME`, as a path.
    fn seen(&self, name: &str) -> PathBuf {
        PathBuf::from(fs::read_to_string(self.base.join(name)).unwrap())
    }

    /// `git status` of a worktree, untracked files included.
    fn status(&self, dir: &Path) -> String {
        self.git(dir, &["status", "--porcelain", "--untracked-files=all"])
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Nothing a test started may stay held at its latch.
        let latch = self.base.join("latch");
        if latch.exists() {
            std::thread::spawn(move || {
                let _ = fs::write(latch, "go\n");
            });
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// Git settings of the test's own, never the machine's: no global excludes
/// file, and `main` for a new repository.
fn own_git_config(command: &mut Command) -> &mut Command {
    command
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "core.excludesFile")
        .env("GIT_CONFIG_VALUE_0", "/dev/null")
        .env("GIT_CONFIG_KEY_1", "init.defaultBranch")
        .env("GIT_CONFIG_VALUE_1", "main")
}

fn receipt(out: Output, code: i32) -> Value {
    assert_eq!(
        out.status.code(),
        Some(code),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("no receipt: {}", text(&out.stdout)))
}

fn outcome(receipt: &Value) -> &str {
    receipt["tasks"][0]["outcome"].as_str().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn wait_until(what: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while !predicate() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn mkfifo(path: &Path) {
    let _ = fs::remove_file(path);
    assert!(Command::new("mkfifo").arg(path).status().unwrap().success());
}

/// Let a task held at the FIFO `path` go on.
fn release(path: PathBuf) {
    // Opening a FIFO for writing waits for its reader; do it aside.
    std::thread::spawn(move || {
        let _ = fs::write(path, "go\n");
    });
}

/// The directory a worktree's tasks get: `<caches>/<64 hex>/cache`, with
/// sykli's own marker and lease beside it, and outside the work tree `tree`
/// (in its `.git` for the main checkout, whose git directory is the common
/// one; elsewhere for a linked worktree). Its `<64 hex>/`.
fn assert_cache_of(fixture: &Fixture, dir: &Path, tree: &Path) -> PathBuf {
    assert_eq!(dir.file_name().unwrap(), "cache", "{}", dir.display());
    let own = dir.parent().unwrap();
    assert_eq!(
        own.parent(),
        Some(fixture.caches().as_path()),
        "{} is not a worktree's directory",
        dir.display()
    );
    let id = own.file_name().unwrap().to_str().unwrap();
    assert!(
        id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "{id}"
    );
    for sykli in ["worktree.json", "lease"] {
        assert!(
            own.join(sykli).is_file(),
            "{} has no {sykli}",
            own.display()
        );
    }
    assert!(
        !dir.starts_with(tree) || dir.starts_with(tree.join(".git")),
        "{} is inside the work tree {}",
        dir.display(),
        tree.display()
    );
    own.to_path_buf()
}

/// `command`'s output, or a failure once it has run for five minutes (it is
/// killed): a test that waits on Cargo never hangs the suite.
fn bounded(mut command: Command, what: &str) -> Output {
    let child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id().to_string();
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(child.wait_with_output());
    });
    match receive.recv_timeout(Duration::from_secs(300)) {
        Ok(out) => out.unwrap(),
        Err(_) => {
            let _ = Command::new("kill").args(["-KILL", &pid]).status();
            panic!("{what} did not end within 300 s");
        }
    }
}

/// Kill the process group `group` (a task's shell leads its own) and wait
/// until it is gone, so the leases its processes held are released.
fn kill_group(group: &str) {
    let group = format!("-{}", group.trim());
    assert!(
        Command::new("kill")
            .args(["-KILL", "--", &group])
            .status()
            .unwrap()
            .success()
    );
    wait_until("the group is gone", || {
        !Command::new("kill")
            .args(["-0", "--", &group])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    });
}

/// The command a task runs to note the directory it got as `base/dir-NAME`,
/// NAME being its worktree's directory name.
fn note_dir(b: &str) -> String {
    format!("printf %s \"$DIR\" > '{b}/dir-'\"$(basename \"$(pwd)\")\"")
}

/// A placeholder sykli does not know is refused where the contract is
/// read, before anything runs, never handed to a task literally. A `$`
/// without braces is not a placeholder and still reaches the task as
/// written.
#[test]
fn an_unknown_or_unclosed_placeholder_is_refused_before_anything_runs() {
    for (value, says) in [
        ("${worktree_dir}/x", "${worktree_dir}"),
        ("${WORKTREE_CACHE}", "${WORKTREE_CACHE}"),
        ("a${}b", "${}"),
        ("${worktree_cache", "unclosed"),
    ] {
        let fixture = Fixture::new("refused");
        fixture.write("file.txt", "one\n");
        fixture.contract(|b| {
            json!([{
                "name": "t",
                "run": format!("touch '{b}/ran'"),
                "inputs": ["file.txt"],
                "env": {"X": value},
            }])
        });
        let validate = fixture
            .sykli(&fixture.repo, &["validate", "sykli.json"])
            .output()
            .unwrap();
        assert_eq!(validate.status.code(), Some(1), "{value}");
        let said = text(&validate.stderr);
        assert!(said.contains(says), "{value}: {said}");
        assert!(said.contains("${worktree_cache}"), "{value}: {said}");
        for args in [
            &["lock", "sykli.json"][..],
            &["run", "sykli.json", "--json"],
        ] {
            let out = fixture.sykli(&fixture.repo, args).output().unwrap();
            assert_eq!(out.status.code(), Some(2), "{args:?} {value}");
            assert!(text(&out.stderr).contains(says), "{}", text(&out.stderr));
        }
        assert!(!fixture.base.join("ran").exists(), "{value}: a task ran");
        assert!(!fixture.repo.join(".git/sykli/runs").exists(), "{value}");
    }

    // `$NAME` is no placeholder: the value reaches the task as written.
    let fixture = Fixture::new("literal");
    for (command, says) in [
        ("run", "${worktree_cache}/cargo-target"),
        ("validate", "${worktree_cache}"),
    ] {
        let help = fixture
            .sykli(&fixture.repo, &[command, "--help"])
            .output()
            .unwrap();
        assert!(text(&help.stdout).contains(says), "{command} --help");
    }
    fixture.write("file.txt", "one\n");
    fixture.contract(|_| {
        json!([{
            "name": "t",
            "run": "test \"$X\" = '$HOME/$x and $'",
            "inputs": ["file.txt"],
            "env": {"X": "$HOME/$x and $"},
        }])
    });
    fixture.commit_locked(&["sykli.json"]);
    let ran = fixture.run(&fixture.repo, "sykli.json", 0);
    assert_eq!(outcome(&ran), "passed", "{ran}");
}

/// Each worktree of a clone gets its own directory, in the common git
/// directory, made when a run needs it: the same one on every run, another
/// for every other worktree, and never inside a work tree, so it shows in
/// no `git status` and in no tree a receipt binds. The directory is the
/// tasks' own: one that removes it (as `cargo clean` empties a target
/// directory) takes nothing of sykli's, and gets it back on its next run.
/// Sykli records the directory nowhere: no receipt and no journal names it.
#[test]
fn each_worktree_has_its_own_directory_outside_its_tree() {
    let fixture = Fixture::new("where");
    fixture.write("file.txt", "one\n");
    fixture.contract(|b| {
        json!([{
            "name": "t",
            "run": format!("test -d \"$DIR\" && {}", note_dir(b)),
            "inputs": ["file.txt"],
            "env": {"DIR": "${worktree_cache}"},
        }])
    });
    fixture.commit_locked(&["sykli.json"]);
    assert!(!fixture.caches().exists(), "made before any run needed it");
    let a = fixture.worktree("a");
    let b = fixture.worktree("b");
    let trees = [&fixture.repo, &a, &b];
    for tree in trees {
        let ran = fixture.run(tree, "sykli.json", 0);
        assert_eq!(outcome(&ran), "passed", "{ran}");
    }
    let dirs = ["repo", "a", "b"].map(|name| fixture.seen(&format!("dir-{name}")));
    let mut ids = Vec::new();
    for (dir, tree) in dirs.iter().zip(trees) {
        let own = assert_cache_of(&fixture, dir, tree);
        ids.push(own.file_name().unwrap().to_str().unwrap().to_owned());
        assert_eq!(fixture.status(tree), "", "{}", tree.display());
    }
    assert_ne!(dirs[0], dirs[1]);
    assert_ne!(dirs[1], dirs[2]);
    assert_ne!(dirs[0], dirs[2]);
    assert_eq!(fixture.caches_left().len(), 3);

    // A task removed it; the same worktree gets the same directory back.
    fs::remove_dir_all(&dirs[1]).unwrap();
    fs::write(a.join("file.txt"), "two\n").unwrap();
    let ran = fixture.run(&a, "sykli.json", 0);
    assert_eq!(outcome(&ran), "passed", "{ran}");
    assert_eq!(fixture.seen("dir-a"), dirs[1]);
    assert_cache_of(&fixture, &dirs[1], &a);

    // Not recorded: neither a receipt nor a run's journal names it.
    let mut recorded = Vec::new();
    let mut walk = vec![a.join(".sykli"), fixture.repo.join(".git/sykli/runs")];
    while let Some(path) = walk.pop() {
        if path.is_dir() {
            walk.extend(fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
        } else if let Ok(bytes) = fs::read(&path) {
            recorded.push((path, text(&bytes)));
        }
    }
    assert!(recorded.len() > 2);
    for (path, content) in &recorded {
        for id in &ids {
            assert!(
                !content.contains(id.as_str()),
                "{} names {id}",
                path.display()
            );
        }
    }
}

/// What a cache key covers is the declaration, `${worktree_cache}` as
/// written, never the directory it names. Identical work in two worktrees
/// has one key, although each gets its own directory; a worktree that
/// moves gets another directory and keeps its passes (its old directory is
/// pruned); and changing what is declared changes the key, so a pass earned
/// under one declaration is never reused for another.
#[test]
fn the_cache_key_covers_the_declaration_not_the_directory() {
    let fixture = Fixture::new("key");
    fixture.write("file.txt", "one\n");
    fixture.contract(|b| {
        json!([{
            "name": "t",
            "run": note_dir(b),
            "inputs": ["file.txt"],
            "env": {"DIR": "${worktree_cache}"},
        }])
    });
    // The same task under another name: it has never run, so it runs.
    let probe = fs::read_to_string(fixture.repo.join("sykli.json"))
        .unwrap()
        .replace("dir-", "probe-");
    fs::write(fixture.repo.join("probe.json"), probe).unwrap();
    fixture.commit_locked(&["sykli.json", "probe.json"]);
    let a = fixture.worktree("a");
    let b = fixture.worktree("b");
    for tree in [&a, &b] {
        let ran = fixture.run(tree, "sykli.json", 0);
        assert_eq!(outcome(&ran), "passed", "{ran}");
    }
    let keys = |tree: &Path| {
        fs::read_dir(tree.join(".sykli/cache"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| !name.starts_with('.'))
            .collect::<Vec<_>>()
    };
    assert_eq!(keys(&a).len(), 1);
    assert_eq!(keys(&a), keys(&b), "identical work, two keys");
    let in_a = fixture.seen("dir-a");
    assert_ne!(in_a, fixture.seen("dir-b"));

    // Moved, the worktree is another worktree to Git's files: another
    // directory. Its cached pass still counts, and its old directory goes.
    let moved = fixture.base.join("moved");
    fixture.git(
        &fixture.repo,
        &[
            "worktree",
            "move",
            a.to_str().unwrap(),
            moved.to_str().unwrap(),
        ],
    );
    let ran = fixture.run(&moved, "sykli.json", 0);
    assert_eq!(outcome(&ran), "cached", "{ran}");
    assert!(
        !in_a.exists(),
        "the moved worktree's old directory was kept"
    );
    let ran = fixture.run(&moved, "probe.json", 0);
    assert_eq!(outcome(&ran), "passed", "{ran}");
    let now = fixture.seen("probe-moved");
    assert_cache_of(&fixture, &now, &moved);
    assert_ne!(now, in_a);

    // Another declaration is another key: no pass is reused across it.
    let edited = fs::read_to_string(b.join("sykli.json"))
        .unwrap()
        .replace("${worktree_cache}", "${worktree_cache}/elsewhere");
    fs::write(b.join("sykli.json"), edited).unwrap();
    let lock = fixture.sykli(&b, &["lock", "sykli.json"]).output().unwrap();
    assert!(lock.status.success(), "{}", text(&lock.stderr));
    let ran = fixture.run(&b, "sykli.json", 0);
    assert_eq!(outcome(&ran), "passed", "{ran}");
    assert_eq!(keys(&b).len(), 2);
}

/// What one arm of the hazard observed.
#[derive(Debug)]
struct Observed {
    /// Each worktree's task outcome, `a` and `b`.
    outcomes: [String; 2],
    /// The `CARGO_TARGET_DIR` each task got; empty when none was declared.
    targets: [String; 2],
}

/// `a`'s code: its binary exits 1. `b`'s exits 0.
const BROKEN: &str = "fn main() {\n    println!(\"a\");\n    std::process::exit(1);\n}\n";
const WORKING: &str = "fn main() {\n    println!(\"b\");\n}\n";

/// Two worktrees, `a` and `b`, of one clone build the same binary, `probe`,
/// from different code, under a Cargo whose own configuration puts every
/// build in one target directory. Each worktree's gate runs `cargo test`,
/// whose test runs the binary at its uplifted path (`CARGO_BIN_EXE_probe`,
/// which is `<target>/debug/probe`), as integration tests do. An agent
/// works in `b` with its own `cargo build`, which keeps Cargo's
/// configuration (isolating gates leaves a person's or an agent's own
/// commands as they are): once before `a`'s gate, and once more while `a`'s
/// gate is held after its build, at its test. Then `a`'s test goes on, and
/// `b`'s gate runs.
/// `target` is what the contract declares as `CARGO_TARGET_DIR`, if
/// anything.
///
/// Either way Cargo shares a target directory, `a` runs `b`'s binary: a
/// Cargo that hashes a package by its path relative to the workspace finds
/// `a`'s build fresh after `b`'s (its sources are older) and builds nothing,
/// and every Cargo puts `b`'s second build at the same `debug/probe`.
fn hazard(fixture: &Fixture, target: Option<&str>) -> Observed {
    let cargo = Path::new(env!("CARGO"));
    let toolchain = cargo.parent().unwrap();
    assert!(
        toolchain.join("rustc").exists(),
        "no rustc beside {cargo:?}"
    );
    let base = fixture.base.to_str().unwrap();
    // Cargo's own configuration, as `$CARGO_HOME/config.toml` sets it on a
    // machine that shares one target directory.
    let home = fixture.base.join("home");
    fs::create_dir_all(home.join(".cargo")).unwrap();
    fs::write(
        home.join(".cargo/config.toml"),
        format!("[build]\ntarget-dir = \"{base}/shared-target\"\n"),
    )
    .unwrap();
    let environment = [
        ("HOME", home.to_str().unwrap().to_owned()),
        ("PATH", format!("{}:/usr/bin:/bin", toolchain.display())),
    ];

    fixture.write(
        "Cargo.toml",
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    fixture.write("src/main.rs", BROKEN);
    fixture.write(
        "tests/smoke.rs",
        &format!(
            "const BASE: &str = {base:?};\n\n\
             #[test]\n\
             fn the_probe_succeeds() {{\n    \
                 let here = std::env::current_dir().unwrap();\n    \
                 let name = here.file_name().unwrap().to_string_lossy().into_owned();\n    \
                 let base = std::path::Path::new(BASE);\n    \
                 if base.join(format!(\"hold-{{name}}\")).exists() {{\n        \
                     std::fs::write(base.join(format!(\"{{name}}.ready\")), \"\").unwrap();\n        \
                     let _ = std::fs::read(base.join(\"latch\"));\n    \
                 }}\n    \
                 let status = std::process::Command::new(env!(\"CARGO_BIN_EXE_probe\"))\n        \
                     .status()\n        \
                     .unwrap();\n    \
                 assert!(status.success(), \"probe: {{status}}\");\n\
             }}\n"
        ),
    );
    let lockfile = Command::new(cargo)
        .args(["generate-lockfile", "--offline"])
        .current_dir(&fixture.repo)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(lockfile.status.success(), "{}", text(&lockfile.stderr));
    fixture.contract(|b| {
        let mut task = json!({
            "name": "test",
            "run": format!(
                "printf %s \"${{CARGO_TARGET_DIR-}}\" > '{b}/target-'\"$(basename \"$(pwd)\")\" \
                 && cargo test --offline --quiet"
            ),
            "inputs": ["Cargo.toml", "Cargo.lock", "src/main.rs", "tests/smoke.rs"],
        });
        if let Some(target) = target {
            task["env"] = json!({"CARGO_TARGET_DIR": target});
        }
        json!([task])
    });
    fixture.commit_locked(&["sykli.json"]);
    let a = fixture.worktree("a");
    let b = fixture.worktree("b");
    fs::write(b.join("src/main.rs"), WORKING).unwrap();
    fixture.git(&b, &["commit", "--quiet", "--no-verify", "-am", "b works"]);
    // The agent in `b`, with its own cargo: Cargo's configuration decides.
    let agent_builds_in_b = || {
        // Saved again, as an editor does: newer than any build of it.
        fs::write(b.join("src/main.rs"), WORKING).unwrap();
        let mut build = Command::new(cargo);
        build
            .args(["build", "--offline", "--quiet"])
            .current_dir(&b)
            .env_clear()
            .envs(environment.clone());
        let built = bounded(build, "the agent's cargo build in b");
        assert!(built.status.success(), "{}", text(&built.stderr));
    };

    agent_builds_in_b();
    mkfifo(&fixture.base.join("latch"));
    fs::write(fixture.base.join("hold-a"), "").unwrap();
    let mut gate_a = fixture.sykli(&a, &["run", "sykli.json", "--json"]);
    gate_a.envs(environment.clone());
    let held = std::thread::spawn(move || bounded(gate_a, "a's gate"));
    wait_until("a's build is done and its test is held", || {
        fixture.base.join("a.ready").exists()
    });
    agent_builds_in_b();
    release(fixture.base.join("latch"));
    let out = held.join().unwrap();
    let code = out.status.code().unwrap_or(-1);
    let in_a = receipt(out, code);
    let mut gate_b = fixture.sykli(&b, &["run", "sykli.json", "--json"]);
    gate_b.envs(environment);
    let in_b = receipt(bounded(gate_b, "b's gate"), 0);
    for tree in [&a, &b] {
        assert_eq!(fixture.status(tree), "", "{}", tree.display());
    }
    Observed {
        outcomes: [outcome(&in_a).to_owned(), outcome(&in_b).to_owned()],
        targets: ["a", "b"]
            .map(|name| fs::read_to_string(fixture.base.join(format!("target-{name}"))).unwrap()),
    }
}

/// The hazard, and its repair. With one target directory for every
/// worktree, `a`'s gate runs `b`'s binary: `a`'s receipt claims a pass for
/// code that never ran. With `CARGO_TARGET_DIR=${worktree_cache}/cargo-target`
/// declared, which Cargo puts before its own configuration, each gate builds
/// in its own worktree's directory, outside its tree: `a` fails, as its code
/// does, and `b` passes, while the agent's own builds still go to the shared
/// directory.
#[test]
fn a_shared_target_dir_runs_another_worktrees_binary_and_worktree_cache_does_not() {
    let shared = Fixture::new("shared");
    let observed = hazard(&shared, None);
    assert_eq!(
        observed.outcomes,
        ["passed", "passed"],
        "the hazard did not reproduce: {observed:?}"
    );
    assert_eq!(observed.targets, ["", ""]);

    let isolated = Fixture::new("isolated");
    let observed = hazard(&isolated, Some("${worktree_cache}/cargo-target"));
    assert_eq!(observed.outcomes, ["failed", "passed"], "{observed:?}");
    let [in_a, in_b] = observed.targets.clone().map(PathBuf::from);
    for (target, tree) in [(&in_a, "a"), (&in_b, "b")] {
        assert_eq!(target.file_name().unwrap(), "cargo-target");
        assert_cache_of(
            &isolated,
            target.parent().unwrap(),
            &isolated.base.join(tree),
        );
        assert!(target.join("debug/probe").is_file(), "{observed:?}");
    }
    assert_ne!(in_a, in_b);
    // The agent's own builds kept Cargo's configuration.
    assert!(isolated.base.join("shared-target/debug/probe").is_file());
}

/// A worktree's directory is removed once the worktree is gone, when a run
/// is recorded in any worktree of the clone, and never while a run in it is
/// live: its coordinator, and any task of it that outlives its coordinator,
/// hold the directory's lease. A worktree that is still there keeps its
/// directory, and so does one git keeps locked, wherever it is. Each task
/// here removes the directory it was given, as `cargo clean` empties a
/// target directory: sykli's own marker and lease are not in it.
#[test]
fn a_gone_worktrees_directory_is_pruned_never_while_its_run_lives() {
    let fixture = Fixture::new("prune");
    fixture.write("file.txt", "one\n");
    fixture.contract(|b| {
        json!([{
            "name": "t",
            "run": format!(
                "{} && rm -rf \"$DIR\" && n=$(basename \"$(pwd)\") && if [ -e '{b}/hold-'$n ]; then \
                 echo $$ > '{b}/pid'; printf ready > '{b}/ready'; read token < '{b}/latch'; fi",
                note_dir(b)
            ),
            "inputs": ["file.txt"],
            "env": {"DIR": "${worktree_cache}"},
        }])
    });
    fixture.commit_locked(&["sykli.json"]);
    let a = fixture.worktree("a");
    let b = fixture.worktree("b");
    let c = fixture.worktree("c");
    for tree in [&fixture.repo, &a, &b, &c] {
        let ran = fixture.run(tree, "sykli.json", 0);
        assert_eq!(outcome(&ran), "passed", "{ran}");
    }
    // Each worktree's own `<id>/`, which holds the directory its tasks got.
    let [main, in_a, in_b, in_c] = ["repo", "a", "b", "c"].map(|name| {
        fixture
            .seen(&format!("dir-{name}"))
            .parent()
            .unwrap()
            .to_path_buf()
    });
    for own in [&main, &in_a, &in_b, &in_c] {
        assert!(own.join("worktree.json").is_file(), "{}", own.display());
        assert!(!own.join("cache").exists(), "the task removed it");
    }

    // Removed, a worktree's directory goes at the next run, in any worktree.
    fixture.git(&fixture.repo, &["worktree", "remove", a.to_str().unwrap()]);
    let ran = fixture.run(&fixture.repo, "sykli.json", 0);
    assert_eq!(outcome(&ran), "cached", "{ran}");
    assert!(!in_a.exists(), "a's directory outlived a");
    assert!(main.is_dir() && in_b.is_dir() && in_c.is_dir());
    assert_eq!(
        fixture.caches_left().len(),
        3,
        "{:?}",
        fixture.caches_left()
    );

    // Locked, a worktree out of reach (on media not mounted now) keeps it.
    fixture.git(&fixture.repo, &["worktree", "lock", c.to_str().unwrap()]);
    let away = fixture.base.join("c-unmounted");
    fs::rename(&c, &away).unwrap();
    fixture.run(&fixture.repo, "sykli.json", 0);
    assert!(in_c.is_dir(), "a locked worktree's directory was pruned");
    fixture.git(&fixture.repo, &["worktree", "unlock", c.to_str().unwrap()]);
    fixture.run(&fixture.repo, "sykli.json", 0);
    assert!(!in_c.exists(), "c's directory outlived c");

    // A run in b holds b's directory while it lives, even once b is gone.
    fs::write(b.join("file.txt"), "two\n").unwrap();
    fs::write(fixture.base.join("hold-b"), "").unwrap();
    mkfifo(&fixture.base.join("latch"));
    let mut live = fixture
        .sykli(&b, &["run", "sykli.json", "--json"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("b's task is held", || fixture.base.join("ready").exists());
    fixture.git(
        &fixture.repo,
        &["worktree", "remove", "--force", b.to_str().unwrap()],
    );
    assert!(!b.exists());
    fixture.run(&fixture.repo, "sykli.json", 0);
    assert!(in_b.is_dir(), "pruned while its run was live");

    // Its coordinator dies; its task goes on, holding the lease it inherited.
    live.kill().unwrap();
    live.wait().unwrap();
    fixture.run(&fixture.repo, "sykli.json", 0);
    assert!(in_b.is_dir(), "pruned while a task of its run was live");

    // Once nothing of that run is left, the directory goes, and nothing set
    // aside is left behind either.
    kill_group(&fs::read_to_string(fixture.base.join("pid")).unwrap());
    fixture.run(&fixture.repo, "sykli.json", 0);
    assert!(!in_b.exists(), "b's directory outlived b and its run");
    assert!(main.is_dir(), "the main checkout's directory was pruned");
    assert_eq!(
        fixture.caches_left().len(),
        1,
        "{:?}",
        fixture.caches_left()
    );
}

/// A run's coordinator holds the directory for as long as it runs, on its
/// own: here the task that names the directory has ended, and the one still
/// running (held at its latch) does not name it. Once the coordinator is
/// gone, the directory goes, although that task still runs: it never had
/// the directory.
#[test]
fn a_coordinator_holds_its_worktrees_directory_while_it_runs() {
    let fixture = Fixture::new("hold");
    fixture.write("file.txt", "one\n");
    fixture.contract(|b| {
        json!([
            {
                "name": "named",
                "run": note_dir(b),
                "inputs": ["file.txt"],
                "env": {"DIR": "${worktree_cache}"},
            },
            {
                "name": "held",
                "run": format!(
                    "n=$(basename \"$(pwd)\"); if [ -e '{b}/hold-'$n ]; then echo $$ > '{b}/pid'; \
                     printf ready > '{b}/ready'; read token < '{b}/latch'; fi"
                ),
                "after": ["named"],
                "inputs": ["file.txt"],
            },
        ])
    });
    fixture.commit_locked(&["sykli.json"]);
    let w = fixture.worktree("w");
    mkfifo(&fixture.base.join("latch"));
    fs::write(fixture.base.join("hold-w"), "").unwrap();
    let mut live = fixture
        .sykli(&w, &["run", "sykli.json", "--json"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the held task runs", || fixture.base.join("ready").exists());
    let own = fixture.seen("dir-w").parent().unwrap().to_path_buf();
    fixture.git(
        &fixture.repo,
        &["worktree", "remove", "--force", w.to_str().unwrap()],
    );
    let other = fixture.worktree("other");
    fixture.run(&other, "sykli.json", 0);
    assert!(own.is_dir(), "pruned while its run's coordinator lived");

    live.kill().unwrap();
    live.wait().unwrap();
    fixture.run(&other, "sykli.json", 0);
    assert!(!own.exists(), "kept with no run of its worktree holding it");
    kill_group(&fs::read_to_string(fixture.base.join("pid")).unwrap());
}

/// A detached run's coordinator, and a resumed run, expand
/// `${worktree_cache}` to the same directory as a run in the foreground.
#[test]
fn detached_and_resumed_runs_get_the_same_directory() {
    let fixture = Fixture::new("detach");
    fixture.write("file.txt", "one\n");
    fixture.contract(|b| {
        json!([{
            "name": "t",
            "run": format!("test -d \"$DIR\" && {}", note_dir(b)),
            "inputs": ["file.txt"],
            "env": {"DIR": "${worktree_cache}"},
        }])
    });
    fixture.commit_locked(&["sykli.json"]);
    let w = fixture.worktree("w");
    fixture.run(&w, "sykli.json", 0);
    let foreground = fixture.seen("dir-w");
    assert_cache_of(&fixture, &foreground, &w);
    let status = |run: &str| -> Value {
        let out = fixture
            .sykli(&w, &["status", "--run", run, "--json"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    };

    fs::remove_file(fixture.base.join("dir-w")).unwrap();
    fs::write(w.join("file.txt"), "two\n").unwrap();
    let out = fixture
        .sykli(&w, &["run", "sykli.json", "--detach", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let run = serde_json::from_slice::<Value>(&out.stdout).unwrap()["run"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_until("the detached run completes", || {
        status(&run)["state"] == "completed"
    });
    assert_eq!(status(&run)["outcome"], "passed", "{}", status(&run));
    assert_eq!(fixture.seen("dir-w"), foreground);

    // A run that fails to record its first attempt's start is resumed; the
    // resumed attempt gets the same directory.
    if !cfg!(debug_assertions) {
        return;
    }
    fs::remove_file(fixture.base.join("dir-w")).unwrap();
    fs::write(w.join("file.txt"), "three\n").unwrap();
    let out = fixture
        .sykli(&w, &["run", "sykli.json", "--json"])
        .env("SYKLI_TEST_FAIL_AT", "after-dispatch")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(!fixture.base.join("dir-w").exists());
    let run = text(&out.stderr)
        .lines()
        .find_map(|line| line.strip_prefix("run: ").map(str::to_owned))
        .unwrap_or_else(|| panic!("no run id: {}", text(&out.stderr)));
    let resumed = fixture
        .sykli(&w, &["resume", "--run", &run, "--json"])
        .output()
        .unwrap();
    assert_eq!(resumed.status.code(), Some(0), "{}", text(&resumed.stderr));
    assert_eq!(fixture.seen("dir-w"), foreground);
}
