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

/// The directory a worktree's tasks get: `<caches>/<16 hex>/cache`, with
/// sykli's own marker and lease beside it, and outside the work tree `tree`
/// (in its `.git` for the main checkout, whose git directory is the common
/// one; elsewhere for a linked worktree). Its `<16 hex>/`.
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
        id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()),
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

/// A contract with `tasks` (given where tasks may write) that `validate`
/// refuses (exit 1) and `lock`, `plan` and `run` refuse (exit 2), each
/// saying `says` and naming `${worktree_cache}`, before anything runs.
fn assert_refused(what: &str, says: &str, tasks: impl Fn(&str) -> Value) {
    let fixture = Fixture::new("refused");
    fixture.write("file.txt", "one\n");
    fixture.contract(tasks);
    let validate = fixture
        .sykli(&fixture.repo, &["validate", "sykli.json"])
        .output()
        .unwrap();
    assert_eq!(validate.status.code(), Some(1), "validate, {what}");
    let said = text(&validate.stderr);
    assert!(said.contains(says), "{what}: {said}");
    assert!(said.contains("${worktree_cache}"), "{what}: {said}");
    for args in [
        &["lock", "sykli.json"][..],
        &["plan", "sykli.json", "--json"],
        &["run", "sykli.json", "--json"],
    ] {
        let out = fixture.sykli(&fixture.repo, args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}, {what}");
        assert!(text(&out.stderr).contains(says), "{}", text(&out.stderr));
    }
    assert!(!fixture.base.join("ran").exists(), "{what}: a task ran");
    assert!(!fixture.repo.join(".git/sykli/runs").exists(), "{what}");
}

/// A placeholder sykli does not know is refused where a contract is
/// written or run, before anything runs, never handed to a task literally;
/// so is `$${`, kept free for an escape of `${` later, so that adding one
/// never changes what a contract that is valid now means. A `$` that opens
/// no `${` is not a placeholder and still reaches the task as written.
#[test]
fn an_unknown_or_unclosed_placeholder_is_refused_before_anything_runs() {
    for (value, says) in [
        ("${worktree_dir}/x", "${worktree_dir}"),
        ("${WORKTREE_CACHE}", "${WORKTREE_CACHE}"),
        ("a${}b", "${}"),
        ("${worktree_cache", "unclosed"),
        ("$${worktree_cache}", "reserved"),
        ("x$${HOME}", "reserved"),
    ] {
        assert_refused(value, says, |b| {
            json!([{
                "name": "t",
                "run": format!("touch '{b}/ran'"),
                "inputs": ["file.txt"],
                "env": {"X": value},
            }])
        });
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
            "run": "test \"$X\" = '$HOME/$x and $' && test -n \"${HOME}\" && test -z \"$worktree_cache_x\"",
            "inputs": ["file.txt"],
            "env": {"X": "$HOME/$x and $"},
        }])
    });
    fixture.commit_locked(&["sykli.json"]);
    let ran = fixture.run(&fixture.repo, "sykli.json", 0);
    assert_eq!(outcome(&ran), "passed", "{ran}");
}

/// sykli expands `${worktree_cache}` only in `env` values. Anywhere else a
/// task's shell would make it empty (`${worktree_cache}/cargo-target` is
/// `/cargo-target`, one directory for every worktree), so it is refused,
/// and the refusal says to declare it in `env`.
#[test]
fn the_placeholder_outside_env_is_refused() {
    let run = |command: &'static str| {
        move |b: &str| {
            json!([{
                "name": "t",
                "run": format!("{command}; touch '{b}/ran'"),
                "inputs": ["file.txt"],
            }])
        }
    };
    assert_refused(
        "run",
        "in its run command",
        run("printf %s ${worktree_cache}/x"),
    );
    assert_refused(
        "run, unbraced",
        "in its run command",
        run("echo $worktree_cache"),
    );
    assert_refused(
        "run, a default",
        "in its run command",
        run("echo ${worktree_cache:-/x}"),
    );
    for (field, value, says) in [
        ("inputs", json!(["${worktree_cache}/x"]), "in its inputs"),
        ("outputs", json!(["${worktree_cache}/x"]), "in its outputs"),
        ("workdir", json!("${worktree_cache}"), "in its workdir"),
        ("env", json!({"${worktree_cache}": "x"}), "in an env name"),
    ] {
        assert_refused(field, says, |b| {
            let mut task = json!({
                "name": "t",
                "run": format!("touch '{b}/ran'"),
                "inputs": ["file.txt"],
            });
            task[field] = value.clone();
            json!([task])
        });
    }
}

/// A contract is refused for an unknown placeholder only where it is written
/// or run, never where it is judged: a commit whose contract an earlier
/// sykli ran with `"PS4": "+${LINENO}: "` keeps the verdict its receipt
/// earns, from `verify --commit` and `status --commit`, as it did. (The
/// receipt is made here by running the same task with the value spelled
/// without braces, and naming the committed contract, as that sykli wrote
/// it, without a `run`.)
#[test]
fn an_old_contract_with_braces_is_still_judged() {
    let fixture = Fixture::new("old");
    fixture.write("file.txt", "one\n");
    let task = |ps4: &str| {
        json!({"schema": "sykli-contract.v1", "tasks": [{
            "name": "t",
            "run": "test \"$(cat file.txt)\" = one",
            "inputs": ["file.txt"],
            "env": {"PS4": ps4},
        }]})
    };
    let committed = serde_json::to_vec(&task("+${LINENO}: ")).unwrap();
    fs::write(fixture.repo.join("sykli.json"), &committed).unwrap();
    fixture.git(&fixture.repo, &["add", "--all"]);
    fixture.git(
        &fixture.repo,
        &["commit", "--quiet", "--no-verify", "-m", "c1"],
    );
    let rev = fixture.git(&fixture.repo, &["rev-parse", "HEAD"]);
    // Written and run today, it is refused.
    let validate = fixture
        .sykli(&fixture.repo, &["validate", "sykli.json"])
        .output()
        .unwrap();
    assert_eq!(validate.status.code(), Some(1));

    let outside = fixture.base.join("outside");
    fs::create_dir_all(&outside).unwrap();
    let spelled = outside.join("sykli.json");
    fs::write(&spelled, serde_json::to_vec(&task("+$LINENO: ")).unwrap()).unwrap();
    let mut ran = fixture.run(&fixture.repo, spelled.to_str().unwrap(), 0);
    let value: Value = serde_json::from_slice(&committed).unwrap();
    use sha2::{Digest, Sha256};
    let hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&value).unwrap()));
    ran["contract_hash"] = hash.into();
    ran["lock_digest"] = Value::Null;
    ran.as_object_mut().unwrap().remove("run");
    let receipts = fixture.repo.join(".sykli/receipts");
    let earlier = receipts.join("rcpt_earlier.json");
    fs::write(&earlier, serde_json::to_vec_pretty(&ran).unwrap()).unwrap();

    let verified = fixture
        .sykli(
            &fixture.repo,
            &[
                "verify",
                earlier.to_str().unwrap(),
                "--contract",
                "sykli.json",
                "--commit",
                &rev,
            ],
        )
        .output()
        .unwrap();
    assert_eq!(
        verified.status.code(),
        Some(0),
        "{}{}",
        text(&verified.stdout),
        text(&verified.stderr)
    );
    let status = fixture
        .sykli(
            &fixture.repo,
            &[
                "status",
                "--commit",
                &rev,
                "--contract",
                "sykli.json",
                "--json",
            ],
        )
        .output()
        .unwrap();
    assert_eq!(status.status.code(), Some(0), "{}", text(&status.stderr));
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["verdict"], "passed", "{status}");
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

/// What this repository recommends a Rust contract declare.
fn recommended() -> Value {
    json!({
        "CARGO_TARGET_DIR": "${worktree_cache}/cargo-target",
        "CARGO_BUILD_BUILD_DIR": "${worktree_cache}/cargo-build",
    })
}

/// A Cargo package, `probe`, under a Cargo whose own configuration puts
/// every build of every worktree in one target directory (and, with
/// `build_dir`, its intermediate artifacts in one `build-dir`), as
/// `$CARGO_HOME/config.toml` does on a machine that shares them. `main`
/// holds `a`'s code, whose binary exits 1, and a contract whose one task
/// runs `cargo test`, whose test runs the binary at its uplifted path
/// (`CARGO_BIN_EXE_probe`), as integration tests do; `env` is what the task
/// declares. A task held at `base/latch` when `base/hold-NAME` exists.
struct Probe {
    cargo: PathBuf,
    environment: [(&'static str, String); 2],
}

impl Probe {
    fn new(fixture: &Fixture, build_dir: bool, env: Option<Value>) -> Self {
        let cargo = PathBuf::from(env!("CARGO"));
        let toolchain = cargo.parent().unwrap().to_path_buf();
        assert!(
            toolchain.join("rustc").exists(),
            "no rustc beside {cargo:?}"
        );
        let base = fixture.base.to_str().unwrap();
        let home = fixture.base.join("home");
        fs::create_dir_all(home.join(".cargo")).unwrap();
        let mut config = format!("[build]\ntarget-dir = \"{base}/shared-target\"\n");
        if build_dir {
            config.push_str(&format!("build-dir = \"{base}/shared-build\"\n"));
        }
        fs::write(home.join(".cargo/config.toml"), config).unwrap();
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
        let lockfile = Command::new(&cargo)
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
            if let Some(env) = &env {
                task["env"] = env.clone();
            }
            json!([task])
        });
        fixture.commit_locked(&["sykli.json"]);
        Probe { cargo, environment }
    }

    /// A gate: `run --json` in `tree`, bounded; its receipt, whatever it
    /// exits with.
    fn gate(&self, fixture: &Fixture, tree: &Path) -> Value {
        let mut gate = fixture.sykli(tree, &["run", "sykli.json", "--json"]);
        gate.envs(self.environment.clone());
        let out = bounded(gate, "a gate");
        let code = out.status.code().unwrap_or(-1);
        receipt(out, code)
    }

    /// An agent's own `cargo build` in `tree`, after saving `main.rs` again
    /// as an editor does (newer than any build of it): Cargo's own
    /// configuration decides where it goes.
    fn agent_builds(&self, tree: &Path, code: &str) {
        fs::write(tree.join("src/main.rs"), code).unwrap();
        let mut build = Command::new(&self.cargo);
        build
            .args(["build", "--offline", "--quiet"])
            .current_dir(tree)
            .env_clear()
            .envs(self.environment.clone());
        let built = bounded(build, "an agent's cargo build");
        assert!(built.status.success(), "{}", text(&built.stderr));
    }
}

/// Two worktrees, `a` (`main`'s code) and `b` (code whose binary exits 0),
/// of a `Probe` clone. An agent works in `b` with its own `cargo build`
/// (isolating gates leaves a person's or an agent's own commands as they
/// are): once before `a`'s gate, and once more while `a`'s gate is held
/// after its build, at its test. Then `a`'s test goes on, and `b`'s gate
/// runs.
///
/// Either way Cargo shares a directory, `a` runs `b`'s binary: a Cargo that
/// hashes a package by its path relative to the workspace finds `a`'s build
/// fresh after `b`'s (its sources are older) and builds nothing, and every
/// Cargo puts `b`'s second build at the same `debug/probe`.
fn hazard(fixture: &Fixture, build_dir: bool, env: Option<Value>) -> Observed {
    let probe = Probe::new(fixture, build_dir, env);
    let a = fixture.worktree("a");
    let b = fixture.worktree("b");
    fs::write(b.join("src/main.rs"), WORKING).unwrap();
    fixture.git(&b, &["commit", "--quiet", "--no-verify", "-am", "b works"]);

    probe.agent_builds(&b, WORKING);
    mkfifo(&fixture.base.join("latch"));
    fs::write(fixture.base.join("hold-a"), "").unwrap();
    let held = std::thread::scope(|scope| {
        let held = scope.spawn(|| probe.gate(fixture, &a));
        wait_until("a's build is done and its test is held", || {
            fixture.base.join("a.ready").exists()
        });
        probe.agent_builds(&b, WORKING);
        release(fixture.base.join("latch"));
        held.join().unwrap()
    });
    let in_b = probe.gate(fixture, &b);
    for tree in [&a, &b] {
        assert_eq!(fixture.status(tree), "", "{}", tree.display());
    }
    Observed {
        outcomes: [outcome(&held).to_owned(), outcome(&in_b).to_owned()],
        targets: ["a", "b"]
            .map(|name| fs::read_to_string(fixture.base.join(format!("target-{name}"))).unwrap()),
    }
}

/// The hazard, and its repair. With one target directory for every
/// worktree, `a`'s gate runs `b`'s binary: `a`'s receipt claims a pass for
/// code that never ran. With the recommended `CARGO_TARGET_DIR` and
/// `CARGO_BUILD_BUILD_DIR` under `${worktree_cache}` declared, which Cargo
/// puts before its own configuration, each gate builds in its own
/// worktree's directory, outside its tree: `a` fails, as its code does, and
/// `b` passes, while the agent's own builds still go to the shared
/// directory.
#[test]
fn a_shared_target_dir_runs_another_worktrees_binary_and_worktree_cache_does_not() {
    let shared = Fixture::new("shared");
    let help = shared
        .sykli(&shared.repo, &["run", "--help"])
        .output()
        .unwrap();
    for says in [
        "\"CARGO_TARGET_DIR\": \"${worktree_cache}/cargo-target\"",
        "\"CARGO_BUILD_BUILD_DIR\": \"${worktree_cache}/cargo-build\"",
    ] {
        assert!(text(&help.stdout).contains(says), "run --help: {says}");
    }
    let observed = hazard(&shared, false, None);
    assert_eq!(
        observed.outcomes,
        ["passed", "passed"],
        "the hazard did not reproduce: {observed:?}"
    );
    assert_eq!(observed.targets, ["", ""]);

    let isolated = Fixture::new("isolated");
    let observed = hazard(&isolated, false, Some(recommended()));
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

/// Where Cargo has a `build-dir` (stable since Cargo 1.91), a shared one
/// holds every build's intermediate artifacts, fingerprints and test
/// binaries, and `CARGO_TARGET_DIR` moves only the final ones: with it
/// alone, `a`'s gate still finds `b`'s build fresh and passes on code that
/// never ran. `CARGO_BUILD_BUILD_DIR=${worktree_cache}/cargo-build` as well
/// closes it. A Cargo without `build-dir` ignores both the setting and the
/// variable, and this test says so and stops.
#[test]
fn a_shared_build_dir_needs_cargo_build_build_dir_too() {
    let target_only = Fixture::new("builddir");
    let observed = hazard(
        &target_only,
        true,
        Some(json!({"CARGO_TARGET_DIR": "${worktree_cache}/cargo-target"})),
    );
    if !target_only.base.join("shared-build").exists() {
        eprintln!("skipped: this Cargo has no build-dir (stable since 1.91)");
        return;
    }
    assert_eq!(
        observed.outcomes,
        ["passed", "passed"],
        "CARGO_TARGET_DIR alone closed it after all: {observed:?}"
    );

    let both = Fixture::new("builddir-both");
    let observed = hazard(&both, true, Some(recommended()));
    assert_eq!(observed.outcomes, ["failed", "passed"], "{observed:?}");
    for name in ["a", "b"] {
        let target = PathBuf::from(&observed.targets[usize::from(name == "b")]);
        let own = assert_cache_of(&both, target.parent().unwrap(), &both.base.join(name));
        assert!(own.join("cache/cargo-build").is_dir(), "{}", own.display());
    }
}

/// A worktree removed and made again at the same path (as a caller's
/// disposable checkout is) gets the same git directory from git, but never
/// the directory of the worktree it replaced: that one holds another
/// commit's build, which a checkout that restores file times (as
/// `git-restore-mtime` and CI cache recipes do) would find fresh. Here the
/// first incarnation builds `b`'s code, which passes; the second checks out
/// `a`'s with its files dated before that build, and must fail.
#[test]
fn a_worktree_made_again_at_the_same_path_gets_a_new_directory() {
    let fixture = Fixture::new("again");
    let probe = Probe::new(&fixture, false, Some(recommended()));
    fixture.git(&fixture.repo, &["checkout", "--quiet", "-b", "b"]);
    fs::write(fixture.repo.join("src/main.rs"), WORKING).unwrap();
    fixture.git(
        &fixture.repo,
        &["commit", "--quiet", "--no-verify", "-am", "b"],
    );
    fixture.git(&fixture.repo, &["checkout", "--quiet", "main"]);
    let path = fixture.base.join("checkout");
    let add = |branch: &str| {
        fixture.git(
            &fixture.repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                path.to_str().unwrap(),
                branch,
            ],
        )
    };

    add("b");
    let first = probe.gate(&fixture, &path);
    assert_eq!(outcome(&first), "passed", "{first}");
    let before = fixture.seen("target-checkout");
    fixture.git(
        &fixture.repo,
        &["worktree", "remove", path.to_str().unwrap()],
    );
    add("main");
    let old = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    for file in fixture.git(&path, &["ls-files"]).lines() {
        fs::File::options()
            .write(true)
            .open(path.join(file))
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    let second = probe.gate(&fixture, &path);
    assert_eq!(outcome(&second), "failed", "{second}");
    let after = fixture.seen("target-checkout");
    assert_ne!(
        after, before,
        "the new worktree got the old one's directory"
    );
    assert_cache_of(&fixture, after.parent().unwrap(), &path);
    // The old incarnation's directory is gone with it.
    assert!(!before.exists(), "{} was kept", before.display());
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
