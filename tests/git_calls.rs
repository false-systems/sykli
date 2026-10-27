//! The git processes sykli starts, counted. Every git process writes one
//! `"event":"start"` line to `GIT_TRACE2_EVENT`, nested ones included, so a
//! count is the whole cost of a command in git processes. A hook runs on
//! every push: sykli finds out what it needs from git once and passes it
//! down, and a caller that already knows the repository can say so
//! (`--repo-root`, `--git-common-dir`) and spare the discovery.

#[path = "support/stdio.rs"]
mod stdio;

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
};

// The git commands each command starts, exactly: their number is the
// budget, and their names show the count measured something.

/// `git rev-parse` once to find the repository, the inventory for the
/// undeclared-file checks, the subject's tree from one `cat-file --batch`,
/// and the working-tree snapshot (`read-tree`, `add`, `write-tree`).
const RUN: &[&str] = &[
    "rev-parse",
    "ls-files",
    "cat-file",
    "read-tree",
    "add",
    "write-tree",
];
/// The same with the repository given: no discovery.
const RUN_GIVEN: &[&str] = &["ls-files", "cat-file", "read-tree", "add", "write-tree"];
/// Discovery and one `cat-file --batch` for the commit.
const VERIFY_COMMIT: &[&str] = &["rev-parse", "cat-file"];
const VERIFY_COMMIT_GIVEN: &[&str] = &["cat-file"];
/// Discovery and one `cat-file --batch` for every pushed commit.
const PRE_PUSH: &[&str] = &["rev-parse", "cat-file"];
const PRE_PUSH_GIVEN: &[&str] = &["cat-file"];
/// `validate` refuses what `run` refuses before any task, so it lists the
/// inventory once, after discovery (none with the repository given).
const VALIDATE: &[&str] = &["rev-parse", "ls-files"];
const VALIDATE_GIVEN: &[&str] = &["ls-files"];
/// `init` finds the repository and lists it once, for what it declares and
/// for the judgement of the contract it would write.
const INIT: &[&str] = &["rev-parse", "ls-files"];

const ZEROS: &str = "0000000000000000000000000000000000000000";

/// The task reads one root file and two files in a folder, so the
/// undeclared-sibling check has a folder to inspect.
const CONTRACT: &str = r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"test \"$(cat file.txt)\" = one","inputs":["file.txt","src/a.txt","src/b.txt"]}]}"#;

struct Scratch {
    base: PathBuf,
    repo: PathBuf,
    config: PathBuf,
    traces: AtomicUsize,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("sykli-git-calls-{name}-{nonce}"));
        let repo = base.join("repo");
        let config = base.join("config");
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::create_dir_all(&config).unwrap();
        fs::write(base.join("empty-gitconfig"), "").unwrap();
        fs::write(repo.join("file.txt"), "one\n").unwrap();
        fs::write(repo.join("src/a.txt"), "a\n").unwrap();
        fs::write(repo.join("src/b.txt"), "b\n").unwrap();
        let scratch = Scratch {
            base,
            repo,
            config,
            traces: AtomicUsize::new(0),
        };
        scratch.git(&["init", "--quiet", "--initial-branch=main"]);
        fs::write(scratch.repo.join(".git/info/exclude"), ".sykli/\n").unwrap();
        scratch.commit("fixture");
        scratch
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(&self.repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn commit(&self, message: &str) -> String {
        self.git(&["add", "--all"]);
        self.git(&["commit", "--quiet", "--no-verify", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    /// What a caller that already asked git passes: `--repo-root` and
    /// `--git-common-dir`, as `git rev-parse --path-format=absolute` gives them.
    fn facts(&self) -> Vec<String> {
        let answer = self.git(&[
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-common-dir",
        ]);
        let mut lines = answer.lines();
        vec![
            "--repo-root".into(),
            lines.next().unwrap().into(),
            "--git-common-dir".into(),
            lines.next().unwrap().into(),
        ]
    }

    /// Run sykli with `GIT_TRACE2_EVENT` pointed at a fresh file, and count
    /// the git processes it started. The global git config is an empty file,
    /// so nothing configured on this machine starts processes of its own.
    fn counted(&self, args: &[&str], stdin: Option<&str>) -> (Output, Vec<String>) {
        self.counted_in(&self.repo, args, stdin)
    }

    /// `counted`, with sykli started in `directory`.
    fn counted_in(
        &self,
        directory: &Path,
        args: &[&str],
        stdin: Option<&str>,
    ) -> (Output, Vec<String>) {
        let trace = self.base.join(format!(
            "trace-{}.json",
            self.traces.fetch_add(1, Ordering::SeqCst)
        ));
        let out = stdio::output(
            Command::new(env!("CARGO_BIN_EXE_sykli"))
                .args(args)
                .current_dir(directory)
                .env("GIT_TRACE2_EVENT", &trace)
                .env("GIT_CONFIG_GLOBAL", self.base.join("empty-gitconfig")),
            stdin.unwrap_or_default().as_bytes(),
        )
        .unwrap();
        (out, started(&trace))
    }

    /// Run sykli traced, and say whether it started any git process at all.
    #[cfg(unix)]
    fn uncounted(&self, args: &[&str]) -> (Output, bool) {
        let trace = self.base.join(format!(
            "trace-{}.json",
            self.traces.fetch_add(1, Ordering::SeqCst)
        ));
        let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
            .args(args)
            .current_dir(&self.repo)
            .env("GIT_TRACE2_EVENT", &trace)
            .env("GIT_CONFIG_GLOBAL", self.base.join("empty-gitconfig"))
            .output()
            .unwrap();
        (out, trace.exists())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The git commands a trace records starting (`"event":"start"`), by
/// name: the first word of each argv after git's own options. A trace that
/// is missing or records nothing fails, because counting nothing would
/// prove nothing: every command measured here starts git at least once.
fn started(trace: &Path) -> Vec<String> {
    let text = fs::read_to_string(trace).unwrap_or_else(|error| {
        panic!("no GIT_TRACE2_EVENT trace at {}: {error}", trace.display())
    });
    let commands = text
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["event"] == "start")
        .map(|event| {
            let argv = event["argv"].as_array().cloned().unwrap_or_default();
            let mut words = argv.iter().skip(1).filter_map(|word| word.as_str());
            while let Some(word) = words.next() {
                match word {
                    "-C" | "-c" => {
                        words.next();
                    }
                    option if option.starts_with('-') => {}
                    command => return command.to_string(),
                }
            }
            String::new()
        })
        .collect::<Vec<_>>();
    assert!(
        !commands.is_empty(),
        "the trace at {} records no git process",
        trace.display()
    );
    commands
}

/// Assert a command's exit code and that it started exactly the expected git
/// commands: no more (the budget), no fewer, and these (the count measured
/// what it claims to).
fn within(label: &str, (out, started): (Output, Vec<String>), code: i32, expected: &[&str]) {
    assert_eq!(
        out.status.code(),
        Some(code),
        "{label}: {}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    let mut started = started;
    started.sort();
    let mut expected = expected.to_vec();
    expected.sort();
    assert_eq!(started, expected, "{label}: the git processes started");
}

fn with<'a>(args: &[&'a str], facts: &'a [String]) -> Vec<&'a str> {
    let mut all = args.to_vec();
    all.extend(facts.iter().map(String::as_str));
    all
}

#[test]
fn validate_stays_within_its_git_budget() {
    let scratch = Scratch::new("validate");
    let contract = scratch.config.join("sykli.json");
    fs::write(&contract, CONTRACT).unwrap();
    let contract = contract.to_str().unwrap();
    let lock = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["lock", contract])
        .output()
        .unwrap();
    assert!(lock.status.success(), "{}", text(&lock.stderr));
    within(
        "validate",
        scratch.counted(&["validate", contract], None),
        0,
        VALIDATE,
    );
    within(
        "validate (given)",
        scratch.counted(&with(&["validate", contract], &scratch.facts()), None),
        0,
        VALIDATE_GIVEN,
    );
}

/// `init` lists the repository once, and judges the contract it would write
/// from that same listing (`validate` and `run`'s refusal): no git process
/// beyond discovery and the one listing, where it writes and where it
/// refuses.
#[test]
fn init_judges_its_contract_within_its_git_budget() {
    let scratch = Scratch::new("init");
    fs::write(
        scratch.repo.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(scratch.repo.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    fs::create_dir_all(scratch.repo.join("app/src")).unwrap();
    fs::write(
        scratch.repo.join("app/Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(scratch.repo.join("app/src/main.rs"), "fn main() {}\n").unwrap();
    scratch.commit("crates");
    within(
        "init",
        scratch.counted(&["init", "--no-lock"], None),
        0,
        INIT,
    );
    // In a subdirectory, `src/lib.rs` above it refuses the contract.
    within(
        "init (refused in a subdirectory)",
        scratch.counted_in(&scratch.repo.join("app"), &["init", "--no-lock"], None),
        1,
        INIT,
    );
}

#[test]
fn run_verify_and_pre_push_stay_within_their_git_budgets() {
    let scratch = Scratch::new("outside");
    let contract = scratch.config.join("sykli.json");
    fs::write(&contract, CONTRACT).unwrap();
    let contract = contract.to_str().unwrap();
    let lock = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["lock", contract])
        .output()
        .unwrap();
    assert!(lock.status.success(), "{}", text(&lock.stderr));
    let facts = scratch.facts();
    let none: [String; 0] = [];

    for (given, run, verify, push) in [
        (&none[..], RUN, VERIFY_COMMIT, PRE_PUSH),
        (&facts[..], RUN_GIVEN, VERIFY_COMMIT_GIVEN, PRE_PUSH_GIVEN),
    ] {
        let label = if given.is_empty() { "" } else { " (given)" };
        let head = scratch.git(&["rev-parse", "HEAD"]);
        within(
            &format!("run{label}"),
            scratch.counted(&with(&["run", contract], given), None),
            0,
            run,
        );
        let receipt = scratch.base.join(format!("receipt{}.json", given.len()));
        let (out, _) = scratch.counted(&with(&["run", contract, "--json"], given), None);
        fs::write(&receipt, out.stdout).unwrap();
        within(
            &format!("verify --commit{label}"),
            scratch.counted(
                &with(
                    &[
                        "verify",
                        receipt.to_str().unwrap(),
                        "--contract",
                        contract,
                        "--commit",
                        "HEAD",
                    ],
                    given,
                ),
                None,
            ),
            0,
            verify,
        );
        let pre_push = with(
            &[
                "hook",
                "pre-push",
                "--contract",
                contract,
                "--branch",
                "main",
            ],
            given,
        );
        let mut accepted = pre_push.clone();
        accepted.extend(["origin", "/nowhere/origin.git"]);
        let line = |sha: &str| format!("refs/heads/work {sha} refs/heads/main {ZEROS}\n");
        within(
            &format!("pre-push, accepted{label}"),
            scratch.counted(&accepted, Some(&line(&head))),
            0,
            push,
        );
        // A new commit with no receipt is refused, within the same budget.
        fs::write(
            scratch.repo.join("file.txt"),
            format!("one\n{}\n", given.len()),
        )
        .unwrap();
        let unverified = scratch.commit("no receipt");
        within(
            &format!("pre-push, refused{label}"),
            scratch.counted(&accepted, Some(&line(&unverified))),
            1,
            push,
        );
        fs::write(scratch.repo.join("file.txt"), "one\n").unwrap();
        scratch.commit("back to one");
    }
}

/// A task that uses `${worktree_cache}` costs no git process: which worktree
/// a run is in is read from its `.git` (a directory in the main checkout, a
/// `gitdir:` file in a linked worktree), never asked of git. Each run
/// counted here is a first run in its worktree, and its task fails unless it
/// got the directory, so each count measured a run that expanded it.
#[test]
fn a_worktree_cache_costs_no_git_process() {
    let scratch = Scratch::new("worktree-cache");
    fs::write(
        scratch.repo.join("sykli.json"),
        CONTRACT.replace(
            r#""run":"test"#,
            r#""env":{"DIR":"${worktree_cache}/x"},"run":"mkdir \"$DIR\" && test"#,
        ),
    )
    .unwrap();
    let lock = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["lock", "sykli.json"])
        .current_dir(&scratch.repo)
        .output()
        .unwrap();
    assert!(lock.status.success(), "{}", text(&lock.stderr));
    scratch.commit("contract");
    let linked = scratch.base.join("linked");
    let given = scratch.base.join("given");
    for worktree in [&linked, &given] {
        scratch.git(&[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            worktree.to_str().unwrap(),
        ]);
    }
    within(
        "run, main checkout",
        scratch.counted(&["run", "sykli.json"], None),
        0,
        RUN,
    );
    within(
        "run, linked worktree",
        scratch.counted_in(&linked, &["run", "sykli.json"], None),
        0,
        RUN,
    );
    let common = scratch.repo.join(".git");
    within(
        "run, linked worktree (given)",
        scratch.counted_in(
            &given,
            &[
                "--repo-root",
                given.to_str().unwrap(),
                "--git-common-dir",
                common.to_str().unwrap(),
                "run",
                "sykli.json",
            ],
            None,
        ),
        0,
        RUN_GIVEN,
    );
}

#[test]
fn a_committed_contract_and_a_guessed_branch_stay_within_the_budget() {
    let scratch = Scratch::new("inside");
    fs::write(scratch.repo.join("sykli.json"), CONTRACT).unwrap();
    let lock = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["lock", "sykli.json"])
        .current_dir(&scratch.repo)
        .output()
        .unwrap();
    assert!(lock.status.success(), "{}", text(&lock.stderr));
    let head = scratch.commit("contract");
    scratch.git(&["update-ref", "refs/remotes/origin/main", &head]);
    scratch.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ]);
    let facts = scratch.facts();
    let none: [String; 0] = [];
    for (given, verify, push) in [
        (&none[..], VERIFY_COMMIT, PRE_PUSH),
        (&facts[..], VERIFY_COMMIT_GIVEN, PRE_PUSH_GIVEN),
    ] {
        let label = if given.is_empty() { "" } else { " (given)" };
        let receipt = scratch.base.join(format!("receipt{}.json", given.len()));
        let (out, _) = scratch.counted(&with(&["run", "sykli.json", "--json"], given), None);
        fs::write(&receipt, out.stdout).unwrap();
        within(
            &format!("verify --commit{label}"),
            scratch.counted(
                &with(
                    &[
                        "verify",
                        receipt.to_str().unwrap(),
                        "--contract",
                        "sykli.json",
                        "--commit",
                        "HEAD",
                    ],
                    given,
                ),
                None,
            ),
            0,
            verify,
        );
        let mut args = with(&["hook", "pre-push"], given);
        args.extend(["origin", "/nowhere/origin.git"]);
        within(
            &format!("pre-push, guessed branch{label}"),
            scratch.counted(
                &args,
                Some(&format!("refs/heads/work {head} refs/heads/main {ZEROS}\n")),
            ),
            0,
            push,
        );
    }
}

/// The counter itself: a trace that is missing, empty, or that sykli could
/// not write is a failure, never a count of zero within every budget.
#[test]
fn a_count_without_a_trace_is_a_failure_not_zero() {
    let scratch = Scratch::new("vacuous");
    let missing = scratch.base.join("no-such-folder").join("trace.json");
    assert!(std::panic::catch_unwind(|| started(&missing)).is_err());
    let empty = scratch.base.join("empty.json");
    fs::write(&empty, "").unwrap();
    assert!(std::panic::catch_unwind(|| started(&empty)).is_err());
    // A real command whose trace cannot be written counts as no evidence.
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["hook", "pre-push", "--branch", "main", "origin", "/nowhere"])
        .current_dir(&scratch.repo)
        .env("GIT_TRACE2_EVENT", &missing)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(std::panic::catch_unwind(|| started(&missing)).is_err());
}

/// The inputs `verify --commit` recomputes come from the same single
/// `cat-file` process: a passing receipt edited to name another commit's
/// tree is refused for its inputs (exit 3) within the same budget, which
/// shows the commit's input entries were read there, not skipped.
#[test]
fn the_inputs_check_reads_the_commit_in_the_same_process() {
    let scratch = Scratch::new("inputs");
    let contract = scratch.config.join("sykli.json");
    fs::write(&contract, CONTRACT).unwrap();
    let contract = contract.to_str().unwrap();
    let lock = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["lock", contract])
        .output()
        .unwrap();
    assert!(lock.status.success(), "{}", text(&lock.stderr));
    let (out, _) = scratch.counted(&["run", contract, "--json"], None);
    let mut receipt: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    fs::write(scratch.repo.join("src/a.txt"), "changed\n").unwrap();
    let other = scratch.commit("another tree");
    let tree = scratch.git(&["rev-parse", "HEAD^{tree}"]);
    receipt["subject"]["tree_oid"] = tree.as_str().into();
    receipt["subject"]["head_tree_oid"] = tree.as_str().into();
    let path = scratch.base.join("rebound.json");
    fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    let facts = scratch.facts();
    let none: [String; 0] = [];
    for (given, expected) in [
        (&none[..], VERIFY_COMMIT),
        (&facts[..], VERIFY_COMMIT_GIVEN),
    ] {
        let args = with(
            &[
                "verify",
                path.to_str().unwrap(),
                "--contract",
                contract,
                "--commit",
                &other,
            ],
            given,
        );
        let (out, started) = scratch.counted(&args, None);
        assert!(
            text(&out.stderr).contains("refused: inputs"),
            "{}",
            text(&out.stderr)
        );
        within("verify --commit, rebound", (out, started), 3, expected);
    }
}

/// A contract outside the repository, locked, for the tests below.
fn outside_contract(scratch: &Scratch) -> String {
    let contract = scratch.config.join("sykli.json");
    fs::write(&contract, CONTRACT).unwrap();
    let lock = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["lock", contract.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(lock.status.success(), "{}", text(&lock.stderr));
    contract.to_str().unwrap().to_string()
}

/// `verify --commit` and the pre-push hook for `receipt` and HEAD, the
/// receipt being the only one in the worktree for the hook.
fn judged_at_head(
    scratch: &Scratch,
    contract: &str,
    receipt: &serde_json::Value,
    expected: &[&str],
    label: &str,
) {
    let head = scratch.git(&["rev-parse", "HEAD"]);
    let path = scratch.base.join(format!("{label}.json"));
    fs::write(&path, serde_json::to_vec(receipt).unwrap()).unwrap();
    within(
        &format!("verify --commit, {label}"),
        scratch.counted(
            &[
                "verify",
                path.to_str().unwrap(),
                "--contract",
                contract,
                "--commit",
                "HEAD",
            ],
            None,
        ),
        0,
        expected,
    );
    let _ = fs::remove_dir_all(scratch.repo.join(".sykli"));
    fs::create_dir_all(scratch.repo.join(".sykli")).unwrap();
    fs::write(
        scratch.repo.join(".sykli/receipt.json"),
        serde_json::to_vec(receipt).unwrap(),
    )
    .unwrap();
    within(
        &format!("pre-push, {label}"),
        scratch.counted(
            &[
                "hook",
                "pre-push",
                "--contract",
                contract,
                "--branch",
                "main",
                "origin",
                "/nowhere/origin.git",
            ],
            Some(&format!("refs/heads/work {head} refs/heads/main {ZEROS}\n")),
        ),
        0,
        expected,
    );
}

/// A receipt written before per-task input records has its inputs digest
/// recomputed from the commit's blobs, read in the second round of the same
/// `cat-file` that read the entries: it costs what a new receipt costs.
#[test]
fn an_older_receipt_is_judged_within_the_same_budget() {
    let scratch = Scratch::new("older");
    let contract = outside_contract(&scratch);
    let (out, _) = scratch.counted(&["run", &contract, "--json"], None);
    let mut receipt: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    for task in receipt["tasks"].as_array_mut().unwrap() {
        task.as_object_mut().unwrap().remove("inputs");
    }
    judged_at_head(&scratch, &contract, &receipt, VERIFY_COMMIT, "older");
}

/// An input a checkout converts (here, line endings under `core.autocrlf`)
/// is compared with what a checkout of its blob gives: one `cat-file
/// --filters` for that input, and only for it.
#[test]
fn a_converted_input_costs_one_cat_file_of_its_own() {
    let scratch = Scratch::new("converted");
    let contract = outside_contract(&scratch);
    scratch.git(&["config", "core.autocrlf", "true"]);
    fs::write(scratch.repo.join("src/a.txt"), "a\r\n").unwrap();
    assert_eq!(scratch.git(&["diff", "--name-only"]), "", "the same blob");
    let (out, _) = scratch.counted(&["run", &contract, "--json"], None);
    let receipt: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(receipt["outcome"], "passed");
    judged_at_head(
        &scratch,
        &contract,
        &receipt,
        &["rev-parse", "cat-file", "cat-file"],
        "converted",
    );
}

/// `status --run` reads a run's journal from the common git directory: one
/// `git rev-parse` finds it, and with the repository given no git runs at
/// all. `status --commit` reads the commit as `verify --commit` does,
/// discovery and one `cat-file --batch`, however many receipts and runs of
/// its tree it judges. Each answer is checked too, so the counts measured
/// commands that did their work.
#[cfg(unix)]
#[test]
fn status_queries_stay_within_their_git_budgets() {
    let scratch = Scratch::new("status");
    let contract = scratch.config.join("sykli.json");
    fs::write(&contract, CONTRACT).unwrap();
    let contract = contract.to_str().unwrap();
    let lock = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args(["lock", contract])
        .output()
        .unwrap();
    assert!(lock.status.success(), "{}", text(&lock.stderr));
    let mut runs = Vec::new();
    for _ in 0..2 {
        let (out, _) = scratch.counted(&["run", contract, "--json"], None);
        assert!(out.status.success(), "{}", text(&out.stderr));
        let receipt: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        runs.push(receipt["run"].as_str().unwrap().to_owned());
    }
    let facts = scratch.facts();
    let none: [String; 0] = [];
    for given in [&none[..], &facts[..]] {
        let label = if given.is_empty() { "" } else { " (given)" };
        let status = with(&["status", "--run", &runs[0], "--json"], given);
        let out = if given.is_empty() {
            let (out, started) = scratch.counted(&status, None);
            within(
                &format!("status --run{label}"),
                (out.clone(), started),
                0,
                &["rev-parse"],
            );
            out
        } else {
            let (out, traced) = scratch.uncounted(&status);
            assert!(!traced, "status --run{label} started git");
            out
        };
        let answer: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(answer["state"], "completed", "status --run{label}");

        let commit = with(
            &[
                "status",
                "--commit",
                "HEAD",
                "--contract",
                contract,
                "--json",
            ],
            given,
        );
        let (out, started) = scratch.counted(&commit, None);
        let expected: &[&str] = if given.is_empty() {
            &["rev-parse", "cat-file"]
        } else {
            &["cat-file"]
        };
        within(
            &format!("status --commit{label}"),
            (out.clone(), started),
            0,
            expected,
        );
        let answer: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(answer["verdict"], "passed", "status --commit{label}");
        assert_eq!(answer["current"].as_array().unwrap().len(), 2);
        assert_eq!(answer["runs"].as_array().unwrap().len(), 2);
    }
}
