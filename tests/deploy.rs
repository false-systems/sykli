//! `sykli deploy` and `sykli deployed`, as a person runs them: the real
//! binary, real git with a bare remote, and a deploy contract whose task
//! appends the commit it ships to a file outside the repository, so each
//! deployment that really ran leaves one line.
#![cfg(unix)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

/// The gate: passes while `file.txt` says `ok`.
const GATE: &str = r#"{"schema":"sykli-contract.v1","tasks":[{"name":"check","run":"grep -q ok file.txt","inputs":["file.txt"]}]}"#;

/// The deployment: one task, which fails while `.sykli/fail` exists and
/// makes a commit of its own while `.sykli/move` does (files no tree
/// carries).
const DEPLOY: &str = r#"{"schema":"sykli-contract.v1","tasks":[{"name":"ship","run":"sh deploy/ship.sh","inputs":["deploy/ship.sh"]}]}"#;
const SHIP: &str = "test ! -e .sykli/fail || exit 1
test ! -e .sykli/move || git commit --quiet --allow-empty -m moved || exit 1
git rev-parse HEAD >> ../shipped.log
";

const NOTES: &str = "refs/notes/sykli/deploy";

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", text(&out.stderr));
    text(&out.stdout).trim().to_owned()
}

fn identify(root: &Path) {
    git(root, &["config", "user.name", "Widget Test"]);
    git(root, &["config", "user.email", "widget@example.invalid"]);
}

fn commit(root: &Path, message: &str) -> String {
    git(root, &["add", "--all"]);
    git(root, &["commit", "--quiet", "--no-verify", "-m", message]);
    git(root, &["rev-parse", "HEAD"])
}

fn sykli(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

fn exit(out: &Output, code: i32) {
    assert_eq!(
        out.status.code(),
        Some(code),
        "stdout: {}\nstderr: {}",
        text(&out.stdout),
        text(&out.stderr)
    );
}

/// A bare `origin.git` whose `main` holds both contracts, and a clone of it
/// on branch `work` with one commit more, `change`, which the gate passes.
struct World {
    dir: PathBuf,
    work: PathBuf,
}

impl World {
    fn new(name: &str) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("sykli-deploy-{name}-{nonce}"));
        let work = dir.join("work");
        fs::create_dir_all(work.join("deploy")).unwrap();
        git(
            &dir,
            &["init", "--quiet", "--bare", "-b", "main", "origin.git"],
        );
        git(&work, &["init", "--quiet", "-b", "main"]);
        identify(&work);
        git(&work, &["remote", "add", "origin", "../origin.git"]);
        fs::write(work.join("sykli.json"), GATE).unwrap();
        fs::write(work.join("deploy.json"), DEPLOY).unwrap();
        fs::write(work.join("deploy/ship.sh"), SHIP).unwrap();
        fs::write(work.join("file.txt"), "ok one\n").unwrap();
        exit(&sykli(&work, &["lock", "sykli.json"]), 0);
        exit(&sykli(&work, &["lock", "deploy.json"]), 0);
        commit(&work, "fixture");
        git(&work, &["push", "--quiet", "origin", "main"]);
        git(&work, &["fetch", "--quiet", "origin"]);
        git(&work, &["checkout", "--quiet", "-b", "work"]);
        fs::write(work.join("file.txt"), "ok two\n").unwrap();
        commit(&work, "change");
        World { dir, work }
    }

    /// Put the commit checked out in `root` on the remote's `main`, and
    /// let `root` know.
    fn land(&self, root: &Path) {
        git(root, &["push", "--quiet", "origin", "HEAD:main"]);
        git(root, &["fetch", "--quiet", "origin"]);
    }

    /// The commits the deploy task shipped, oldest first.
    fn shipped(&self) -> Vec<String> {
        fs::read_to_string(self.dir.join("shipped.log"))
            .map(|log| log.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    fn done(self) {
        fs::remove_dir_all(self.dir).unwrap();
    }
}

/// The deployments recorded on `commit` in `root`, one per line.
fn recorded(root: &Path, commit: &str) -> Vec<serde_json::Value> {
    git(root, &["notes", "--ref", NOTES, "show", commit])
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn a_deployment_needs_a_proven_landed_commit_and_is_recorded_each_time_it_runs() {
    let world = World::new("ok");
    let work = &world.work;
    let head = git(work, &["rev-parse", "HEAD"]);
    let tree = git(work, &["rev-parse", "HEAD^{tree}"]);

    // Not proven, then proven but not in the default branch: each refusal
    // says what is missing, and nothing ships.
    let out = sykli(work, &["deploy"]);
    exit(&out, 1);
    assert!(text(&out.stderr).contains("not proven"), "{out:?}");
    exit(&sykli(work, &["run", "sykli.json"]), 0);
    let out = sykli(work, &["deploy"]);
    exit(&out, 1);
    assert!(
        text(&out.stderr).contains("is not in origin/main"),
        "{out:?}"
    );
    assert!(text(&out.stderr).contains("--allow-unlanded"), "{out:?}");
    assert!(world.shipped().is_empty());

    // Nothing recorded is not "nothing was deployed".
    let out = sykli(work, &["deployed"]);
    exit(&out, 0);
    assert!(
        text(&out.stdout).contains("nothing has been recorded in this clone"),
        "{out:?}"
    );
    let out = sykli(work, &["deployed", "--json"]);
    exit(&out, 0);
    let none: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(none["schema"], "sykli-deployed.v1");
    assert_eq!(none["recorded"], false);
    assert_eq!(none["targets"], serde_json::json!([]));

    // Overridden, it ships, and the record says it was not landed.
    let out = sykli(work, &["deploy", "--allow-unlanded", "--target", "staging"]);
    exit(&out, 0);
    assert!(
        text(&out.stdout).contains(&format!("deploy: {} deployed to staging", &head[..12])),
        "{out:?}"
    );
    // Not shared, it says how to.
    assert!(
        text(&out.stdout).contains("git push origin refs/notes/sykli/deploy"),
        "{out:?}"
    );
    assert_eq!(world.shipped(), std::slice::from_ref(&head));
    let lines = recorded(work, &head);
    assert_eq!(lines.len(), 1);
    let first = &lines[0];
    assert_eq!(first["schema"], "sykli-deployment.v1");
    assert_eq!(first["commit"], head.as_str());
    assert_eq!(first["tree"], tree.as_str());
    assert_eq!(first["target"], "staging");
    assert_eq!(first["contract"], "deploy.json");
    let lock: serde_json::Value =
        serde_json::from_slice(&fs::read(work.join("sykli.lock")).unwrap()).unwrap();
    assert_eq!(first["contract_digest"], lock["contracts"]["deploy.json"]);
    let receipt = work.join(format!(
        ".sykli/receipts/rcpt_{}.json",
        first["receipt_digest"].as_str().unwrap()
    ));
    let receipt: serde_json::Value = serde_json::from_slice(&fs::read(receipt).unwrap()).unwrap();
    assert_eq!(receipt["contract_hash"], first["contract_digest"]);
    assert_eq!(receipt["tasks"][0]["outcome"], "passed");
    assert!(first["at"].as_u64().unwrap() > 1_700_000_000);
    assert_eq!(first["unlanded"], true);

    // Landed, it ships without the override, to the default target.
    world.land(work);
    exit(&sykli(work, &["deploy"]), 0);
    let lines = recorded(work, &head);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1]["target"], "default");
    assert_eq!(lines[1]["unlanded"], false);
    // Deployed again, it runs again: a pass reused from the cache would be
    // a deployment that never happened.
    exit(&sykli(work, &["deploy", "--allow-unlanded"]), 0);
    assert_eq!(world.shipped(), [head.clone(), head.clone(), head.clone()]);
    let lines = recorded(work, &head);
    assert_eq!(lines.len(), 3);
    assert_eq!(
        lines[2]["unlanded"], false,
        "a landed commit is recorded as landed whatever was allowed"
    );

    // What went out: the latest per target, from this clone's notes.
    let out = sykli(work, &["deployed"]);
    exit(&out, 0);
    let said = text(&out.stdout);
    let line = |target: &str| {
        said.lines()
            .find(|line| line.starts_with(&format!("{target}: ")))
            .unwrap_or_else(|| panic!("no line for {target}: {said}"))
            .to_owned()
    };
    assert!(
        line("default").contains(&head[..12]) && line("default").contains("change"),
        "{said}"
    );
    assert!(!line("default").contains("unlanded"), "{said}");
    assert!(line("staging").contains("unlanded"), "{said}");
    assert_eq!(said.lines().count(), 2, "{said}");
    let out = sykli(work, &["deployed", "--json"]);
    exit(&out, 0);
    let all: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(all["recorded"], true);
    assert_eq!(all["unreadable"], 0);
    let targets = all["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 2);
    assert_eq!(targets[0]["target"], "default");
    assert_eq!(targets[0]["commit"], head.as_str());
    assert_eq!(targets[0]["subject"], "change");
    assert_eq!(targets[0]["at"], lines[2]["at"]);
    assert_eq!(targets[0]["receipt_digest"], lines[2]["receipt_digest"]);
    assert_eq!(targets[1]["target"], "staging");
    assert_eq!(targets[1]["unlanded"], true);
    let out = sykli(work, &["deployed", "--target", "staging"]);
    exit(&out, 0);
    assert_eq!(text(&out.stdout).lines().count(), 1);
    assert!(text(&out.stdout).starts_with("staging: "), "{out:?}");
    let out = sykli(work, &["deployed", "--target", "elsewhere"]);
    exit(&out, 0);
    assert!(
        text(&out.stdout).contains("nothing has been recorded in this clone for elsewhere"),
        "{out:?}"
    );

    // Uncommitted work is not what the receipt proved.
    fs::write(work.join("stray.txt"), "x\n").unwrap();
    let out = sykli(work, &["deploy"]);
    exit(&out, 1);
    assert!(text(&out.stderr).contains("commit first"), "{out:?}");
    assert_eq!(world.shipped().len(), 3);
    world.done();
}

#[test]
fn a_failed_deploy_run_records_nothing_as_deployed() {
    let world = World::new("failed");
    let work = &world.work;
    world.land(work);
    exit(&sykli(work, &["run", "sykli.json"]), 0);
    fs::create_dir_all(work.join(".sykli")).unwrap();
    fs::write(work.join(".sykli/fail"), "").unwrap();

    let out = sykli(work, &["deploy"]);
    exit(&out, 1);
    assert!(
        text(&out.stderr).contains("nothing is recorded as deployed"),
        "{out:?}"
    );
    assert!(world.shipped().is_empty());
    let out = sykli(work, &["deployed"]);
    exit(&out, 0);
    assert!(
        text(&out.stdout).contains("nothing has been recorded in this clone"),
        "{out:?}"
    );
    // A deploy run that moves HEAD passed, for a commit nobody checked: the
    // deployment is not recorded on either commit, and it says so.
    fs::remove_file(work.join(".sykli/fail")).unwrap();
    fs::write(work.join(".sykli/move"), "").unwrap();
    let before = git(work, &["rev-parse", "HEAD"]);
    let out = sykli(work, &["deploy"]);
    exit(&out, 2);
    assert!(text(&out.stderr).contains("HEAD moved"), "{out:?}");
    assert!(
        text(&out.stderr).contains("no deployment is recorded"),
        "{out:?}"
    );
    assert_ne!(git(work, &["rev-parse", "HEAD"]), before);
    assert!(
        Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", NOTES])
            .current_dir(work)
            .output()
            .unwrap()
            .stdout
            .is_empty()
    );
    git(work, &["reset", "--quiet", "--hard", &before]);
    fs::remove_file(work.join(".sykli/move")).unwrap();
    fs::remove_file(world.dir.join("shipped.log")).unwrap();

    // A gate that failed is not proof either, whatever the deploy would do.
    fs::write(work.join("file.txt"), "bad\n").unwrap();
    commit(work, "break the gate");
    world.land(work);
    exit(&sykli(work, &["run", "sykli.json"]), 1);
    let out = sykli(work, &["deploy"]);
    exit(&out, 1);
    assert!(text(&out.stderr).contains("not proven"), "{out:?}");
    assert!(world.shipped().is_empty());
    world.done();
}

#[test]
fn records_shared_from_two_clones_are_both_kept() {
    let world = World::new("shared");
    let work = &world.work;
    world.land(work);
    let head = git(work, &["rev-parse", "HEAD"]);
    exit(&sykli(work, &["run", "sykli.json"]), 0);
    git(&world.dir, &["clone", "--quiet", "origin.git", "other"]);
    let other = world.dir.join("other");
    identify(&other);
    exit(&sykli(&other, &["run", "sykli.json"]), 0);

    let out = sykli(work, &["deploy", "--target", "east", "--remote", "origin"]);
    exit(&out, 0);
    assert!(text(&out.stdout).contains("shared with origin"), "{out:?}");
    // The other clone has not seen that record: its own is merged with it,
    // on the same commit, and neither is lost.
    exit(
        &sykli(
            &other,
            &["deploy", "--target", "west", "--remote", "origin"],
        ),
        0,
    );
    let targets = |root: &Path| {
        let mut targets = recorded(root, &head)
            .iter()
            .map(|line| line["target"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        targets.sort();
        targets
    };
    assert_eq!(targets(&world.dir.join("origin.git")), ["east", "west"]);
    assert_eq!(targets(&other), ["east", "west"]);
    assert_eq!(targets(work), ["east"], "nothing was fetched here since");
    let out = sykli(&other, &["deployed"]);
    exit(&out, 0);
    let said = text(&out.stdout);
    assert!(said.contains("east: ") && said.contains("west: "), "{said}");

    // A remote that cannot be reached: deployed and recorded here, and said
    // not to be shared.
    git(work, &["remote", "add", "gone", "../gone.git"]);
    git(work, &["update-ref", "refs/remotes/gone/main", "HEAD"]);
    let out = sykli(work, &["deploy", "--remote", "gone"]);
    exit(&out, 2);
    assert!(
        text(&out.stderr).contains("recorded in this clone, but not shared"),
        "{out:?}"
    );
    assert_eq!(targets(work), ["default", "east"]);
    world.done();
}

#[test]
fn a_line_that_is_not_a_deployment_is_counted_not_dropped() {
    let world = World::new("unreadable");
    let work = &world.work;
    world.land(work);
    let head = git(work, &["rev-parse", "HEAD"]);
    exit(&sykli(work, &["run", "sykli.json"]), 0);
    exit(&sykli(work, &["deploy"]), 0);
    // A line of another schema, one that is not JSON, and a record copied
    // from another commit.
    let base = git(work, &["rev-parse", "HEAD~1"]);
    let note = git(work, &["notes", "--ref", NOTES, "show", &head]);
    let copied = note.lines().next().unwrap();
    let later = copied.replace("sykli-deployment.v1", "sykli-deployment.v2");
    fs::write(
        world.dir.join("note"),
        format!("{copied}\n{later}\nnot json\n"),
    )
    .unwrap();
    git(
        work,
        &["notes", "--ref", NOTES, "add", "-f", "-F", "../note", &head],
    );
    fs::write(world.dir.join("note"), format!("{copied}\n")).unwrap();
    git(
        work,
        &["notes", "--ref", NOTES, "add", "-f", "-F", "../note", &base],
    );

    let out = sykli(work, &["deployed"]);
    exit(&out, 0);
    assert!(text(&out.stdout).starts_with("default: "), "{out:?}");
    assert!(
        text(&out.stderr).contains("3 recorded lines could not be read"),
        "{out:?}"
    );
    let out = sykli(work, &["deployed", "--json"]);
    let all: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(all["unreadable"], 3);
    assert_eq!(all["targets"].as_array().unwrap().len(), 1);
    world.done();
}

#[test]
fn deploy_and_deployed_say_what_they_take() {
    for (command, flags) in [
        (
            "deploy",
            vec![
                "--target",
                "--remote",
                "--branch",
                "--allow-unlanded",
                "safe to repeat",
            ],
        ),
        ("deployed", vec!["--target", "--json"]),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
            .args([command, "--help"])
            .output()
            .unwrap();
        exit(&out, 0);
        let help = text(&out.stdout);
        for flag in flags {
            assert!(help.contains(flag), "{command} --help: {help}");
        }
        assert!(help.contains("Exit codes:"), "{command} --help: {help}");
    }
}
