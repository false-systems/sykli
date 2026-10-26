//! Sykli is the content-addressed evaluator for declared work graphs.
//!
//! A receipt claims exactly what ran — never what it meant.

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode, Stdio};
use std::thread;
use std::time::Instant;
use sykli::{Contract, Task};

mod canonical;
mod cargo_inputs;
mod hook;
mod init;
#[cfg(unix)]
mod journal;
mod repo;
#[cfg(unix)]
mod runs;
mod worktree_cache;

/// Durable runs need Unix file leases. Elsewhere there is no coordinator:
/// these types have no values, so every use of one is unreachable.
#[cfg(not(unix))]
mod runs {
    use crate::{Outcome, TaskReceipt};

    pub(crate) enum Coordinator {}
    pub(crate) struct Attempt<'a>(std::convert::Infallible, std::marker::PhantomData<&'a ()>);

    impl Coordinator {
        pub(crate) fn dispatch(&self, _: &str, _: &str) -> Result<Attempt<'_>, String> {
            match *self {}
        }
        pub(crate) fn reuse(&self, _: &str, _: &str, _: &TaskReceipt) -> Result<(), String> {
            match *self {}
        }
        pub(crate) fn settle(
            &self,
            _: &str,
            _: Option<&str>,
            _: &TaskReceipt,
        ) -> Result<(), String> {
            match *self {}
        }
        pub(crate) fn conclude(
            &self,
            _: &std::path::Path,
            _: &str,
            _: &[u8],
            _: Outcome,
        ) -> Result<(), String> {
            match *self {}
        }
    }

    impl Attempt<'_> {
        pub(crate) fn started(&self, _: i32, _: u64) -> Result<(), String> {
            match self.0 {}
        }
        pub(crate) fn finished(
            &self,
            _: &TaskReceipt,
            _: std::time::Instant,
        ) -> Result<(), String> {
            match self.0 {}
        }
        pub(crate) fn interrupted(
            &self,
            _: i32,
            _: Option<i32>,
            _: Option<i32>,
        ) -> Result<(), String> {
            match self.0 {}
        }
    }

    pub(crate) fn signalled() -> Option<i32> {
        None
    }
}

#[derive(Parser)]
#[command(
    name = "sykli",
    version,
    about = "Content-addressed evaluator for declared work graphs"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// The repository's top level, when the caller already knows it (as
    /// `git rev-parse --path-format=absolute --show-toplevel` prints it); with
    /// --git-common-dir, sykli runs no git to find the repository. Its .git
    /// must lead to --git-common-dir (checked from files), or sykli exits 2
    #[arg(
        long,
        global = true,
        value_name = "ABS_PATH",
        requires = "git_common_dir"
    )]
    repo_root: Option<PathBuf>,
    /// The repository's common git directory, when the caller already knows it
    /// (as `git rev-parse --path-format=absolute --git-common-dir` prints it);
    /// given with --repo-root
    #[arg(long, global = true, value_name = "ABS_PATH", requires = "repo_root")]
    git_common_dir: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Validate a declared contract without executing it
    #[command(after_help = "Exit codes:\n  \
        0  valid\n  \
        1  invalid, unreadable, or drifted from sykli.lock\n  \
        2  refused for undeclared files, as `run` refuses it (also exit 2), or that check failed\n\n\
        Inside a git repository, validate also makes the check `run` makes before any task, so\n\
        its verdict depends on the work tree, not only on the contract and its lock. In a Cargo\n\
        graph, every Rust source, Cargo file and file under tests/, and every file a source\n\
        includes must be declared, and an include must name its path as a literal; a\n\
        document a string literal names but no task declares only warns (exit 0). In every\n\
        graph, each tracked file beside a task's declared\n\
        inputs must be declared, except documents, licences and CI configuration; and a\n\
        declared link to a directory needs every file under it declared. Neither the Cargo\n\
        files by name nor the sibling rule asks for the JSON contract being validated or\n\
        its sykli.lock (an include of either is still asked for). It does not check\n\
        that declared inputs exist, and it runs no task (a sykli.rs emitter is run, to read\n\
        the contract it emits).\n\n\
        An env value may name ${worktree_cache} (see `sykli run --help`); any other ${...},\n\
        an unclosed one, a $${, $worktree_cache without braces in an env value, or\n\
        ${worktree_cache} anywhere but an env value is invalid.\n\
        These rules apply where a contract is written or run, not where `verify` or\n\
        `status --commit` judge a contract that an earlier version ran.")]
    Validate {
        /// Path to a sykli-contract.v1 JSON file or a sykli.rs emitter (sykli.json when only it exists); may be outside the repository
        #[arg(default_value = "sykli.rs")]
        contract: PathBuf,
        /// Print the verdict as JSON (`sykli-validate.v1`); exit 1 when invalid
        #[arg(long)]
        json: bool,
    },
    /// Execute a declared contract and record a receipt
    #[command(after_help = "Exit codes:\n  \
        0  every task passed or was cached\n  \
        1  a task failed, errored, or was blocked\n  \
        2  could not evaluate: unreadable contract, lock drift, undeclared Cargo inputs, no git or sh,\n     \
           or the run's journal could not be written (nothing unrecorded is reported as passed)\n  \
        128+N  stopped by signal N (SIGINT 130, SIGTERM 143): running tasks were stopped and\n     \
           recorded as interrupted, whatever they exited with, and no receipt was written;\n     \
           `sykli resume --run ID` continues the run\n\n\
        The receipt is written to .sykli/receipts/rcpt_<sha256>.json; --json also prints it.\n\n\
        On Linux and macOS every run is durable: before any task starts, the run is recorded in\n\
        $(git rev-parse --git-common-dir)/sykli/runs/<run-id>/, and each task's attempts\n\
        (dispatched, started, finished or interrupted, with its output) as they happen; the\n\
        run id is printed first (`run: ID`, on stderr with --json) and is the receipt's `run`.\n\
        `sykli status --run ID` reads that record at any time, also after a crash: a task\n\
        whose end was not recorded is lost, never passed. Each task runs in its own process\n\
        group. Elsewhere runs are not durable and nothing changes.\n\n\
        A contract outside the repository judges the git repository around the current\n\
        directory: its inputs are paths from that repository's root, its lock is the\n\
        sykli.lock beside the contract, and the receipt still goes to the repository's .sykli/.\n\
        Files beside it that a task reads are declared in the task's contract_inputs (paths\n\
        from the contract's directory) and named as $SYKLI_CONTRACT_DIR/PATH: a directory of\n\
        the task's own holding exactly those files, as its key and receipt record them,\n\
        removed when the task ends. A task that names the contract's directory exits 2.\n\n\
        A task's declared env values may name ${worktree_cache}: a directory that belongs to\n\
        this worktree, <git-common-dir>/sykli/worktrees/<id>/cache, outside the work tree,\n\
        made when a run needs it; a worktree made again at the same path gets a new one.\n\
        For a Rust contract, declare both of these in the task's \"env\":\n  \
          \"CARGO_TARGET_DIR\": \"${worktree_cache}/cargo-target\"\n  \
          \"CARGO_BUILD_BUILD_DIR\": \"${worktree_cache}/cargo-build\"\n\
        so that no gate runs a binary or test that another worktree built (a Cargo without\n\
        build-dir ignores the second); the first gate in each worktree builds cold. Cache\n\
        keys cover the value as declared, never the directory. On Linux and macOS a run\n\
        holds the directory, and a later run removes it once its worktree is gone\n\
        (removed, moved or pruned, and not locked). Any other ${...} in an env value, an\n\
        unclosed one, a $${, $worktree_cache without braces in an env value, or\n\
        ${worktree_cache} anywhere but an env value exits 2.")]
    Run {
        /// Path to a sykli-contract.v1 JSON file or a sykli.rs emitter (sykli.json when only it exists); may be outside the repository
        #[arg(default_value = "sykli.rs")]
        contract: PathBuf,
        /// Print the run receipt as JSON on stdout; progress and task output go to stderr
        #[arg(long)]
        json: bool,
        /// Record the run, hand it to a coordinator that outlives this command, and print
        /// its id once the record is durable (`{"schema":"sykli-run.v1","run":ID}` with
        /// --json); follow it with `sykli status --run ID` (Linux and macOS)
        #[arg(long)]
        detach: bool,
    },
    /// Select the tasks a change requires
    #[command(after_help = "Exit codes:\n  \
        0  plan printed (all tasks when no --changed is given)\n  \
        2  could not evaluate the contract or --explain found an input error\n\n\
        A contract outside the repository plans for the git repository around the current\n\
        directory; --changed paths are read from where the command is typed.")]
    Plan {
        /// Contract path, which may be outside the repository (--explain defaults to sykli.json; otherwise sykli.rs, falling back to sykli.json)
        contract: Option<PathBuf>,
        /// Changed file path; repeat for multiple files
        #[arg(long)]
        changed: Vec<PathBuf>,
        /// Print the plan as JSON
        #[arg(long)]
        json: bool,
        /// Explain selection and current cache evidence without running tasks (JSON contracts only)
        #[arg(long)]
        explain: bool,
        /// Coverage comparison base (default HEAD); does not filter task selection
        #[arg(long, requires = "explain")]
        base: Option<String>,
        /// Preview an edited JSON contract despite lock drift; never write or accept the lock
        #[arg(long, requires = "explain")]
        preview: bool,
    },
    /// Inspect a graph run (--run), or what applies to a commit (--commit)
    #[cfg(unix)]
    #[command(after_help = "Exit codes:\n  \
        0  records inspected (work may be unfinished)\n  \
        2  unknown run, unreadable journal, or no such commit\n\n\
        --run ID reads a run's journal (sykli-run-status.v1): its state (requested, running,\n\
        completed, interrupted, or lost when its coordinator ended without concluding it),\n\
        its subject and contract, each task's state (pending, running, passed, failed,\n\
        errored, cached, blocked, interrupted, lost) with every attempt, and its receipt once\n\
        complete. A run also keeps a copy of its receipt in its own directory\n\
        (receipt.kept), which outlives the worktree that ran it: `verify KEPT --commit REV`\n\
        judges it from any worktree of the clone. A task whose end was not recorded is\n\
        never passed. No git runs.\n\n\
        --commit REV judges every receipt of REV's tree that this clone keeps (this worktree's\n\
        .sykli/receipts, those the pre-push hook kept, and the receipts of runs, where they\n\
        were written or as the run kept them) as `verify --commit` does\n\
        (sykli-commit-status.v1). Receipts that verify for REV now are `current`; the others\n\
        of the same tree (another contract, a failed check) are `historical`, kept and shown\n\
        but never current evidence. Runs of that tree are listed with their states and each\n\
        task's attempts. The verdict never lets a pass hide a failure: the newest result its\n\
        run recorded decides, and a pass no run recorded never outranks a recorded failure\n\
        (`conflict`, with why). Like `verify`, it does not establish that tasks ran.")]
    Status {
        /// A graph run's id, as `run` printed it or its receipt's `run` names it
        #[arg(long, required_unless_present = "commit", conflicts_with = "commit")]
        run: Option<String>,
        /// Receipts and runs that apply to this commit, judged as `verify --commit` judges them
        #[arg(long, value_name = "REV")]
        commit: Option<String>,
        /// The contract to judge by, as for `verify`. Only with --commit: a run
        /// (--run) records the contract it ran
        #[arg(
            long,
            default_value = "sykli.rs",
            requires = "commit",
            conflicts_with = "run"
        )]
        contract: PathBuf,
        /// Print one versioned JSON document on stdout (sykli-run-status.v1 with --run,
        /// sykli-commit-status.v1 with --commit); an error goes to stderr and exits 2
        #[arg(long)]
        json: bool,
    },
    /// Continue a graph run whose coordinator ended (--run)
    #[cfg(unix)]
    #[command(after_help = "Exit codes:\n  \
        0  the run concluded and passed\n  \
        1  the run concluded and a task failed\n  \
        2  unknown run, busy, or a run whose source, contract or runtime changed since it\n     \
           was requested\n  \
        128+N  stopped again by signal N\n\n\
        --run ID continues a graph run whose coordinator ended (crashed, killed, or stopped\n\
        on a signal) from its journal. Tasks with an accepted outcome are kept, failed ones\n\
        included. Every attempt left unfinished is recorded as lost, and every lost or\n\
        interrupted task is retried as a new attempt that supersedes the old one, which\n\
        stays in the history. It is refused while the run's coordinator or any of its\n\
        attempts' processes is alive (the attempt's lease is held, or the process group the\n\
        journal recorded for it still exists; --assume-ended overrides the group check), and\n\
        when the worktree's source, the contract, its lock or the runtime is not what the run\n\
        recorded. Prints the receipt like `run`.\n\n\
        A resumed task runs at least once, and possibly more than once: an attempt that was\n\
        lost or interrupted may have run to completion, and done whatever it does, before its\n\
        end went unrecorded. A resume warns, naming each task it runs again whose last\n\
        attempt had started.")]
    Resume {
        /// A graph run's id
        #[arg(long)]
        run: String,
        /// Take an attempt whose process group still exists as ended (lost), for when
        /// that group id now belongs to an unrelated process
        #[arg(long)]
        assume_ended: bool,
        /// Print the run receipt as JSON on stdout; progress and task output go to stderr
        #[arg(long)]
        json: bool,
    },
    #[cfg(unix)]
    #[command(name = "__run_coordinator", hide = true)]
    RunCoordinator {
        #[arg(long)]
        run: String,
        #[arg(long)]
        lease_fd: i32,
    },
    /// Detect the repository's ecosystems and write a declared graph
    #[command(after_help = "Exit codes:\n  \
        0  wrote the contract\n  \
        1  nothing to declare — no Cargo.toml, package.json, or go.mod — or, with\n     \
           --contract, no git repository around the current directory or no way to write\n  \
        2  the contract exists; pass --force to overwrite it\n\n\
        With --contract PATH outside the repository, the tasks are still inferred from\n\
        the git repository around the current directory, with paths from its root; the\n\
        contract and its sykli.lock are written to PATH's directory (created if needed)\n\
        and nothing in the repository is touched: no .sykli/, no .gitignore entry.")]
    Init {
        /// Where to write the sykli-contract.v1 JSON
        #[arg(default_value = "sykli.json")]
        path: PathBuf,
        /// Write the contract (and its lock) to this path, which may be outside the repository
        #[arg(long, value_name = "PATH", conflicts_with = "path")]
        contract: Option<PathBuf>,
        /// Overwrite an existing contract
        #[arg(long)]
        force: bool,
        /// Do not pin the written contract in sykli.lock
        #[arg(long = "no-lock")]
        no_lock: bool,
    },
    /// Pin the contract's hash in sykli.lock beside it (one entry per contract file)
    #[command(after_help = "Exit codes:\n  0  locked\n  2  could not read or hash the contract")]
    Lock {
        /// Path to a sykli-contract.v1 JSON file or a sykli.rs emitter (sykli.json when only it exists); may be outside the repository
        #[arg(default_value = "sykli.rs")]
        contract: PathBuf,
    },
    /// Check that a receipt is consistent with the current tree and contract
    #[command(after_help = "Exit codes (first failing stage decides):\n  \
        0  verified\n  \
        1  outcome or evidence failed — the work is bad or incomplete\n  \
        2  cannot verify — not a receipt, self-contradictory records, unreadable input, git or contract error\n  \
        3  tree or input mismatch — receipt is stale; re-run sykli\n     \
           (with --commit: not a run of that commit's tree or declared inputs,\n     \
           or a run that read an input the commit cannot show at its path)\n  \
        4  contract mismatch — contract drifted from the receipt; re-lock\n     \
           (with --commit: from the contract or lock in that commit;\n     \
           for a contract outside the repository: from it, or from its lock file)\n\n\
        Without --commit the subject is the checkout: its tree and declared inputs.\n\
        With --commit the subject is the commit, and no input is read from the\n\
        checkout; REV is resolved in the repository around the current directory.\n\
        A contract outside that repository is read from its file, with or without\n\
        --commit, and the receipt must have run with the sykli.lock beside it\n\
        (the receipt's lock_digest); each of its tasks' contract_inputs is read from\n\
        beside it too, and must be what the receipt records (exit 3).\n\
        With --commit each declared input is judged against the commit at its exact\n\
        path: the tree entry, bytes and execute bit the run recorded must be the\n\
        commit's (bytes as stored, or as a checkout of the commit converts them: line\n\
        endings, smudge filters, by the commit's .gitattributes and this repository's\n\
        configuration, including .git/info/attributes; needs git 2.40 or later), and\n\
        the records must hash to the receipt's inputs digest.\n\
        A symbolic link is judged with the declared input it names; a `..` in its\n\
        target may only leave the link's own directories. A receipt whose run read a\n\
        file where the commit has none (an ignored or untracked file, inside a\n\
        submodule, spelled differently from git) is refused: the commit cannot show\n\
        it. An input a task declares as an output is generated by the run: its bytes\n\
        are the receipt's claim, and only that task's own inputs are judged.\n\
        On any failure the first failing check is also printed to stderr as one line.\n\n\
        A verified receipt is consistent, not proven. Verify establishes a valid\n\
        schema, records consistent with each other and the contract, the contract's\n\
        identity, and the tree and every declared input the run did not generate,\n\
        read from the checkout or the commit, never from the receipt. It does not\n\
        establish where the receipt came from, who produced it, or that its tasks\n\
        ran: receipts are unsigned, so an outcome rewritten to passed, or a receipt\n\
        recomputed in full for a commit with consistent digests, verifies. A\n\
        converted input is bound only as the commit's attributes and this\n\
        repository's configuration convert it.\n\n\
        RECEIPT may be anywhere: the copy a run keeps in its own directory,\n\
        <git-common-dir>/sykli/runs/<run-id>/rcpt_<sha256>.json (`status --run` names it,\n\
        receipt.kept), is judged like any other, and outlives the worktree that ran it.")]
    Verify {
        /// Path to a sykli-receipt.v1 JSON file
        receipt: PathBuf,
        /// Path to sykli.rs or a sykli-contract.v1 JSON file; may be outside the repository
        #[arg(long, default_value = "sykli.rs")]
        contract: PathBuf,
        /// Judge the receipt as evidence for this commit instead of the checkout:
        /// the contract and lock are read from REV (from their files when the
        /// contract is outside the repository), the receipt's tree must be
        /// REV^{tree}, every declared input the run read must be what REV has at
        /// its exact path (entry, bytes and execute bit), and every task passed
        /// or cached (the run need not be clean)
        #[arg(long, value_name = "REV")]
        commit: Option<String>,
        /// Print the verdict as one JSON document (`sykli-verify.v1`): every check, the
        /// first failing stage, the exit code, and what the verdict establishes and does
        /// not, with the receipt's own outcome as `claimed_outcome`; the exit code is
        /// unchanged
        #[arg(long)]
        json: bool,
    },
    /// Git hook entrypoints that gate what is committed and pushed
    Hook {
        #[command(subcommand)]
        hook: Hook,
    },
}

#[derive(Subcommand)]
enum Hook {
    /// Refuse a commit unless the graph passes on exactly the tree being committed
    #[command(after_help = "Exit codes:\n  \
        0  the graph passed on the staged tree, or there is no sykli.json or sykli.rs\n  \
        1  refused: unstaged changes or untracked files, or the graph failed\n  \
        2  could not evaluate: no git repository, or a missing or unreadable contract\n\n\
        Runs from the repository root. A tracked file whose disk content differs\n\
        from the index, or an untracked file that is not ignored, would make the\n\
        run check a tree other than the commit's, so either one refuses the commit.\n\
        With --contract, that contract is run instead of the repository's own.")]
    PreCommit {
        /// Gate with this contract instead of the repository's sykli.rs or sykli.json,
        /// for example one kept outside the repository
        #[arg(long)]
        contract: Option<PathBuf>,
    },
    /// Refuse a push to the default branch without a passing receipt of the pushed commit's tree
    #[command(after_help = "Exit codes:\n  \
        0  nothing pushed to a gated branch, every such push has a receipt that\n     \
           passes `verify --commit`, or there is no sykli.json or sykli.rs\n  \
        1  refused: a push to a gated branch has no such receipt\n  \
        2  could not evaluate: no git repository, unreadable input, a missing --contract,\n     \
           or a --branch that is not a branch name\n\n\
        Reads git's pre-push lines on stdin: <local ref> <local sha> <remote ref> <remote sha>.\n\
        With --branch, the gated branches are exactly those named (refs/heads/BRANCH for\n\
        each), and nothing is guessed. Without it, the gated branch is the default branch:\n\
        the one refs/remotes/REMOTE/HEAD names, else main and master. When that HEAD\n\
        names a ref that does not exist (a renamed default branch, pruned), main and\n\
        master are gated and a note says so.\n\
        Deletions, tags and pushes to other branches pass. Receipts are looked for in this\n\
        worktree: .sykli/receipt.json and .sykli/receipts/*.json. Whether a push is gated\n\
        is decided from the commits: the pushed commit's sykli.json gates it, and a commit\n\
        without one is refused when the branch had one (push with --no-verify to drop it).\n\
        With --contract, that contract gates every push to a gated branch instead,\n\
        whatever the commits carry; one outside the repository is read from its file.\n\
        An accepted push's receipts are kept in the common git directory, as\n\
        sykli/receipts/rcpt_<sha256>.json, so they outlive the worktree; a failed copy only warns.\n\
        A kept receipt is evidence for its commit, not proof the remote has it (dry runs\n\
        reach this hook too), and readers must still run verify --commit on it.")]
    PrePush {
        /// Remote name, as git passes it
        remote: Option<String>,
        /// Remote URL, as git passes it (unused)
        url: Option<String>,
        /// Gate with this contract instead of the one each pushed commit carries,
        /// for example one kept outside the repository
        #[arg(long)]
        contract: Option<PathBuf>,
        /// Gate pushes to this branch (repeatable) instead of the remote's default
        /// branch; when given, exactly refs/heads/BRANCH for each is gated, and
        /// nothing is guessed from refs/remotes/REMOTE/HEAD
        #[arg(long = "branch", value_name = "BRANCH")]
        branches: Vec<String>,
    },
    /// Install thin git hooks that call `sykli hook`, shared by every worktree
    #[command(after_help = "Exit codes:\n  \
        0  installed, updated, or already current\n  \
        1  refused: a hook sykli did not write is in the way (see --force), or\n     \
           core.hooksPath sends git elsewhere\n  \
        2  not a git repository, or a hook could not be written\n\n\
        Writes $(git rev-parse --git-common-dir)/hooks/<name> as\n  \
        #!/bin/sh\n  \
        exec sykli hook <name> \"$@\"\n\
        so `sykli` must be on PATH wherever git runs. With neither flag, both hooks.")]
    Install {
        /// Install the pre-commit hook
        #[arg(long)]
        pre_commit: bool,
        /// Install the pre-push hook
        #[arg(long)]
        pre_push: bool,
        /// Replace a hook sykli did not write, or an edited one; the old file is kept as NAME.pre-sykli
        #[arg(long)]
        force: bool,
    },
}

/// `sykli-lock.v2`: one lock file per directory, one entry per contract file
/// in it. `sykli-lock.v1` (a single hash) is still read and applies to
/// whichever contract is loaded, as it always did.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LockFile {
    schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    contract_hash: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    contracts: BTreeMap<String, String>,
}

fn lock_key(contract: &Path) -> String {
    contract
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("sykli.json")
        .to_string()
}

#[derive(Serialize)]
struct PlanOutput {
    schema: &'static str,
    contract_hash: String,
    tasks: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    explanations: Option<Vec<TaskExplanation>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_coverage: Option<InputCoverage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    contract_preview: Option<ContractPreview>,
}

#[derive(Serialize)]
struct ContractPreview {
    pinned_hash: Option<String>,
    matches_lock: bool,
}

#[derive(Serialize)]
struct InputCoverage {
    base_commit: Option<String>,
    paths: Vec<CoveragePath>,
    cargo_missing_inputs: Vec<String>,
    /// Absent when empty, so complete graphs report what they always did.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    folder_missing_inputs: Vec<FolderInput>,
    /// Absent when empty: files under a directory a declared link leads to.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    link_missing_inputs: Vec<LinkInput>,
    /// Absent when empty: documents a string literal in a Rust source names
    /// that no task declares. They warn; they never make `refusal` refuse.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    named_inputs: Vec<NamedInput>,
}

impl InputCoverage {
    /// Why `run` would refuse this graph before executing it, if it would.
    fn refusal(&self) -> Option<String> {
        if !self.cargo_missing_inputs.is_empty() {
            Some(cargo_coverage_error(&self.cargo_missing_inputs))
        } else if !self.folder_missing_inputs.is_empty() {
            Some(folder_coverage_error(&self.folder_missing_inputs))
        } else if !self.link_missing_inputs.is_empty() {
            Some(link_coverage_error(&self.link_missing_inputs))
        } else {
            None
        }
    }
}

/// A Cargo graph: `Cargo.toml` at the root, and the graph declares it or a
/// task names `cargo`. It decides both the input-completeness check and
/// whether the toolchain is part of every task's runtime (sykli#64).
fn is_cargo_graph(contract: &Contract) -> Result<bool, String> {
    if !Path::new("Cargo.toml").is_file() {
        return Ok(false);
    }
    let names_cargo = contract.tasks.iter().any(|task| {
        task.run
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
            .any(|word| word == "cargo")
    });
    Ok(names_cargo || declared_inputs(contract)?.contains_key(&absolute(Path::new("Cargo.toml"))?))
}

/// What git lists for the undeclared-file checks, asked once for both. One
/// `git ls-files --stage` lists the index; for a Cargo graph the same process
/// also lists the untracked files that are not ignored (`--others`), which
/// `--stage` prints without a mode and object id. Nothing runs until a check
/// needs it.
///
/// The contract being run, when it is a JSON file, and the lock beside it
/// are what the run evaluates, bound by the contract hash and the lock
/// digest (sykli#73): the rules that ask for a file by its name (Cargo's) or
/// by its folder (the sibling check) never ask for them, wherever the
/// contract is kept and however its path is spelled (`evaluated`). What a
/// Rust source includes is still asked for, the contract too: it is
/// compiled into the build, and a task's key does not cover the contract.
/// So is every file under a declared link to a directory. An emitter
/// (`sykli.rs`) is a Rust source of its crate, which Cargo builds, and is
/// still asked for.
struct Inventory {
    cargo: bool,
    /// The contract and its lock, as given and as the listing spells them.
    evaluated: Vec<PathBuf>,
    listed: std::cell::OnceCell<Result<Listed, String>>,
}

struct Listed {
    root: PathBuf,
    cached: Vec<PathBuf>,
    others: Vec<PathBuf>,
}

impl Inventory {
    fn new(contract: &Contract, contract_path: &Path) -> Result<Self, String> {
        let mut named = vec![lock_path(contract_path)];
        let json = contract_path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
        if json {
            named.push(contract_path.to_path_buf());
        }
        let mut evaluated = Vec::new();
        for path in named {
            evaluated.push(absolute(&path)?);
            evaluated.extend(as_listed(&path));
        }
        Ok(Inventory {
            cargo: is_cargo_graph(contract)?,
            evaluated,
            listed: std::cell::OnceCell::new(),
        })
    }

    /// Whether `path`, as the listing spells it, is the contract being run
    /// or its lock.
    fn evaluated(&self, path: &Path) -> bool {
        self.evaluated.iter().any(|evaluated| evaluated == path)
    }

    fn listed(&self) -> Result<&Listed, String> {
        self.listed
            .get_or_init(|| {
                let root = coverage_root()?;
                let files = repo::files(&root, self.cargo)?;
                let spelled = |paths: Vec<String>| {
                    paths
                        .into_iter()
                        .map(|path| absolute(&root.join(path)))
                        .collect::<Result<Vec<_>, _>>()
                };
                Ok(Listed {
                    cached: spelled(files.cached)?,
                    others: spelled(files.others)?,
                    root,
                })
            })
            .as_ref()
            .map_err(Clone::clone)
    }
}

/// `path` spelled as the listing spells files: its directory resolved, below
/// the repository's top level as the current directory spells it
/// (`coverage_root`), so that a contract named through a link to the
/// repository (macOS's `/tmp`) is the same file. Its own name is kept: a
/// contract named by a link is pinned by the lock beside the link. None
/// outside the repository.
fn as_listed(path: &Path) -> Option<PathBuf> {
    let repository = repo::repository().ok()?;
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let resolved = located(directory)?;
    let below = resolved.strip_prefix(repository.resolved()?).ok()?;
    absolute(
        &repository
            .root_here()
            .ok()?
            .join(below)
            .join(path.file_name()?),
    )
    .ok()
}

/// A document a string literal in a Rust source names, which no task
/// declares. It warns, never refuses: a help or message text names files a
/// task never reads, so the name alone does not make it an input.
#[derive(Serialize, PartialEq, Eq, PartialOrd, Ord)]
struct NamedInput {
    path: String,
    named_at: String,
}

fn named_warning(named: &NamedInput) -> String {
    format!(
        "{} is named in {} but not declared; declare it if a task reads it",
        named.path, named.named_at
    )
}

/// The Cargo inputs no task declares, which refuse, and the documents a
/// source names that no task declares, which warn.
fn cargo_missing_inputs(
    contract: &Contract,
    inventory: &Inventory,
) -> Result<(Vec<String>, Vec<NamedInput>), String> {
    // This is checked on every invocation, before execution can reuse cached passes. Its
    // input is the current file inventory, not the contract's existing list.
    if !inventory.cargo {
        return Ok((Vec::new(), Vec::new()));
    }
    let listed = inventory.listed()?;
    let root = &listed.root;
    let state = root.join(".sykli");
    let declared = declared_inputs(contract)?;
    let files = listed
        .cached
        .iter()
        .chain(&listed.others)
        .filter(|path| !path.starts_with(&state))
        .collect::<BTreeSet<_>>();
    // Cargo's files by name and every listed file a listed Rust source
    // includes refuse (`cargo_inputs`, which `init` declares by), and so does
    // an include that names no knowable file; a document a string names warns.
    let shown = |path: &Path| {
        path.strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    };
    let mut sources = cargo_inputs::Sources::default();
    for path in &files {
        if path.extension().is_some_and(|ext| ext == "rs") && path.is_file() {
            let text = fs::read(path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            sources.read(path, &String::from_utf8_lossy(&text), &shown(path));
        }
    }
    if let Some(refusal) = sources.refusal() {
        return Err(refusal);
    }
    let mut wanted = BTreeSet::new();
    let mut named = BTreeSet::new();
    let declarable = |path: &Path| {
        fs::symlink_metadata(path).is_ok_and(|meta| !meta.is_dir())
            && !leads_out_of_the_repository(path)
    };
    for path in &files {
        let relative = path.strip_prefix(root).unwrap_or(path);
        // The contract being run and its lock are not asked for by name,
        // but are when a source includes them (sykli#73).
        let by_name = cargo_inputs::by_name(relative) && !inventory.evaluated(path);
        if by_name || sources.included().contains(*path) {
            wanted.insert((*path).clone());
        } else if cargo_inputs::docs(relative) && !declared.contains_key(*path) {
            if let Some(at) = sources.named_at(&shown(path)) {
                if declarable(path) {
                    named.insert(NamedInput {
                        path: shown(path),
                        named_at: at.to_string(),
                    });
                }
            }
        }
    }
    let mut missing = BTreeSet::new();
    for path in wanted {
        // A link to a directory outside the repository is never declarable.
        if declared.contains_key(&path) || leads_out_of_the_repository(&path) {
            continue;
        }
        match fs::symlink_metadata(&path) {
            // A submodule: a directory in the index, never a declarable file.
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                let relative = path.strip_prefix(root).unwrap_or(&path);
                missing.insert(relative.to_string_lossy().into_owned());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("cannot inspect {}: {error}", path.display())),
        }
    }
    Ok((missing.into_iter().collect(), named.into_iter().collect()))
}

/// Why `run` refuses this contract before any task, for files it leaves
/// undeclared (`cargo_missing_inputs`, `folder_missing_inputs`, then
/// `link_missing_inputs`), if it does, after warning of each document a
/// source names that no task declares. `validate` asks the same inside a git
/// repository, so a contract it accepts is one `run` starts.
fn undeclared_refusal(contract: &Contract, path: &Path) -> Result<Option<String>, String> {
    let inventory = Inventory::new(contract, path)?;
    let (missing, named) = cargo_missing_inputs(contract, &inventory)?;
    for named in &named {
        eprintln!("warning: {}", named_warning(named));
    }
    if !missing.is_empty() {
        return Ok(Some(cargo_coverage_error(&missing)));
    }
    let folders = folder_missing_inputs(contract, &inventory)?;
    if !folders.is_empty() {
        return Ok(Some(folder_coverage_error(&folders)));
    }
    let linked = link_missing_inputs(contract, &inventory)?;
    if !linked.is_empty() {
        return Ok(Some(link_coverage_error(&linked)));
    }
    Ok(None)
}

fn cargo_coverage_error(missing: &[String]) -> String {
    format!(
        "Cargo inputs are undeclared: {missing:?}; add them to the affected tasks and re-lock before running (no cached results used)"
    )
}

/// A file inside a folder a task reads that the task does not declare.
#[derive(Serialize, PartialEq, Eq, PartialOrd, Ord)]
struct FolderInput {
    path: String,
    task: String,
}

/// A tripwire for one pattern: a tracked file next to a declared input that
/// the task does not declare. A task that declares an input in a folder
/// likely reads that folder, so such a file can change its result without
/// changing its key: a test that read a whole fixture folder kept its cached
/// pass after a fixture was added. Every tracked file (the index, so a new
/// file counts once it is `git add`ed) directly inside a folder holding one
/// of a task's declared inputs must be one of that task's inputs too, for
/// Cargo and other graphs alike. Only the repository root is exempt; a task's
/// working directory is a folder like any other. Documents, licences and CI
/// configuration (`cargo_inputs::docs`) are never asked for, so a task that
/// reads a folder of Markdown fixtures declares them itself.
///
/// It does not catch files in new subfolders or in parent folders of an input
/// folder, anything in the repository root, untracked, ignored or generated
/// files, or anything outside the repository. Untracked files are left out so
/// editor and OS scratch files beside an input never refuse a run; the
/// pre-commit hook already refuses a commit while untracked files exist.
fn folder_missing_inputs(
    contract: &Contract,
    inventory: &Inventory,
) -> Result<Vec<FolderInput>, String> {
    let mut tasks = Vec::new();
    for task in &contract.tasks {
        let workdir = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
        let declared = task
            .inputs
            .iter()
            .map(|input| absolute(&workdir.join(input)))
            .collect::<Result<BTreeSet<_>, _>>()?;
        let folders = declared
            .iter()
            .filter_map(|path| path.parent())
            .map(Path::to_path_buf)
            .collect::<BTreeSet<_>>();
        if !folders.is_empty() {
            tasks.push((&task.name, declared, folders));
        }
    }
    if tasks.is_empty() {
        return Ok(Vec::new());
    }
    repo::repository().map_err(|_| {
        "not inside a git repository; sykli binds every receipt to a git tree".to_string()
    })?;
    let listed = inventory.listed()?;
    let root = &listed.root;
    for (_, _, folders) in &mut tasks {
        folders.remove(root);
    }
    let mut missing = BTreeSet::new();
    for file in &listed.cached {
        // Documents, licences and CI configuration are never asked for
        // (`cargo_inputs::docs`), so a commit that adds documents never
        // refuses a run; nor are the contract being run and its lock.
        if file.starts_with(root.join(".sykli"))
            || cargo_inputs::docs(file.strip_prefix(root).unwrap_or(file))
            || inventory.evaluated(file)
        {
            continue;
        }
        let Some(folder) = file.parent() else {
            continue;
        };
        for (name, declared, folders) in &tasks {
            // A link to a directory outside the repository is never
            // declarable.
            if !folders.contains(folder)
                || declared.contains(file)
                || leads_out_of_the_repository(file)
            {
                continue;
            }
            match fs::symlink_metadata(file) {
                // A submodule: a directory in the index, never a declarable file.
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => {
                    let relative = file.strip_prefix(root).unwrap_or(file);
                    missing.insert(FolderInput {
                        path: relative.to_string_lossy().replace('\\', "/"),
                        task: (*name).clone(),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("cannot inspect {}: {error}", file.display())),
            }
        }
    }
    Ok(missing.into_iter().collect())
}

fn folder_coverage_error(missing: &[FolderInput]) -> String {
    const SHOWN: usize = 10;
    let mut listed = missing
        .iter()
        .take(SHOWN)
        .map(|input| format!("{} (task {})", input.path, input.task))
        .collect::<Vec<_>>();
    if missing.len() > SHOWN {
        listed.push(format!("and {} more", missing.len() - SHOWN));
    }
    format!(
        "tracked files next to a task's declared inputs that the task does not declare: {}; add each to its task's inputs (or move it out of that folder) and re-lock before running (no cached results used)",
        listed.join(", ")
    )
}

/// Where a symbolic link leads, for the rules about declared links.
enum Behind {
    /// A directory in the repository, as a path from its top level.
    Directory(PathBuf),
    /// Out of the repository: a file, a directory, or nothing there.
    Outside(PathBuf),
    /// Not a symbolic link, or one to a file in the repository, or to
    /// nothing in it.
    Other,
}

fn link_behind(path: &Path) -> Behind {
    let linked = fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink());
    let root = repo::repository()
        .ok()
        .and_then(|repository| repository.resolved());
    let Some(root) = root.filter(|_| linked) else {
        return Behind::Other;
    };
    let target = match fs::canonicalize(path) {
        Ok(target) => target,
        // Nothing there (or a loop): where its names lead, from the link's
        // own directory as resolved.
        Err(_) => {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let named = fs::read_link(path)
                .ok()
                .zip(fs::canonicalize(parent).ok())
                .and_then(|(text, directory)| absolute(&directory.join(text)).ok());
            let Some(named) = named else {
                return Behind::Other;
            };
            named
        }
    };
    match target.strip_prefix(root) {
        Ok(inside) if target.is_dir() => Behind::Directory(inside.to_path_buf()),
        Ok(_) => Behind::Other,
        Err(_) => Behind::Outside(target),
    }
}

/// Whether `path` is a symbolic link that leads out of the repository, to a
/// file, a directory or nothing there: no commit holds what it leads to, so
/// it is never a declarable input, and the rules that ask for files beside
/// declared ones, or for Cargo's, never ask for it.
fn leads_out_of_the_repository(path: &Path) -> bool {
    matches!(link_behind(path), Behind::Outside(_))
}

/// Where a declared path leads, every symbolic link on its way followed.
enum Leads {
    /// Inside the repository, or there is no repository to judge.
    Inside,
    /// Out of it: where a link on the way, or the path's end, leads.
    Outside(PathBuf),
    /// Through more than `LINK_HOPS` links: a loop, or as good as one.
    Endless,
}

/// How many symbolic links one declared path may lead through, as Linux's
/// `MAXSYMLINKS`.
const LINK_HOPS: usize = 40;

/// Where `path` leads (sykli#54): `ext/file.txt` with `ext` a link to a
/// directory elsewhere, an output `out` whose link leads to a file
/// elsewhere that the run has yet to make, or `hop` linked to `out`. It is
/// resolved as the kernel resolves it, part by part from the resolved top
/// level, following every link, chained or not, also one that leads to
/// nothing yet, by its text. Wherever a link's text leads, once resolved,
/// must be inside the repository, and so must the end; a chain of more than
/// `LINK_HOPS` links is `Endless`.
fn leads(path: &Path) -> Leads {
    enum Part {
        Root(OsString),
        Up,
        Name(OsString),
        /// A link's text is resolved: where it led is judged here.
        LinkEnd,
    }
    let parts = |path: &Path| {
        path.components()
            .filter_map(|component| match component {
                Component::Prefix(_) | Component::RootDir => {
                    Some(Part::Root(component.as_os_str().to_owned()))
                }
                Component::ParentDir => Some(Part::Up),
                Component::Normal(name) => Some(Part::Name(name.to_owned())),
                Component::CurDir => None,
            })
            .collect::<Vec<_>>()
    };
    let Ok(repository) = repo::repository() else {
        return Leads::Inside;
    };
    let (Ok(top), Some(root), Ok(path)) = (
        repository.root_here(),
        repository.resolved(),
        absolute(path),
    ) else {
        return Leads::Inside;
    };
    // Outside unless it is the root or below it, spelled as resolved (a
    // link's text may spell the same place another way, as Windows does).
    let outside = |place: &Path| {
        !place.starts_with(root) && !located(place).is_some_and(|place| place.starts_with(root))
    };
    let Ok(below) = path.strip_prefix(&top) else {
        return match located(&path).filter(|resolved| outside(resolved)) {
            Some(target) => Leads::Outside(target),
            None => Leads::Inside,
        };
    };
    let mut pending: VecDeque<Part> = parts(below).into();
    let mut resolved = root.to_path_buf();
    let mut hops = 0;
    while let Some(part) = pending.pop_front() {
        match part {
            Part::Root(root_part) => resolved.push(root_part),
            Part::Up => {
                resolved.pop();
            }
            Part::LinkEnd => {
                if outside(&resolved) {
                    return Leads::Outside(resolved);
                }
            }
            Part::Name(name) => {
                let next = resolved.join(name);
                match fs::read_link(&next) {
                    // A link: its text is resolved from its own directory,
                    // where `resolved` is, before the rest of the path.
                    Ok(text) => {
                        hops += 1;
                        if hops > LINK_HOPS {
                            return Leads::Endless;
                        }
                        pending.push_front(Part::LinkEnd);
                        for part in parts(&text).into_iter().rev() {
                            pending.push_front(part);
                        }
                    }
                    // Not a link, or nothing there yet.
                    Err(_) => resolved = next,
                }
            }
        }
    }
    if outside(&resolved) {
        Leads::Outside(resolved)
    } else {
        Leads::Inside
    }
}

/// Refuse a task whose working directory, declared input or declared output
/// a symbolic link on its way leads out of the repository (sykli#54). Such a
/// path is relative and climbs nowhere, so the contract's own check passes
/// it, yet a run would hash a file no commit holds into the key and the
/// receipt, or write an output, or work, outside. It is judged here, against
/// the work tree, before any task runs: a link a task makes while it runs is
/// not seen.
fn through_links_out(task: &Task) -> Result<(), String> {
    let workdir = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
    let mut paths = vec![(
        "works in",
        workdir.display().to_string(),
        workdir.to_path_buf(),
    )];
    for (what, declared) in [
        ("declares the input", &task.inputs),
        ("declares the output", &task.outputs),
    ] {
        paths.extend(
            declared
                .iter()
                .map(|path| (what, path.clone(), workdir.join(path))),
        );
    }
    for (what, declared, path) in paths {
        match leads(&path) {
            Leads::Inside => {}
            Leads::Outside(target) => {
                return Err(format!(
                    "task {:?} {what} {declared:?}, which a symbolic link on its way leads to {}, \
                     outside the repository: no commit holds what is there, so sykli cannot \
                     bind it and no receipt of it could verify; declare paths inside the \
                     repository instead, and re-lock",
                    task.name,
                    target.display()
                ));
            }
            Leads::Endless => {
                return Err(format!(
                    "task {:?} {what} {declared:?}, which leads through more than {LINK_HOPS} \
                     symbolic links (a loop): sykli cannot tell whether it stays inside or \
                     leads outside the repository, so it cannot bind it; declare paths inside \
                     the repository instead, and re-lock",
                    task.name
                ));
            }
        }
    }
    Ok(())
}

/// A file under the directory a task's declared symbolic link leads to,
/// which the task does not declare.
#[derive(Serialize, PartialEq, Eq, PartialOrd, Ord)]
struct LinkInput {
    path: String,
    task: String,
    link: String,
}

/// A declared symbolic link to a directory is recorded by its text
/// (`link_text`), so the files a task reads through it are in no key unless
/// the task declares them: every tracked file under that directory must be
/// one of the task's inputs too (`init` declares them). A link that leads out
/// of the repository, to a file, a directory or nothing there, leads where no
/// commit can show, which sykli cannot bind, so a contract that declares one
/// is refused.
fn link_missing_inputs(
    contract: &Contract,
    inventory: &Inventory,
) -> Result<Vec<LinkInput>, String> {
    let mut links = Vec::new();
    for task in &contract.tasks {
        let workdir = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
        let mut directories = Vec::new();
        for input in &task.inputs {
            match link_behind(&workdir.join(input)) {
                Behind::Directory(directory) => directories.push((input, directory)),
                Behind::Outside(target) => {
                    return Err(format!(
                        "task {:?} declares {input}, a symbolic link to {}, outside the \
                         repository: no commit holds what it leads to, so sykli cannot bind it \
                         and no receipt of it could verify; declare the files the task reads \
                         inside the repository instead, and re-lock",
                        task.name,
                        target.display()
                    ));
                }
                Behind::Other => {}
            }
        }
        through_links_out(task)?;
        if !directories.is_empty() {
            let declared = task
                .inputs
                .iter()
                .map(|input| absolute(&workdir.join(input)))
                .collect::<Result<BTreeSet<_>, _>>()?;
            links.push((&task.name, declared, directories));
        }
    }
    if links.is_empty() {
        return Ok(Vec::new());
    }
    let listed = inventory.listed()?;
    let root = &listed.root;
    let mut missing = BTreeSet::new();
    for file in &listed.cached {
        if file.starts_with(root.join(".sykli")) {
            continue;
        }
        let relative = file.strip_prefix(root).unwrap_or(file);
        for (task, declared, directories) in &links {
            if declared.contains(file) {
                continue;
            }
            for (link, directory) in directories {
                if !relative.starts_with(directory) {
                    continue;
                }
                match fs::symlink_metadata(file) {
                    // A submodule: a directory in the index, never a file.
                    Ok(meta) if meta.is_dir() => {}
                    Ok(_) => {
                        missing.insert(LinkInput {
                            path: relative.to_string_lossy().replace('\\', "/"),
                            task: (*task).clone(),
                            link: (*link).clone(),
                        });
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(format!("cannot inspect {}: {error}", file.display()));
                    }
                }
            }
        }
    }
    Ok(missing.into_iter().collect())
}

fn link_coverage_error(missing: &[LinkInput]) -> String {
    const SHOWN: usize = 10;
    let mut listed = missing
        .iter()
        .take(SHOWN)
        .map(|input| {
            format!(
                "{} (task {}, through {})",
                input.path, input.task, input.link
            )
        })
        .collect::<Vec<_>>();
    if missing.len() > SHOWN {
        listed.push(format!("and {} more", missing.len() - SHOWN));
    }
    format!(
        "tracked files under a directory that a task's declared symbolic link leads to, which the task does not declare: {}; a link to a directory is declared by its text, so declare each file under it in that task's inputs and re-lock before running (no cached results used)",
        listed.join(", ")
    )
}

#[derive(Serialize)]
struct CoveragePath {
    path: String,
    kind: &'static str,
    tasks: Vec<String>,
}

fn coverage_root() -> Result<PathBuf, String> {
    // Use the same current-directory spelling as declared paths. Git's absolute
    // root can expand Windows short names, making lexical comparisons disagree.
    repo::repository()?.root_here()
}

fn git_paths(repository: &Path, args: &[&str]) -> Result<Vec<PathBuf>, String> {
    let output = repo::git_command()
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into());
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            let path = std::str::from_utf8(path)
                .map_err(|_| "input coverage requires UTF-8 Git paths".to_string())?;
            absolute(&repository.join(path))
        })
        .collect()
}

fn declared_inputs(contract: &Contract) -> Result<BTreeMap<PathBuf, Vec<String>>, String> {
    let mut declared: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for task in &contract.tasks {
        let root = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
        for input in &task.inputs {
            let tasks = declared.entry(absolute(&root.join(input))?).or_default();
            if !tasks.contains(&task.name) {
                tasks.push(task.name.clone());
            }
        }
    }
    Ok(declared)
}

fn input_coverage(
    contract: &Contract,
    contract_path: &Path,
    changed: &[PathBuf],
    base: Option<&str>,
) -> Result<InputCoverage, String> {
    let repository = coverage_root()?;
    let inventory = Inventory::new(contract, contract_path)?;
    let base_commit = match base {
        Some(base) => Some(git(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{base}^{{commit}}"),
        ])?),
        None => match git(&["rev-parse", "--verify", "HEAD^{commit}"]) {
            Ok(commit) => Some(commit),
            Err(error) => {
                let branch = git(&["symbolic-ref", "--quiet", "HEAD"])?;
                let exists = repo::git_command()
                    .args(["show-ref", "--verify", "--quiet", &branch])
                    .output()
                    .map_err(|error| error.to_string())?;
                if exists.status.code() != Some(1) {
                    return Err(error);
                }
                None
            }
        },
    };
    let mut paths: BTreeSet<_> = match &base_commit {
        Some(base) => git_paths(
            &repository,
            &["diff", "--name-only", "--no-renames", "-z", base, "--"],
        ),
        None => git_paths(&repository, &["ls-files", "--cached", "-z"]),
    }?
    .into_iter()
    .collect();
    paths.extend(git_paths(
        &repository,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?);
    // Local receipts and cache are excluded from tree identity too. Explicit
    // hints below remain visible, including ignored files named by the caller.
    paths.retain(|path| !path.starts_with(repository.join(".sykli")));
    for path in changed {
        paths.insert(absolute(path)?);
    }
    let mut declared = declared_inputs(contract)?;
    let contract_path = absolute(contract_path)?;
    let lock_path = contract_path.with_file_name("sykli.lock");
    let paths = paths
        .into_iter()
        .map(|path| {
            let tasks = declared.remove(&path).unwrap_or_default();
            let kind = if !tasks.is_empty() {
                "declared_input"
            } else if path == contract_path || path == lock_path {
                "evaluation_metadata"
            } else {
                "unmapped"
            };
            let display = path.strip_prefix(&repository).unwrap_or(&path);
            Ok(CoveragePath {
                path: display
                    .to_str()
                    .ok_or("input coverage requires UTF-8 paths")?
                    .to_string(),
                kind,
                tasks,
            })
        })
        .collect::<Result<_, String>>()?;
    let (cargo_missing_inputs, named_inputs) = cargo_missing_inputs(contract, &inventory)?;
    Ok(InputCoverage {
        base_commit,
        paths,
        cargo_missing_inputs,
        named_inputs,
        folder_missing_inputs: folder_missing_inputs(contract, &inventory)?,
        link_missing_inputs: link_missing_inputs(contract, &inventory)?,
    })
}

#[derive(Serialize)]
struct TaskExplanation {
    task: String,
    selection: Vec<SelectionReason>,
    cache: CacheExplanation,
}

#[derive(Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
enum SelectionReason {
    AllTasks,
    ChangedInput { path: String },
    AffectedDependency { task: String },
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum CacheExplanation {
    Available {
        receipt: String,
    },
    Missing {
        code: CacheMiss,
    },
    Invalid {
        code: CacheMiss,
    },
    Deferred {
        dependencies: Vec<String>,
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        input_errors: BTreeMap<String, String>,
    },
    InputError {
        message: String,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum CacheMiss {
    EntryMissing,
    EntryUnreadable,
    EntryInvalid,
    ProvenanceInvalid,
    ArtifactMissing,
    ArtifactUnreadable,
    ArtifactInvalid,
}

impl std::fmt::Display for CacheMiss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::EntryMissing => "no entry for the current key",
            Self::EntryUnreadable => "cache entry cannot be read",
            Self::EntryInvalid => "cache entry is invalid",
            Self::ProvenanceInvalid => "source receipt is missing or invalid",
            Self::ArtifactMissing => "cached artifact is missing",
            Self::ArtifactUnreadable => "cached artifact cannot be read",
            Self::ArtifactInvalid => "cached artifact digest does not match",
        })
    }
}

impl std::fmt::Display for CacheExplanation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Available { receipt } => write!(
                f,
                "available; receipt {receipt} (restoration not attempted)"
            ),
            Self::Missing { code } | Self::Invalid { code } => write!(f, "{code}"),
            Self::Deferred {
                dependencies,
                input_errors,
            } => {
                write!(
                    f,
                    "deferred until dependencies complete: {}",
                    dependencies.join(", ")
                )?;
                for (task, message) in input_errors {
                    write!(f, "; input error in {task}: {message}")?;
                }
                Ok(())
            }
            Self::InputError { message } => write!(f, "input error: {message}"),
        }
    }
}

fn explain_plan(
    contract: &Contract,
    contract_path: &Path,
    levels: &[Vec<usize>],
    changed: &[PathBuf],
    selected: &[String],
) -> Result<Vec<TaskExplanation>, String> {
    let repository = git(&["rev-parse", "--show-toplevel"])
        .map_err(|_| "plan --explain requires a git repository".to_string())?;
    let runtime = graph_runtime(contract)?;
    let contract_inputs = contract_input_records(contract, &contract_directory(contract_path)?);
    let cache = LocalCache::new(Path::new(&repository));
    let changed: HashSet<_> = changed
        .iter()
        .map(|path| absolute(path))
        .collect::<Result<_, _>>()?;
    let selected: HashSet<_> = selected.iter().map(String::as_str).collect();
    let mut stable_keys: BTreeMap<&str, String> = BTreeMap::new();
    let mut explanations = Vec::new();
    let mut errors_by_task: HashMap<&str, BTreeMap<String, String>> = HashMap::new();
    for &index in levels.iter().flatten() {
        let task = &contract.tasks[index];
        // Inspect omitted ancestors too: --changed filters the display, not execution.
        let dependencies: Vec<_> = task
            .after
            .iter()
            .filter(|name| !stable_keys.contains_key(name.as_str()))
            .cloned()
            .collect();
        let mut input_errors: BTreeMap<String, String> = task
            .after
            .iter()
            .flat_map(|name| errors_by_task[name.as_str()].iter())
            .map(|(name, message)| (name.clone(), message.clone()))
            .collect();
        let state = if !dependencies.is_empty() {
            CacheExplanation::Deferred {
                dependencies,
                input_errors: input_errors.clone(),
            }
        } else {
            let after = task
                .after
                .iter()
                .map(|name| (name.as_str(), stable_keys[name.as_str()].as_str()))
                .collect();
            // Only a run makes the inherit key; with none yet, nothing is
            // cached under it, and explaining never makes it.
            let unkeyed = if task.inherit.is_empty() {
                Ok(false)
            } else {
                made_inherit_key().map(|key| key.is_none())
            };
            match unkeyed.and_then(|unkeyed| {
                (!unkeyed)
                    .then(|| cache_key(task, &runtime, &after, &contract_inputs))
                    .transpose()
            }) {
                Ok(None) => CacheExplanation::Missing {
                    code: CacheMiss::EntryMissing,
                },
                Err(message) => CacheExplanation::InputError { message },
                Ok(Some(key)) => match cache.inspect(task, &key, &runtime.fingerprint) {
                    Ok(entry) => {
                        // Restoring outputs can change downstream inputs, even on a cache hit.
                        if task.outputs.is_empty() {
                            stable_keys.insert(task.name.as_str(), key);
                        }
                        CacheExplanation::Available {
                            receipt: entry.receipt,
                        }
                    }
                    Err(code @ CacheMiss::EntryMissing) => CacheExplanation::Missing { code },
                    Err(code) => CacheExplanation::Invalid { code },
                },
            }
        };
        if let CacheExplanation::InputError { message } = &state {
            input_errors.insert(task.name.clone(), message.clone());
        }
        errors_by_task.insert(task.name.as_str(), input_errors);
        if !selected.contains(task.name.as_str()) {
            continue;
        }
        let mut selection = Vec::new();
        if changed.is_empty() {
            selection.push(SelectionReason::AllTasks);
        } else {
            let root = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
            for input in &task.inputs {
                if changed.contains(&absolute(&root.join(input))?) {
                    selection.push(SelectionReason::ChangedInput {
                        path: input.clone(),
                    });
                }
            }
            for name in &task.after {
                if selected.contains(name.as_str()) {
                    selection.push(SelectionReason::AffectedDependency { task: name.clone() });
                }
            }
        }
        explanations.push(TaskExplanation {
            task: task.name.clone(),
            selection,
            cache: state,
        });
    }
    Ok(explanations)
}

/// The contract a command works on when none was named. Every command
/// normally defaults to `sykli.rs`, the emitter; `plan --explain` bypasses
/// this fallback and defaults directly to JSON. A repository set up by `sykli init`
/// has `sykli.json` and no emitter. When the default was not overridden and
/// only `sykli.json` exists, that is the contract. An explicit path is never
/// touched.
fn resolve_contract(contract: PathBuf) -> PathBuf {
    if contract == Path::new("sykli.rs") && !contract.exists() && Path::new("sykli.json").exists() {
        return PathBuf::from("sykli.json");
    }
    contract
}

/// The repository a contract outside it judges: the git toplevel of the
/// current directory, when `contract` lies outside it. A person can keep a
/// repository's contract in their own configuration instead of committing
/// it; its inputs are still paths in the repository it judges. None when the
/// contract is inside that repository, or there is no repository, which is
/// how every command has always worked. Symbolic links are resolved first,
/// so a link in the repository to a contract elsewhere is outside it.
fn outside_repository(contract: &Path) -> Option<PathBuf> {
    let repository = repo::repository().ok()?;
    if located(contract)?.starts_with(repository.resolved()?) {
        return None;
    }
    // Spelled from the current directory, as declared paths are compared
    // (`coverage_root`), so paths made absolute before entering agree with
    // them: Git's own spelling can expand Windows short names.
    repository.root_here().ok()
}

/// Where `path` is on disk with symbolic links resolved, even before it
/// exists: its nearest existing ancestor resolved, the rest appended.
fn located(path: &Path) -> Option<PathBuf> {
    let path = absolute(path).ok()?;
    path.ancestors().find_map(|ancestor| {
        let resolved = fs::canonicalize(ancestor).ok()?;
        let rest = path.strip_prefix(ancestor).ok()?;
        // Joining nothing would end the path in a separator.
        Some(if rest.as_os_str().is_empty() {
            resolved
        } else {
            resolved.join(rest)
        })
    })
}

/// Work from the repository a contract outside it judges. Paths given on
/// the command line are made absolute first, so they keep meaning what they
/// meant where the command was typed; then the toplevel becomes the current
/// directory, the root every declared path resolves against. Nothing changes
/// for a contract inside the repository.
fn enter_for_contract(contract: &mut PathBuf, paths: &mut [&mut PathBuf]) -> Result<(), String> {
    let Some(root) = outside_repository(contract) else {
        return Ok(());
    };
    *contract = absolute(contract)?;
    for path in paths {
        **path = absolute(path)?;
    }
    std::env::set_current_dir(&root)
        .map_err(|error| format!("cannot enter the repository at {}: {error}", root.display()))
}

fn main() -> ExitCode {
    // Rust ignores SIGPIPE, which turns `sykli plan --json | head` into a
    // panic on the first write after the reader leaves. Restore the default
    // so a closed pipe ends the process quietly, as it does for other tools.
    #[cfg(unix)]
    // SAFETY: SIGPIPE (13) with SIG_DFL (0); no handler code runs.
    unsafe {
        journal::signal(13, 0);
    }
    let mut cli = Cli::parse();
    if let (Some(root), Some(common_dir)) = (cli.repo_root.take(), cli.git_common_dir.take()) {
        if let Err(error) = repo::give(root, common_dir) {
            eprintln!("error: {error}");
            return ExitCode::from(2);
        }
    }
    match &mut cli.command {
        Command::Plan {
            contract, explain, ..
        } => {
            let path = contract
                .take()
                .unwrap_or_else(|| PathBuf::from(if *explain { "sykli.json" } else { "sykli.rs" }));
            *contract = Some(if *explain {
                path
            } else {
                resolve_contract(path)
            });
        }
        Command::Validate { contract, .. }
        | Command::Run { contract, .. }
        | Command::Lock { contract, .. }
        | Command::Verify { contract, .. } => {
            *contract = resolve_contract(std::mem::take(contract));
        }
        _ => {}
    }
    let entered = match &mut cli.command {
        Command::Validate { contract, .. }
        | Command::Run { contract, .. }
        | Command::Lock { contract } => enter_for_contract(contract, &mut []),
        Command::Verify {
            receipt, contract, ..
        } => enter_for_contract(contract, &mut [receipt]),
        Command::Plan {
            contract: Some(contract),
            changed,
            ..
        } => enter_for_contract(contract, &mut changed.iter_mut().collect::<Vec<_>>()),
        // A hook enters the repository root itself; the contract it was
        // given keeps meaning what it meant where git ran the hook.
        Command::Hook {
            hook:
                Hook::PreCommit {
                    contract: Some(contract),
                }
                | Hook::PrePush {
                    contract: Some(contract),
                    ..
                },
        } => absolute(contract).map(|path| *contract = path),
        _ => Ok(()),
    };
    if let Err(error) = entered {
        eprintln!("error: {error}");
        return ExitCode::from(match cli.command {
            Command::Validate { .. } => 1,
            _ => 2,
        });
    }
    match cli.command {
        #[cfg(unix)]
        Command::Status {
            run: Some(run),
            json,
            ..
        } => match runs::status(&run) {
            Ok(status) => {
                if json {
                    println!("{status}");
                } else {
                    println!("{}", describe_run(&status));
                }
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(2)
            }
        },
        #[cfg(unix)]
        Command::Status {
            commit,
            contract,
            json,
            ..
        } => {
            // clap requires --run or --commit, so a status without --run has --commit.
            let Some(rev) = commit else {
                eprintln!("error: status takes --run ID or --commit REV");
                return ExitCode::from(2);
            };
            let mut contract = resolve_contract(contract);
            match enter_for_contract(&mut contract, &mut [])
                .and_then(|()| commit_status(&rev, &contract))
            {
                Ok(status) => {
                    if json {
                        println!("{status}");
                    } else {
                        println!("{}", describe_commit(&status));
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("error: {error}");
                    ExitCode::from(2)
                }
            }
        }
        #[cfg(unix)]
        Command::Resume {
            run,
            assume_ended,
            json,
        } => match resume_run(&run, assume_ended, json) {
            Ok(ran) => exit_for(ran, json),
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(2)
            }
        },
        #[cfg(unix)]
        Command::RunCoordinator { run, lease_fd } => match coordinator_main(&run, lease_fd) {
            Ok(ran) => exit_for(ran, false),
            Err(error) => {
                eprintln!("coordinator: {error}");
                ExitCode::from(2)
            }
        },
        Command::Validate { contract, json } => {
            match load(&contract)
                .and_then(|loaded| {
                    authored(&loaded.0)?;
                    placed(&loaded.0, &contract).map(|()| loaded)
                })
                .map_err(|error| (1, error))
                .and_then(|loaded| {
                    // What `run` refuses before any task for undeclared
                    // files, with `run`'s exit code (2), so a caller can
                    // tell it from an invalid or unpinned contract (1);
                    // outside a git repository there is no inventory to
                    // judge, and `run` cannot start at all.
                    if repo::repository().is_ok() {
                        match undeclared_refusal(&loaded.0, &contract) {
                            Ok(None) => {}
                            Ok(Some(refusal)) | Err(refusal) => return Err((2, refusal)),
                        }
                    }
                    Ok(loaded)
                }) {
                Ok((_, _, contract_hash)) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "schema": "sykli-validate.v1",
                                "contract": contract.display().to_string(),
                                "valid": true,
                                "contract_hash": contract_hash,
                                "errors": [],
                            })
                        );
                    } else {
                        println!("valid: {}", contract.display());
                    }
                    ExitCode::SUCCESS
                }
                Err((code, error)) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "schema": "sykli-validate.v1",
                                "contract": contract.display().to_string(),
                                "valid": false,
                                "contract_hash": null,
                                "errors": [error],
                            })
                        );
                    } else {
                        eprintln!("invalid {}: {error}", contract.display());
                    }
                    ExitCode::from(code)
                }
            }
        }
        Command::Run {
            contract: path,
            json,
            detach,
        } => match load_pinned(&path).and_then(|((contract, levels, hash), lock)| {
            run(&path, &contract, &levels, hash, lock, json, detach)
        }) {
            Ok(ran) => exit_for(ran, json),
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(2)
            }
        },
        Command::Hook { hook } => match hook {
            Hook::PreCommit { contract } => hook::pre_commit(contract.as_deref()),
            Hook::PrePush {
                remote,
                url: _,
                contract,
                branches,
            } => hook::pre_push(remote.as_deref(), contract.as_deref(), &branches),
            Hook::Install {
                pre_commit,
                pre_push,
                force,
            } => hook::install(pre_commit, pre_push, force),
        },
        Command::Plan {
            contract,
            changed,
            json,
            explain,
            base,
            preview,
        } => {
            let contract = contract.unwrap();
            if explain
                && !contract
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
            {
                eprintln!(
                    "plan --explain reads JSON contracts only; use sykli.json or create it with sykli init"
                );
                return ExitCode::from(2);
            }
            if explain && !contract.exists() {
                eprintln!(
                    "JSON contract {} does not exist; use sykli init to create one",
                    contract.display()
                );
                return ExitCode::from(2);
            }
            let loaded = if preview {
                load_unlocked(&contract)
            } else {
                load(&contract)
            };
            match loaded.and_then(|(loaded, levels, hash)| {
                authored(&loaded)?;
                placed(&loaded, &contract)?;
                let tasks = affected(&loaded, &levels, &changed)?;
                let mut explanations = if explain {
                    Some(explain_plan(&loaded, &contract, &levels, &changed, &tasks)?)
                } else {
                    None
                };
                let contract_preview = if preview {
                    let pinned_hash = read_lock(&contract)?;
                    Some(ContractPreview {
                        matches_lock: pinned_hash.as_ref() == Some(&hash),
                        pinned_hash,
                    })
                } else {
                    None
                };
                let input_coverage = if explain {
                    Some(input_coverage(
                        &loaded,
                        &contract,
                        &changed,
                        base.as_deref(),
                    )?)
                } else {
                    None
                };
                if let Some(message) = input_coverage.as_ref().and_then(InputCoverage::refusal) {
                    for item in explanations.iter_mut().flatten() {
                        item.cache = CacheExplanation::InputError {
                            message: message.clone(),
                        };
                    }
                }
                Ok(PlanOutput {
                    schema: "sykli-plan.v1",
                    contract_hash: hash,
                    tasks,
                    explanations,
                    input_coverage,
                    contract_preview,
                })
            }) {
                Ok(plan) => {
                    let input_error = plan
                        .input_coverage
                        .as_ref()
                        .is_some_and(|coverage| coverage.refusal().is_some())
                        || plan.explanations.as_ref().is_some_and(|items| {
                            items.iter().any(|item| match &item.cache {
                                CacheExplanation::InputError { .. } => true,
                                CacheExplanation::Deferred { input_errors, .. } => {
                                    !input_errors.is_empty()
                                }
                                _ => false,
                            })
                        });
                    if json {
                        if serde_json::to_writer(io::stdout().lock(), &plan).is_err() {
                            return ExitCode::from(2);
                        }
                        println!();
                    } else if let Some(explanations) = &plan.explanations {
                        if let Some(preview) = &plan.contract_preview {
                            println!(
                                "Contract preview only; matches lock: {}. Execution still enforces any existing lock.",
                                preview.matches_lock
                            );
                        }
                        for item in explanations {
                            println!("{}", item.task);
                            for reason in &item.selection {
                                match reason {
                                    SelectionReason::AllTasks => {
                                        println!("  selected: all tasks requested")
                                    }
                                    SelectionReason::ChangedInput { path } => println!(
                                        "  selected: declared input {path} matches --changed"
                                    ),
                                    SelectionReason::AffectedDependency { task } => {
                                        println!("  selected: affected dependency {task}")
                                    }
                                }
                            }
                            println!("  cache: {}", item.cache);
                        }
                        if let Some(coverage) = &plan.input_coverage {
                            if let Some(message) = coverage.refusal() {
                                println!("{message}");
                            }
                            println!(
                                "Input coverage against {} (plus --changed hints):",
                                coverage.base_commit.as_deref().unwrap_or("unborn HEAD")
                            );
                            for path in &coverage.paths {
                                match path.kind {
                                    "unmapped" => println!(
                                        "  warning: no task declares input {:?}; review dependencies before relying on cached results",
                                        path.path
                                    ),
                                    "evaluation_metadata" => {
                                        println!("  evaluation metadata: {:?}", path.path)
                                    }
                                    _ => println!(
                                        "  declared input: {:?} -> {}",
                                        path.path,
                                        path.tasks.join(", ")
                                    ),
                                }
                            }
                            for named in &coverage.named_inputs {
                                println!("  warning: {}", named_warning(named));
                            }
                            println!(
                                "Unmapped paths are advisory except for missing Cargo inputs and undeclared tracked siblings of declared inputs, which block execution and cache reuse."
                            );
                        }
                    } else {
                        for task in plan.tasks {
                            println!("{task}");
                        }
                    }
                    ExitCode::from(if input_error { 2 } else { 0 })
                }
                Err(error) => {
                    eprintln!("invalid {}: {error}", contract.display());
                    ExitCode::from(2)
                }
            }
        }
        Command::Init {
            path,
            contract,
            force,
            no_lock,
        } => {
            let Some(mut contract) = contract else {
                return init::run(&path, force, !no_lock, &write_lock);
            };
            if git(&["rev-parse", "--show-toplevel"]).is_err() {
                eprintln!(
                    "init --contract infers the tasks from the git repository around the current directory, and there is no git repository here"
                );
                return ExitCode::from(1);
            }
            // Inside the repository, it is the positional path by another name.
            if outside_repository(&contract).is_none() {
                return init::run(&contract, force, !no_lock, &write_lock);
            }
            if let Err(error) = enter_for_contract(&mut contract, &mut []) {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
            init::run_outside(&contract, force, !no_lock, &write_lock)
        }
        Command::Lock { contract } => match write_lock(&contract) {
            Ok(path) => {
                println!("locked: {}", path.display());
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(2)
            }
        },
        Command::Verify {
            receipt,
            contract,
            commit,
            json,
        } => {
            let verdict = verify(&receipt, &contract, commit.as_deref());
            if json {
                return report_json(&receipt, commit.as_deref(), verdict);
            }
            match verdict {
                Ok(checks) => report(&checks),
                Err(error) => {
                    eprintln!("error: {error}");
                    ExitCode::from(NOT_VERIFIABLE)
                }
            }
        }
    }
}

/// What `run` and `resume --run` exit with: 0 passed, 1 failed, 128+N
/// stopped by signal N; a detached run prints its id.
fn exit_for(ran: Ran, json: bool) -> ExitCode {
    match ran {
        Ran::Concluded { passed, .. } => {
            if passed {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Ran::Interrupted { run, signal } => {
            eprintln!(
                "interrupted: run {run} was stopped by signal {signal}; the tasks it stopped are \
                 recorded as interrupted, and `sykli resume --run {run}` continues it"
            );
            ExitCode::from(u8::try_from(128 + signal).unwrap_or(255))
        }
        Ran::Detached { run } => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"schema": "sykli-run.v1", "run": run})
                );
            } else {
                println!("run: {run}");
            }
            ExitCode::SUCCESS
        }
    }
}

/// `sykli resume --run`: take over a run whose coordinator ended, once its
/// worktree, contract, lock and runtime are shown to be what it recorded,
/// and decide what it left undecided.
#[cfg(unix)]
fn resume_run(run: &str, assume_ended: bool, json: bool) -> Result<Ran, String> {
    let coordinator = runs::Coordinator::reopen(run, assume_ended)?;
    let request = coordinator.request();
    let here = repo::repository()?;
    let same = |a: &Path, b: &Path| {
        fs::canonicalize(a)
            .ok()
            .is_some_and(|a| fs::canonicalize(b).ok() == Some(a))
    };
    if !same(&here.root, Path::new(&request.subject.repository)) {
        return Err(format!(
            "run {run} ran in the worktree at {}; resume it from there",
            request.subject.repository
        ));
    }
    std::env::set_current_dir(&request.workdir).map_err(|error| {
        format!(
            "cannot enter {}, where run {run} ran: {error}",
            request.workdir
        )
    })?;
    let (contract, levels, _) = parse_contract(request.contract.as_bytes())?;
    authored(&contract)?;
    let ((_, _, hash), lock) = load_pinned(Path::new(&request.contract_path))?;
    if hash != request.contract_hash || lock != request.lock_digest {
        return Err(format!(
            "the contract {} or its lock is not what run {run} recorded; start a new run",
            request.contract_path
        ));
    }
    let runtime = graph_runtime(&contract)?;
    if runtime.fingerprint != request.runtime {
        return Err(format!(
            "the runtime (shell, environment or toolchain) is not what run {run} recorded; \
             start a new run"
        ));
    }
    let subject = subject(&contract, Path::new(&request.contract_path))?;
    if subject.tree_oid != request.subject.tree_oid
        || subject.inputs_digest != request.subject.inputs_digest
    {
        return Err(format!(
            "the source changed since run {run} was requested (tree {}, now {}); its receipt \
             would claim a tree that did not produce it; start a new run",
            request.subject.tree_oid, subject.tree_oid
        ));
    }
    if subject.contract_inputs != request.subject.contract_inputs {
        return Err(format!(
            "a contract input beside the contract {} changed since run {run} was requested; \
             start a new run",
            request.contract_path
        ));
    }
    worktree_cache::prepare(&contract)?;
    coordinator.take_over()?;
    checkpoint("after-lost")?;
    // At least once, not exactly once: say which tasks may run twice.
    for (task, attempt) in coordinator.restarted() {
        eprintln!(
            "warning: re-running {task}: its attempt {attempt} started and may have run to \
             completion unobserved; a resumed task runs at least once, and possibly more than once"
        );
    }
    let cache = LocalCache::new(Path::new(&request.subject.repository));
    cache.bound();
    let progress = Progress::of(json);
    progress.line("run", run);
    coordinate(&coordinator, &contract, &levels, &runtime, &cache, progress)
}

/// A detached coordinator (`run --detach`): everything it needs is in the
/// run's record. One that cannot start its run records why.
#[cfg(unix)]
fn coordinator_main(run: &str, fd: i32) -> Result<Ran, String> {
    let coordinator = runs::Coordinator::received(run, fd)?;
    let request = coordinator.request();
    let started = (|| {
        let (contract, levels, _) = parse_contract(request.contract.as_bytes())?;
        authored(&contract)?;
        let runtime = graph_runtime(&contract)?;
        if runtime.fingerprint != request.runtime {
            return Err("the runtime changed before the run's coordinator started".to_string());
        }
        worktree_cache::prepare(&contract)?;
        Ok((contract, levels, runtime))
    })();
    let (contract, levels, runtime) = match started {
        Ok(started) => started,
        Err(error) => {
            coordinator.stop(None, &error)?;
            return Err(error);
        }
    };
    let cache = LocalCache::new(Path::new(&request.subject.repository));
    checkpoint("after-record")?;
    coordinate(
        &coordinator,
        &contract,
        &levels,
        &runtime,
        &cache,
        Progress::Quiet,
    )
}

/// A run's status for a person: its state, each task's, and its receipt.
#[cfg(unix)]
fn describe_run(status: &serde_json::Value) -> String {
    let mut lines = vec![format!(
        "run {}: {}",
        status["run"].as_str().unwrap_or("?"),
        status["state"].as_str().unwrap_or("?")
    )];
    for task in status["tasks"].as_array().into_iter().flatten() {
        let attempts = task["attempts"].as_array().map_or(0, Vec::len);
        lines.push(format!(
            "  {}: {}{}",
            task["name"].as_str().unwrap_or("?"),
            task["state"].as_str().unwrap_or("?"),
            if attempts > 1 {
                format!(" ({attempts} attempts)")
            } else {
                String::new()
            }
        ));
    }
    let receipt = &status["receipt"];
    match receipt["path"].as_str() {
        Some(path) if receipt["present"] == false => lines.push(format!("receipt: {path} (gone)")),
        Some(path) => lines.push(format!("receipt: {path}")),
        None => lines.push("receipt: none yet".into()),
    }
    if let Some(kept) = receipt["kept"]["path"].as_str() {
        if receipt["kept"]["present"] == true {
            lines.push(format!("kept: {kept}"));
        }
    }
    if matches!(status["state"].as_str(), Some("lost" | "interrupted")) {
        lines.push(format!(
            "continue it with: sykli resume --run {}",
            status["run"].as_str().unwrap_or("?")
        ));
    }
    lines.push("Use --json for every attempt.".into());
    lines.join("\n")
}

/// `sykli status --commit REV`: every receipt of REV's tree this clone
/// keeps, judged for REV by the code `verify --commit` uses, and the runs
/// of that tree (`sykli-commit-status.v1`). A receipt that verifies for REV
/// now, passed or failed, is current; every other receipt of that tree is
/// historical: kept and shown, never current evidence. Git is asked once
/// (one `cat-file --batch` for the commit, its contract and inputs), and
/// receipts are read from disk.
///
/// A current receipt is `recorded` when its run concluded with it in this
/// clone's journal (a pruned run's tombstone counts). The verdict follows
/// one rule: a failure is never hidden by a pass. The newest recorded result
/// for REV's tree and contract decides (a concluded failed run whose receipt
/// is gone counts too), so a rerun that passes after a failure passes, and a
/// failure after a pass fails. A result no run recorded has only the time it
/// claims: it never outranks a recorded failure, whatever that time, and a
/// failure it claims after the newest recorded pass is not ignored either;
/// either way the verdict is `conflict`, and `conflict` says why. With no
/// recorded result, the newest receipt decides.
#[cfg(unix)]
fn commit_status(rev: &str, contract_path: &Path) -> Result<serde_json::Value, String> {
    use serde_json::json;
    let repository = repo::repository()
        .map_err(|_| "not inside a git repository; --commit names a commit".to_string())?;
    let (judged, at) = commit_facts(repository, rev, contract_path)?;
    let (runs, unreadable) = runs::runs_of_tree(&at.tree)?;
    let mut candidates = vec![repository.root.join(".sykli/receipt.json")];
    for directory in [
        repository.root.join(".sykli/receipts"),
        repository.common_dir.join("sykli/receipts"),
    ] {
        if let Ok(entries) = fs::read_dir(directory) {
            candidates.extend(
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.extension().is_some_and(|ext| ext == "json")),
            );
        }
    }
    // A run's receipt where it was written, and the copy the run kept, which
    // outlives the worktree that wrote it; one receipt is judged once.
    candidates.extend(runs.iter().flat_map(|run| {
        [&run["receipt"]["path"], &run["receipt"]["kept"]["path"]]
            .into_iter()
            .filter_map(|path| path.as_str().map(PathBuf::from))
    }));
    let mut seen = HashSet::new();
    let mut current = Vec::new();
    let mut historical = Vec::new();
    for path in candidates {
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(receipt) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if receipt["subject"]["tree_oid"].as_str() != Some(at.tree.as_str()) {
            continue;
        }
        let digest = sha256(&bytes);
        if !seen.insert(digest.clone()) {
            continue;
        }
        let path = spelled(&path);
        // A receipt whose run this clone keeps (or pruned) counts only once
        // that run has concluded with it: a coordinator stopped, or fenced
        // off, between the receipt and its conclusion did not finish the
        // run. One whose run's journal cannot be read is not current either.
        let mut retried = Vec::new();
        let named = match receipt["run"].as_str() {
            Some(run) => Some((run, runs::known(run)?)),
            None => None,
        };
        let not_judged = match &named {
            Some((run, runs::Known::Kept(Ok(status)))) => {
                let state = status["state"].as_str().unwrap_or("?");
                if state != "completed" {
                    Some((
                        "unconcluded",
                        format!("its run {run} has not concluded with it (the run is {state})"),
                    ))
                } else if status["receipt"]["digest"] != digest.as_str() {
                    Some((
                        "unrecorded",
                        format!(
                            "its run {run} concluded with another receipt ({})",
                            status["receipt"]["digest"].as_str().unwrap_or("?")
                        ),
                    ))
                } else {
                    retried = status["tasks"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|task| task["attempts"].as_array().is_some_and(|a| a.len() > 1))
                        .map(|task| task["name"].clone())
                        .collect();
                    None
                }
            }
            Some((run, runs::Known::Kept(Err(error)))) => Some((
                "unconcluded",
                format!("its run {run}'s journal cannot be read: {error}"),
            )),
            Some((run, runs::Known::Pruned(note))) => {
                if note["receipt"]["digest"] == digest.as_str() {
                    None
                } else if note["state"] == "completed" {
                    Some((
                        "unrecorded",
                        format!("its run {run} concluded with another receipt, and was pruned"),
                    ))
                } else {
                    Some((
                        "unconcluded",
                        format!(
                            "its run {run} was pruned without concluding with it (it was {})",
                            note["state"].as_str().unwrap_or("?")
                        ),
                    ))
                }
            }
            Some((_, runs::Known::Unknown)) | None => None,
        };
        let recorded = not_judged.is_none()
            && matches!(
                named,
                Some((_, runs::Known::Kept(Ok(_)) | runs::Known::Pruned(_)))
            );
        let (judgement, stage, refusal_line) = match not_judged {
            Some((judgement, why)) => (judgement, None, Some(why)),
            None => match judge(&path, Ok((&judged, &at))) {
                Ok(checks) => match checks.iter().find(|check| !check.ok) {
                    None => ("verified", None, None),
                    Some(first) => (
                        match first.failure_code {
                            OUTCOME_FAILED => "failed",
                            NOT_VERIFIABLE => "unverifiable",
                            _ => "not-applicable",
                        },
                        Some(first.name),
                        Some(refusal(first)),
                    ),
                },
                Err(error) => ("unverifiable", None, Some(error)),
            },
        };
        let entry = json!({
            "receipt": path,
            "digest": digest,
            "run": receipt["run"],
            "started_at_ms": receipt["started_at_ms"],
            "outcome": receipt["outcome"],
            "judgement": judgement,
            "stage": stage,
            "refusal": refusal_line,
            "recorded": recorded,
            "retried": retried,
        });
        if matches!(judgement, "verified" | "failed") {
            current.push(entry);
        } else {
            historical.push(entry);
        }
    }
    let newest_first = |entries: &mut Vec<serde_json::Value>| {
        entries.sort_by_key(|entry| std::cmp::Reverse(entry["started_at_ms"].as_u64()));
    };
    newest_first(&mut current);
    newest_first(&mut historical);

    // Every result for REV: current receipts, and the failure of a run of
    // this tree and contract that concluded failed and whose receipt is gone.
    struct Evidence {
        passed: bool,
        recorded: bool,
        at: u64,
        what: String,
    }
    let mut evidence = current
        .iter()
        .map(|entry| Evidence {
            passed: entry["judgement"] == "verified",
            recorded: entry["recorded"] == true,
            at: entry["started_at_ms"].as_u64().unwrap_or(0),
            what: format!(
                "receipt {}",
                entry["digest"]
                    .as_str()
                    .unwrap_or("?")
                    .chars()
                    .take(12)
                    .collect::<String>()
            ),
        })
        .collect::<Vec<_>>();
    evidence.extend(
        runs.iter()
            .filter(|run| {
                run["state"] == "completed"
                    && matches!(run["outcome"].as_str(), Some("failed" | "errored"))
                    && run["contract_digest"] == judged.hash.as_str()
                    && run["receipt"]["present"] == false
                    // Its kept copy stands in only when it is current
                    // evidence itself.
                    && !current
                        .iter()
                        .any(|entry| entry["digest"] == run["receipt"]["digest"])
            })
            .map(|run| Evidence {
                passed: false,
                recorded: true,
                at: run["requested_at_ms"].as_u64().unwrap_or(0),
                what: format!(
                    "run {} (its receipt is gone)",
                    run["run"].as_str().unwrap_or("?")
                ),
            }),
    );
    // The newest, a failure on a tie.
    let newest = |recorded: bool| {
        evidence
            .iter()
            .filter(|result| result.recorded == recorded)
            .max_by_key(|result| (result.at, !result.passed))
    };
    let (verdict, conflict) = match newest(true) {
        Some(last) if last.passed => match evidence
            .iter()
            .find(|result| !result.recorded && !result.passed && result.at > last.at)
        {
            Some(failure) => (
                "conflict",
                Some(format!(
                    "{} failed, claiming a time after the pass {} recorded",
                    failure.what, last.what
                )),
            ),
            None => ("passed", None),
        },
        Some(last) => match evidence
            .iter()
            .find(|result| !result.recorded && result.passed)
        {
            Some(pass) => (
                "conflict",
                Some(format!(
                    "{} passed, but no run in this clone recorded it, and {} failed",
                    pass.what, last.what
                )),
            ),
            None => ("failed", None),
        },
        None => match newest(false) {
            Some(last) if last.passed => ("passed", None),
            Some(_) => ("failed", None),
            None if runs
                .iter()
                .any(|run| matches!(run["state"].as_str(), Some("requested" | "running"))) =>
            {
                ("pending", None)
            }
            None => ("none", None),
        },
    };
    let runs = runs
        .iter()
        .map(|run| {
            json!({
                "run": run["run"],
                "state": run["state"],
                "outcome": run["outcome"],
                "dirty": run["subject"]["dirty"],
                "receipt": run["receipt"],
                "tasks": run["tasks"].as_array().into_iter().flatten()
                    .map(|task| json!({
                        "name": task["name"],
                        "state": task["state"],
                        // Every attempt's outcome, oldest first: a pass that
                        // came only after a lost or interrupted attempt says so.
                        "attempts": task["attempts"].as_array().into_iter().flatten()
                            .map(|attempt| attempt["outcome"].as_str().unwrap_or("running"))
                            .collect::<Vec<_>>(),
                    }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "schema": "sykli-commit-status.v1",
        "commit": at.id,
        "tree": at.tree,
        "contract": {"path": contract_path, "hash": judged.hash},
        "verdict": verdict,
        "conflict": conflict,
        "current": current,
        "historical": historical,
        "runs": runs,
        "unreadable_runs": unreadable,
        "trust": TRUST,
    }))
}

/// `status --commit` for a person.
#[cfg(unix)]
fn describe_commit(status: &serde_json::Value) -> String {
    let short = |value: &serde_json::Value| {
        value
            .as_str()
            .map(|text| text.chars().take(12).collect::<String>())
            .unwrap_or_else(|| "?".into())
    };
    let mut lines = vec![format!(
        "commit {} (tree {}): {}",
        short(&status["commit"]),
        short(&status["tree"]),
        status["verdict"].as_str().unwrap_or("?")
    )];
    for (title, key) in [
        ("current", "current"),
        ("historical, not current", "historical"),
    ] {
        let entries = status[key].as_array().cloned().unwrap_or_default();
        if entries.is_empty() {
            continue;
        }
        lines.push(format!("{title}:"));
        for entry in entries {
            lines.push(format!(
                "  {} {} ({}){}",
                entry["judgement"].as_str().unwrap_or("?"),
                entry["receipt"].as_str().unwrap_or("?"),
                entry["outcome"].as_str().unwrap_or("?"),
                entry["refusal"]
                    .as_str()
                    .map(|refusal| format!(": {refusal}"))
                    .unwrap_or_default()
            ));
        }
    }
    for run in status["runs"].as_array().into_iter().flatten() {
        lines.push(format!(
            "run {}: {}",
            short(&run["run"]),
            run["state"].as_str().unwrap_or("?")
        ));
    }
    lines.join("\n")
}

/// `verify --json`: the verdict as one `sykli-verify.v1` document, with
/// what it establishes and what it does not; the exit code is `report`'s.
fn report_json(
    receipt: &Path,
    commit: Option<&str>,
    verdict: Result<Vec<Check>, String>,
) -> ExitCode {
    use serde_json::json;
    let (checks, error) = match verdict {
        Ok(checks) => (checks, None),
        Err(error) => (Vec::new(), Some(error)),
    };
    let first = checks.iter().find(|check| !check.ok);
    let exit = match (&error, first) {
        (Some(_), _) => NOT_VERIFIABLE,
        (None, Some(first)) => first.failure_code,
        (None, None) => 0,
    };
    // The tiers a passing verify establishes, each from its checks.
    let tier = |name: &str| match name {
        "schema" => "schema",
        "records" => "consistency",
        "contract lock" | "contract" | "lock" => "contract",
        "outcome" => "outcome",
        _ => "source",
    };
    let mut established = Vec::new();
    let mut not_established = Vec::new();
    for name in ["schema", "contract", "source", "consistency"] {
        let tier_checks = checks
            .iter()
            .filter(|check| tier(check.name) == name)
            .collect::<Vec<_>>();
        if !tier_checks.is_empty() && tier_checks.iter().all(|check| check.ok) {
            established.push(name);
        } else {
            not_established.push(name);
        }
    }
    // A receipt's outcome is its own claim: receipts are unsigned, and an
    // outcome rewritten with consistent digests verifies (`TRUST`). Its check
    // is reported, and decides the exit code, but establishes nothing.
    not_established.extend(["outcome", "provenance", "producer", "execution"]);
    // The receipt's own outcome, as it records it, for a caller that needs
    // it without reading the human `actual` text: its claim, which a passing
    // verify does not establish. Null when the receipt cannot be read or
    // records no outcome a receipt can have.
    let claimed_outcome = fs::read(receipt)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| serde_json::from_value::<Outcome>(value["outcome"].clone()).ok());
    if let Some(first) = first {
        eprintln!("refused: {}", refusal(first));
    }
    if let Some(error) = &error {
        eprintln!("error: {error}");
    }
    println!(
        "{}",
        json!({
            "schema": "sykli-verify.v1",
            "receipt": receipt,
            "subject": match commit {
                Some(rev) => json!({"kind": "commit", "rev": rev}),
                None => json!({"kind": "checkout"}),
            },
            "verified": exit == 0,
            "claimed_outcome": claimed_outcome,
            "exit": exit,
            "stage": first.map(|check| check.name),
            "refusal": first.map(refusal),
            "error": error,
            "checks": checks.iter().map(|check| json!({
                "name": check.name,
                "ok": check.ok,
                "expected": check.expected,
                "actual": check.actual,
            })).collect::<Vec<_>>(),
            "trust": {
                "established": established,
                "not_established": not_established,
                "statement": TRUST,
            },
        })
    );
    ExitCode::from(exit)
}

/// A contract, its execution levels, and its hash.
type Loaded = (Contract, Vec<Vec<usize>>, String);

fn load(path: &Path) -> Result<Loaded, String> {
    load_pinned(path).map(|(loaded, _)| loaded)
}

/// `load`, with the SHA-256 of the lock file it checked the contract against
/// (None when there is none), read once so a receipt names exactly the lock
/// that check used.
fn load_pinned(path: &Path) -> Result<(Loaded, Option<String>), String> {
    let loaded = load_unlocked(path)?;
    let (pin, digest) = read_lock_digest(path)?;
    if pin.is_some_and(|lock| lock != loaded.2) {
        return Err(format!(
            "contract differs from {}; run `sykli lock` to accept it",
            lock_path(path).display()
        ));
    }
    Ok((loaded, digest))
}

/// A lock file and the SHA-256 of its bytes, or None when there is none.
fn read_lock_file(lock_path: &Path) -> Result<Option<(LockFile, String)>, String> {
    if !lock_path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(lock_path).map_err(|error| error.to_string())?;
    let lock = parse_lock(&bytes, &lock_path.display().to_string())?;
    Ok(Some((lock, sha256(&bytes))))
}

fn parse_lock(bytes: &[u8], name: &str) -> Result<LockFile, String> {
    let lock: LockFile =
        serde_json::from_slice(bytes).map_err(|error| format!("invalid {name}: {error}"))?;
    match lock.schema.as_str() {
        "sykli-lock.v1" if lock.contract_hash.is_some() && lock.contracts.is_empty() => {}
        "sykli-lock.v2" if lock.contract_hash.is_none() => {}
        _ => return Err(format!("unsupported lock file {name}")),
    }
    Ok(lock)
}

/// An existing lock must pin the named contract; only an absent lock is unpinned.
fn pinned(lock: LockFile, contract: &Path) -> Result<String, String> {
    match lock.contract_hash {
        Some(hash) => Ok(hash),
        None => lock
            .contracts
            .get(&lock_key(contract))
            .cloned()
            .ok_or_else(|| {
                format!(
                    "{} does not pin {}; run `sykli lock` to accept it",
                    lock_path(contract).display(),
                    lock_key(contract)
                )
            }),
    }
}

/// The pinned hash for this contract, if its directory's lock file has one.
fn read_lock(path: &Path) -> Result<Option<String>, String> {
    read_lock_digest(path).map(|(pin, _)| pin)
}

/// The pinned hash for this contract, if its directory's lock file has one,
/// and the SHA-256 of that file, if there is one, from a single read.
fn read_lock_digest(path: &Path) -> Result<(Option<String>, Option<String>), String> {
    Ok(match read_lock_file(&lock_path(path))? {
        Some((lock, digest)) => (Some(pinned(lock, path)?), Some(digest)),
        None => {
            warn_unpinned_link(path);
            (None, None)
        }
    })
}

/// A contract named through a symbolic link is pinned by the lock beside the
/// link, as every contract is by the lock beside the path it is named by.
/// When there is none there but the link's target has one, the contract runs
/// unpinned, which is rarely meant: say so, once per command.
fn warn_unpinned_link(contract: &Path) {
    static WARNED: std::sync::Once = std::sync::Once::new();
    let linked = fs::symlink_metadata(contract).is_ok_and(|meta| meta.file_type().is_symlink());
    let Some(target) = linked.then(|| fs::canonicalize(contract).ok()).flatten() else {
        return;
    };
    if lock_path(&target).is_file() {
        WARNED.call_once(|| {
            eprintln!(
                "warning: {} links to {}, which has a sykli.lock beside it, but the lock is read \
                 beside the link, where there is none, so this contract runs unpinned; link the \
                 lock too, or name the target",
                contract.display(),
                target.display()
            )
        });
    }
}

fn load_unlocked(path: &Path) -> Result<(Contract, Vec<Vec<usize>>, String), String> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("");
    let bytes = if extension.eq_ignore_ascii_case("json") {
        fs::read(path).map_err(|error| error.to_string())?
    } else if extension == "rs" {
        emit_contract(path)?
    } else {
        return Err(format!(
            "{} is neither a .json contract nor a .rs emitter",
            path.display()
        ));
    };
    parse_contract(&bytes)
}

fn parse_contract(bytes: &[u8]) -> Result<(Contract, Vec<Vec<usize>>, String), String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let canonical = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
    let hash = sha256(&canonical);
    let mut parsed = CANONICAL_CONTRACTS.lock().unwrap();
    if !parsed.iter().any(|(known, _)| known == &hash) {
        parsed.push((hash.clone(), canonical));
    }
    drop(parsed);
    let contract: Contract = serde_json::from_value(value).map_err(|error| error.to_string())?;
    let levels = validate(&contract)?;
    Ok((contract, levels, hash))
}

fn lock_path(contract: &Path) -> PathBuf {
    contract
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join("sykli.lock")
}

fn write_lock(contract: &Path) -> Result<PathBuf, String> {
    let (loaded, _, contract_hash) = load_unlocked(contract)?;
    authored(&loaded)?;
    placed(&loaded, contract)?;
    let path = lock_path(contract);
    // A v1 lock could only ever pin one contract; migrating keeps that pin
    // under this contract's name and upgrades the file to v2.
    let mut contracts = read_lock_file(&path)?
        .map(|(lock, _)| lock.contracts)
        .unwrap_or_default();
    contracts.insert(lock_key(contract), contract_hash);
    let mut bytes = serde_json::to_vec_pretty(&LockFile {
        schema: "sykli-lock.v2".into(),
        contract_hash: None,
        contracts,
    })
    .map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    let temporary = path.with_file_name(format!(".sykli.lock.tmp-{}", std::process::id()));
    fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    if let Err(error) = fs::rename(&temporary, &path) {
        if path.exists() {
            fs::remove_file(&path).map_err(|error| error.to_string())?;
            fs::rename(&temporary, &path).map_err(|error| error.to_string())?;
        } else {
            return Err(error.to_string());
        }
    }
    Ok(path)
}

fn emit_contract(path: &Path) -> Result<Vec<u8>, String> {
    if !path.is_file() {
        return Err(format!(
            "contract emitter {} does not exist",
            path.display()
        ));
    }
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let output = ProcessCommand::new("cargo")
        .args([
            "run",
            "--quiet",
            "--features",
            "sykli",
            "--bin",
            "sykli",
            "--",
            "--emit",
        ])
        .current_dir(directory)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "contract emitter failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

/// Every contract path is repository-relative by construction: an input, output
/// or workdir is joined onto the root before it is read, written, or restored
/// from cache. A path that climbs out of that root, or names a root of its own,
/// reaches files the contract was never granted.
///
/// Rejecting `is_absolute()` alone is not enough. On Windows `C:build` is
/// drive-relative and reports `is_absolute() == false`, yet resolves against
/// that drive's current directory rather than the repository. Refusing the
/// prefix and root components covers every spelling on every platform, and
/// `components()` preserves `..` rather than resolving it, so a climb is still
/// visible here.
///
/// This is a check on what a contract may *declare*. A declared path that stays
/// inside the tree can still resolve outside it through a symlink; that is a
/// property of the work tree, not of the contract, and is refused where the
/// work tree is judged, before any task runs (`through_links_out`, sykli#54).
fn validate_contract_path(task: &str, field: &str, path: &Path) -> Result<(), String> {
    validate_path_within(task, field, path, "the repository")
}

/// `validate_contract_path` for a path from `root`: a contract input stays
/// inside the contract's directory as an input stays inside the repository.
fn validate_path_within(task: &str, field: &str, path: &Path, root: &str) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err(format!("task {task:?} declares an empty {field} path"));
    }
    for component in path.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "task {task:?} {field} path {:?} must stay inside {root}",
                    path.display().to_string()
                ));
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    Ok(())
}

/// What a contract must also meet where it is written or run (`validate`,
/// `lock`, `plan`, `run`, `resume`, the pre-commit hook), beyond `validate`:
/// placeholders sykli knows, only where it expands them. Never applied where
/// a contract is judged as evidence (`verify`, `status --commit`, the
/// pre-push hook), so a contract run before a rule existed keeps the meaning
/// it ran with.
fn authored(contract: &Contract) -> Result<(), String> {
    contract.tasks.iter().try_for_each(worktree_cache::check)
}

/// What a contract must also meet where it is written or run (`validate`,
/// `lock`, `plan`, `run`; a resumed or detached run runs what `run`
/// placed), given where it is (`path`); never where a contract is judged,
/// like `authored`. Only a contract outside the repository declares
/// `contract_inputs`: beside one inside it, files are in the repository and
/// declared as `inputs`. And no task of a contract outside it names the
/// contract's directory (as the path names it, or resolved) in its command
/// or an `env` value: sykli cannot hash what a task reads there by that
/// name, and a changed helper would reuse a stale pass (sykli#75). Declared
/// files are reached as `$SYKLI_CONTRACT_DIR/PATH`.
///
/// Only what the contract says is looked at, by a plain match on the two
/// spellings, in text read as the task's shell would spell its paths
/// (`spelled_out`). A path in the repository is not the directory's, even
/// when the directory holds the repository: repository inputs cover it.
/// Other spellings (a relative `../`, another link, another case, a value
/// the task inherits, `PATH`, a path a helper names) are not seen, as
/// nothing a task reads outside its declared files is.
///
/// With no git repository around the current directory there is no inside
/// or outside to judge, and nothing can run: nothing here applies.
fn placed(contract: &Contract, path: &Path) -> Result<(), String> {
    let Ok(repository) = repo::repository() else {
        return Ok(());
    };
    if outside_repository(path).is_none() {
        return match contract
            .tasks
            .iter()
            .find(|task| !task.contract_inputs.is_empty())
        {
            Some(task) => Err(format!(
                "task {:?} declares contract_inputs, files beside a contract outside the git \
                 repository around the current directory, and {} is not outside it: declare \
                 the files it reads in the repository as inputs",
                task.name,
                path.display()
            )),
            None => Ok(()),
        };
    }
    let directory = contract_directory(path)?;
    let resolved = fs::canonicalize(&directory).ok();
    // Where the directory holds the repository: the repository's root from
    // it, so each spelling of the directory gives one of the root.
    let below = resolved
        .as_deref()
        .zip(repository.resolved())
        .and_then(|(directory, root)| root.strip_prefix(directory).ok())
        .map(|below| below.to_string_lossy().replace('\\', "/"));
    let slashed = |path: &Path| path.to_string_lossy().replace('\\', "/");
    let mut spellings = vec![slashed(&directory)];
    spellings.extend(resolved.as_deref().map(slashed));
    for task in &contract.tasks {
        let home = task
            .env
            .get("HOME")
            .cloned()
            .or_else(|| std::env::var("HOME").ok());
        for text in std::iter::once(&task.run).chain(task.env.values()) {
            let text = spelled_out(text, home.as_deref());
            let named = spellings.iter().find(|spelling| {
                let root = below.as_ref().map(|below| format!("{spelling}/{below}"));
                names_directory(&text, spelling, root.as_deref())
            });
            if let Some(named) = named {
                return Err(format!(
                    "task {:?} names the directory of its contract, {named}, which is outside \
                     the repository: sykli cannot hash what a task reads there by that name; \
                     declare each file the task uses there in its contract_inputs and name it \
                     as \"$SYKLI_CONTRACT_DIR/PATH\"",
                    task.name
                ));
            }
        }
    }
    Ok(())
}

/// `text` with its paths spelled as a shell would reach them, as far as a
/// plain rewrite can: `${HOME}`, `$HOME` (not a longer name) and a `~` that
/// starts a word, alone or before `/`, read as `home`; then quotes removed,
/// as the shell's quote removal does (`"$HOME"/gate` is `$HOME/gate`), and a
/// `\` removed as an escape, or on Windows read as `/` (sh there takes
/// both); and `//` and `/./` read as `/`.
fn spelled_out(text: &str, home: Option<&str>) -> String {
    let mut text = text.to_string();
    if let Some(home) = home {
        let word = |c: char| c.is_alphanumeric() || c == '_';
        let mut out = String::with_capacity(text.len());
        let mut rest = text.as_str();
        while let Some(at) = rest.find(['$', '~']) {
            let (head, tail) = rest.split_at(at);
            out.push_str(head);
            let before = out.chars().next_back();
            let (taken, after) = if let Some(after) = tail.strip_prefix("${HOME}") {
                (true, after)
            } else if let Some(after) = tail
                .strip_prefix("$HOME")
                .filter(|after| !after.starts_with(word))
            {
                (true, after)
            } else if let Some(after) = tail.strip_prefix('~').filter(|after| {
                let starts = before.is_none_or(|c| c.is_whitespace() || "=:\"'(;&|".contains(c));
                starts
                    && after
                        .chars()
                        .next()
                        .is_none_or(|c| c == '/' || c.is_whitespace() || "\"';:&|)".contains(c))
            }) {
                (true, after)
            } else {
                (false, &tail[1..])
            };
            if taken {
                out.push_str(home);
            } else {
                out.push_str(&tail[..1]);
            }
            rest = after;
        }
        out.push_str(rest);
        text = out;
    }
    let mut text = text.replace(['"', '\''], "");
    text = if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text.replace('\\', "")
    };
    loop {
        let shorter = text.replace("/./", "/").replace("//", "/");
        if shorter == text {
            return text;
        }
        text = shorter;
    }
}

/// Whether `text` names `directory` or a path in it: the directory's path
/// not continued by more of a name on either side, so `/a/gate-old` and
/// `/x/a/gate` do not name `/a/gate`; a variable just before it (`$P/a/gate`)
/// may be empty, so it does not hide the path. A path in `root` (the
/// repository, when the directory holds it) is not the directory's, unless
/// it has a `..` in it (strictly: even one that stays inside).
fn names_directory(text: &str, directory: &str, root: Option<&str>) -> bool {
    let name = |c: char| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '~');
    let delimiter = |c: char| c.is_whitespace() || "\"'`;&|<>():".contains(c);
    text.match_indices(directory).any(|(at, _)| {
        let head = &text[..at];
        let variable = head
            .trim_end_matches(|c: char| c.is_ascii_alphanumeric() || c == '_')
            .ends_with('$');
        let before = head.chars().next_back();
        let after = text[at + directory.len()..].chars().next();
        if (before.is_some_and(|c| name(c) || c == '/') && !variable) || after.is_some_and(name) {
            return false;
        }
        // The path runs on from the directory (whose own spelling may hold
        // a delimiter, as `C:` does) to the end of the word.
        let path = &text[at..];
        let past = &path[directory.len()..];
        let path = &path[..directory.len() + past.find(delimiter).unwrap_or(past.len())];
        let in_root = root.is_some_and(|root| {
            path.strip_prefix(root)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        });
        !in_root || path.contains("/..")
    })
}

/// The directory a contract is in, absolute, as the path names it (a link
/// is not followed): where its lock is, and where its contract inputs are.
fn contract_directory(path: &Path) -> Result<PathBuf, String> {
    let path = absolute(path)?;
    path.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{} has no directory", path.display()))
}

/// Each task's contract inputs as the run reads them from `directory`, only
/// for the tasks that declare any.
fn contract_input_records(contract: &Contract, directory: &Path) -> InputRecords {
    contract
        .tasks
        .iter()
        .filter(|task| !task.contract_inputs.is_empty())
        .map(|task| {
            let records = task
                .contract_inputs
                .iter()
                .map(|input| (input.clone(), contract_input_record(&directory.join(input))))
                .collect();
            (task.name.clone(), records)
        })
        .collect()
}

/// One contract input as a run reads it: the record a repository input has,
/// without a tree entry (it is in no tree), or None when it is not a file
/// that can be read.
fn contract_input_record(path: &Path) -> Option<InputRecord> {
    path.is_file().then(|| input_record(path).ok()).flatten()
}

fn validate(contract: &Contract) -> Result<Vec<Vec<usize>>, String> {
    if contract.schema != "sykli-contract.v1" {
        return Err(format!("unsupported schema {:?}", contract.schema));
    }

    let mut names = HashMap::with_capacity(contract.tasks.len());
    for (index, task) in contract.tasks.iter().enumerate() {
        if task.name.trim().is_empty() {
            return Err("task name cannot be empty".into());
        }
        if task.run.trim().is_empty() {
            return Err(format!("task {:?} has an empty command", task.name));
        }
        if task
            .runtime
            .as_deref()
            .is_some_and(|runtime| runtime != "shell")
        {
            return Err(format!("task {:?} uses an unsupported runtime", task.name));
        }
        if let Some(workdir) = &task.workdir {
            validate_contract_path(&task.name, "workdir", workdir)?;
        }
        for input in &task.inputs {
            validate_contract_path(&task.name, "input", Path::new(input))?;
        }
        for output in &task.outputs {
            validate_contract_path(&task.name, "output", Path::new(output))?;
        }
        for input in &task.contract_inputs {
            validate_path_within(
                &task.name,
                "contract input",
                Path::new(input),
                "the contract's directory",
            )?;
        }
        if names.insert(task.name.as_str(), index).is_some() {
            return Err(format!("duplicate task name {:?}", task.name));
        }
        let mut inherited = HashSet::new();
        for name in &task.inherit {
            let valid = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
            if !valid {
                return Err(format!(
                    "task {:?} inherits an invalid variable name {name:?}",
                    task.name
                ));
            }
            if !inherited.insert(name.as_str()) || task.env.contains_key(name) {
                return Err(format!(
                    "task {:?} inherits {name:?} twice or alongside a declared env value",
                    task.name
                ));
            }
        }
    }

    let mut dependents = vec![Vec::new(); contract.tasks.len()];
    let mut pending = vec![0; contract.tasks.len()];
    for (index, task) in contract.tasks.iter().enumerate() {
        for dependency in &task.after {
            let Some(&dependency_index) = names.get(dependency.as_str()) else {
                return Err(format!(
                    "task {:?} depends on unknown task {dependency:?}",
                    task.name
                ));
            };
            dependents[dependency_index].push(index);
            pending[index] += 1;
        }
    }

    let mut ready: VecDeque<_> = pending
        .iter()
        .enumerate()
        .filter_map(|(index, &count)| (count == 0).then_some(index))
        .collect();
    let mut visited = 0;
    let mut levels = Vec::new();
    while !ready.is_empty() {
        let level: Vec<_> = ready.drain(..).collect();
        visited += level.len();
        for &index in &level {
            for &dependent in &dependents[index] {
                pending[dependent] -= 1;
                if pending[dependent] == 0 {
                    ready.push_back(dependent);
                }
            }
        }
        levels.push(level);
    }

    if visited != contract.tasks.len() {
        return Err("dependency cycle detected".into());
    }
    Ok(levels)
}

fn affected(
    contract: &Contract,
    levels: &[Vec<usize>],
    changed: &[PathBuf],
) -> Result<Vec<String>, String> {
    if changed.is_empty() {
        return Ok(levels
            .iter()
            .flatten()
            .map(|&index| contract.tasks[index].name.clone())
            .collect());
    }
    let changed: HashSet<_> = changed
        .iter()
        .map(|path| absolute(path))
        .collect::<Result<_, _>>()?;
    let names: HashMap<_, _> = contract
        .tasks
        .iter()
        .enumerate()
        .map(|(index, task)| (task.name.as_str(), index))
        .collect();
    let mut affected = vec![false; contract.tasks.len()];

    for level in levels {
        for &index in level {
            let task = &contract.tasks[index];
            let root = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
            affected[index] = task
                .inputs
                .iter()
                .map(|input| absolute(&root.join(input)))
                .collect::<Result<Vec<_>, _>>()?
                .iter()
                .any(|input| changed.contains(input))
                || task.after.iter().any(|name| affected[names[name.as_str()]]);
        }
    }

    Ok(levels
        .iter()
        .flatten()
        .filter(|&&index| affected[index])
        .map(|&index| contract.tasks[index].name.clone())
        .collect())
}

fn absolute(path: &Path) -> Result<PathBuf, String> {
    let path = if path.is_absolute() {
        path.into()
    } else {
        std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in path.components() {
        if component == std::path::Component::ParentDir {
            normalized.pop();
        } else if component != std::path::Component::CurDir {
            normalized.push(component);
        }
    }
    Ok(normalized)
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Outcome {
    Passed,
    Failed,
    Errored,
    Cached,
    Blocked,
}

#[derive(Serialize)]
struct Subject {
    repository: String,
    /// OID of the working tree content that actually ran, not of HEAD.
    tree_oid: String,
    inputs_digest: String,
    head_tree_oid: String,
    /// The tree that ran is not HEAD's, or a submodule had uncommitted changes.
    dirty: bool,
    /// A submodule had uncommitted changes, which its gitlink in `tree_oid`
    /// does not show. Written only when true, so other receipts are unchanged.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    submodules_modified: bool,
    /// The records `inputs_digest` was computed from, read once; each task's
    /// go into its own receipt record.
    #[serde(skip)]
    inputs: InputRecords,
    /// HEAD's commit when the run began, for a run's journal: a clean run
    /// ran exactly this commit's tree. Not part of the receipt.
    #[serde(skip)]
    #[cfg_attr(not(unix), allow(dead_code))]
    head_commit: Option<String>,
    /// The directory of the contract, where its contract inputs are.
    #[serde(skip)]
    contract_directory: PathBuf,
    /// The contract inputs as the run read them, once, by task (only tasks
    /// that declare any); each task's go into its own receipt record. They
    /// are in no tree, so the subject's digests do not cover them.
    #[serde(skip)]
    contract_inputs: InputRecords,
}

/// One task's record in a receipt. A durable run also keeps each one as the
/// output of the attempt (or reuse) that produced it, and builds the receipt
/// from those, so it round-trips: reading it back and writing it again gives
/// the same bytes.
#[derive(Clone, Serialize, Deserialize)]
struct TaskReceipt {
    name: String,
    command: String,
    runtime_fingerprint: String,
    exit_code: Option<i32>,
    /// The signal that ended the shell, where it was one (Unix). Kept in the
    /// run's journal, not in the receipt.
    #[serde(skip)]
    #[cfg_attr(not(unix), allow(dead_code))]
    signal: Option<i32>,
    /// When this task was witnessed running. On a cache hit this replays the
    /// original run's time rather than now — paired with `source: "cache"`, the
    /// receipt then says the evidence was witnessed then and reused now.
    /// Absent on receipts written before the field existed, and on tasks that
    /// never started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    started_at_ms: Option<u64>,
    duration_ms: u64,
    /// Lossy UTF-8 for readers; `stdout_digest` is over the raw bytes.
    stdout: String,
    stdout_truncated: bool,
    stdout_bytes_dropped: u64,
    stderr: String,
    stderr_truncated: bool,
    stderr_bytes_dropped: u64,
    stdout_digest: String,
    stderr_digest: String,
    output_digests: BTreeMap<String, String>,
    /// Keyed digests of inherited environment values, by name; `absent` when
    /// unset (`inherited_keyed_digests`). Receipts before it recorded a plain
    /// SHA-256 as `inherited_digests`, which nothing read and is no longer
    /// written: a guess at a short value could be checked against it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    inherited_keyed_digests: BTreeMap<String, String>,
    outcome: Outcome,
    importable: bool,
    class: Option<String>,
    retryable: bool,
    source: String,
    error: Option<String>,
    provenance: Option<String>,
    #[serde(skip)]
    cache_key: Option<String>,
    /// Each declared input as the run read it, recorded so `verify --commit`
    /// can compare every input with the commit; together they hash to the
    /// subject's `inputs_digest`. Graph runs only; absent from receipts
    /// written before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    inputs: Option<BTreeMap<String, Option<InputRecord>>>,
    /// Each declared contract input (a file beside a contract outside the
    /// repository) as the run read it, recorded so `verify` can compare it
    /// with the file there; no tree entry, since it is in no tree. Only on
    /// tasks that declare any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    contract_inputs: Option<BTreeMap<String, Option<InputRecord>>>,
}

#[derive(Serialize)]
struct Receipt {
    schema: &'static str,
    contract_hash: String,
    /// SHA-256 of the bytes of the lock file beside the contract (`sykli.lock`)
    /// as this run read it to check the contract; null when there was none.
    /// Receipts written before the field existed do not have it.
    lock_digest: Option<String>,
    /// When this run began. Always now, even when every task was reused: the
    /// run is what happened now, the evidence may be older.
    started_at_ms: u64,
    subject: Subject,
    tasks: Vec<TaskReceipt>,
    outcome: Outcome,
    /// The durable run that produced this receipt (`sykli status --run`),
    /// whose journal holds each task's attempts. Absent from receipts
    /// written before runs were journaled, and where they are not (Windows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    run: Option<String>,
}

struct ShellRuntime {
    path: PathBuf,
    fingerprint: String,
    environment: Vec<(OsString, OsString)>,
}

const MAX_CAPTURE_BYTES: usize = 1024 * 1024;

struct CapturedOutput {
    bytes: Vec<u8>,
    digest: String,
    bytes_dropped: u64,
    error: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheEntry {
    receipt: String,
    outputs: Vec<CacheOutput>,
    /// When the run this entry caches was witnessed. Replayed into a restored
    /// receipt so a cache hit still says when the evidence was produced, rather
    /// than claiming now. Absent on entries written before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    witnessed_at_ms: Option<u64>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheOutput {
    path: String,
    artifact: String,
    digest: String,
}

trait Cache {
    fn restore(&self, task: &Task, key: &str, runtime: &ShellRuntime) -> Option<TaskReceipt>;
    fn store(&self, task: &Task, record: &TaskReceipt, receipt: &str) -> Result<(), String>;
}

struct LocalCache {
    cache: PathBuf,
    receipts: PathBuf,
    /// Bytes the cache directory may hold before the least recently used
    /// entries are dropped.
    budget: u64,
    /// Receipts the directory may hold before the oldest unreferenced ones are
    /// dropped. Read once here rather than inside the prune so a test can set
    /// it without touching the environment: `set_var` is process-global and
    /// unsound beside the rest of a threaded test binary.
    receipt_budget: usize,
}

/// Default cache budget: 1 GiB.
///
/// A cache with no budget is not a cache, it is a leak with a lookup table. The
/// number is a judgement, not a measurement — this repository's own cache sits
/// under a megabyte across 238 entries, so the default is far above ordinary
/// use and exists to bound the pathological case rather than shape the common
/// one. Override with `SYKLI_CACHE_BUDGET_BYTES`; `0` disables eviction.
const DEFAULT_CACHE_BUDGET: u64 = 1024 * 1024 * 1024;

fn cache_budget() -> u64 {
    std::env::var("SYKLI_CACHE_BUDGET_BYTES")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(DEFAULT_CACHE_BUDGET)
}

/// Default receipt budget: 200 files.
///
/// Counted rather than sized because receipts are small and numerous — about
/// 8 KiB each here — so what accumulates is entries in a directory, not bytes.
/// Override with `SYKLI_RECEIPT_BUDGET`; `0` keeps every receipt forever.
const DEFAULT_RECEIPT_BUDGET: usize = 200;

fn receipt_budget() -> usize {
    std::env::var("SYKLI_RECEIPT_BUDGET")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(DEFAULT_RECEIPT_BUDGET)
}

/// Receipt filenames any surviving cache entry still cites.
///
/// A cached result is only evidence because the receipt it names can still be
/// read. Evicting one of these would turn a cache hit into a claim with nothing
/// behind it, so they are never counted against the budget and never removed.
fn referenced_receipts(cache: &Path) -> std::collections::HashSet<String> {
    let mut referenced = std::collections::HashSet::new();
    let Ok(entries) = fs::read_dir(cache) else {
        return referenced;
    };
    for entry in entries.flatten() {
        let Ok(bytes) = fs::read(entry.path().join("entry.json")) else {
            continue;
        };
        if let Ok(parsed) = serde_json::from_slice::<CacheEntry>(&bytes) {
            referenced.insert(parsed.receipt);
        }
    }
    referenced
}

/// Every immediate child of `directory` with its total size and the most recent
/// modification time anywhere beneath it.
///
/// Recency comes from the newest file in an entry rather than the directory
/// stamp: restoring from an entry does not reliably touch the directory, and an
/// entry being reused is exactly the one eviction must keep.
fn cache_entries(directory: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    fn walk(path: &Path, size: &mut u64, newest: &mut std::time::SystemTime) {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                walk(&entry.path(), size, newest);
            } else {
                *size += meta.len();
                if let Ok(modified) = meta.modified() {
                    if modified > *newest {
                        *newest = modified;
                    }
                }
            }
        }
    }

    let mut out = Vec::new();
    let Ok(children) = fs::read_dir(directory) else {
        return out;
    };
    for child in children.flatten() {
        if !child.path().is_dir() {
            continue;
        }
        let mut size = 0;
        let mut newest = std::time::SystemTime::UNIX_EPOCH;
        walk(&child.path(), &mut size, &mut newest);
        out.push((child.path(), size, newest));
    }
    out
}

/// How a run ended, for its caller.
enum Ran {
    /// Every task was decided and the receipt is durable: whether the run
    /// passed, and where its receipt is.
    Concluded { passed: bool, receipt: PathBuf },
    /// Stopped on a signal: the attempts sykli signalled are interrupted,
    /// and no receipt is written; `sykli resume --run` continues it.
    #[cfg_attr(not(unix), allow(dead_code))]
    Interrupted { run: String, signal: i32 },
    /// Handed to a detached coordinator once its record was durable.
    #[cfg_attr(not(unix), allow(dead_code))]
    Detached { run: String },
}

/// Where progress lines and task output go.
#[derive(Clone, Copy, PartialEq)]
enum Progress {
    /// Progress and task stdout to stdout, task stderr to stderr.
    Human,
    /// Everything to stderr, so stdout carries only the JSON document.
    Json,
    /// Nowhere: a detached coordinator has no terminal, and each task's
    /// captured output is kept with its attempt.
    #[cfg_attr(not(unix), allow(dead_code))]
    Quiet,
}

impl Progress {
    fn of(json: bool) -> Self {
        if json {
            Progress::Json
        } else {
            Progress::Human
        }
    }

    fn line(self, status: &str, subject: &str) {
        match self {
            Progress::Human => println!("{status}: {subject}"),
            Progress::Json => eprintln!("{status}: {subject}"),
            Progress::Quiet => {}
        }
    }

    /// Where a task's stdout is relayed.
    fn stdout(self) -> Box<dyn Write + Send> {
        match self {
            Progress::Human => Box::new(io::stdout()),
            Progress::Json => Box::new(io::stderr()),
            Progress::Quiet => Box::new(io::sink()),
        }
    }

    /// Where a task's stderr is relayed.
    fn stderr(self) -> Box<dyn Write + Send> {
        match self {
            Progress::Quiet => Box::new(io::sink()),
            _ => Box::new(io::stderr()),
        }
    }
}

/// Durable runs need the run lease (`flock`); on other platforms a run
/// keeps everything in memory until its receipt is written, as it always
/// did, and the commands that need a run's journal say so.
#[cfg_attr(unix, allow(dead_code))]
const DURABLE_RUNS_UNSUPPORTED: &str = "durable runs are unsupported on this platform (they need \
     Unix file leases): a run is kept in memory until its receipt is written, and --detach, \
     status --run and resume --run are unavailable";

/// A test hook, compiled into debug builds only, at each point where a
/// crash must leave the run's record true. `SYKLI_TEST_PAUSE_AT=POINT`
/// with `SYKLI_TEST_PAUSE_DIR=DIR` makes the process write its pid to
/// `DIR/POINT.pid` when it reaches POINT, then wait until `DIR/POINT.release`
/// (a FIFO) is written, so a test can kill it exactly there.
/// `SYKLI_TEST_FAIL_AT=POINT` makes the step at POINT fail, as a full disk
/// would. Release builds have neither.
fn checkpoint(point: &str) -> Result<(), String> {
    #[cfg(debug_assertions)]
    {
        if std::env::var_os("SYKLI_TEST_FAIL_AT").is_some_and(|at| at == point) {
            return Err(format!("injected failure at {point}"));
        }
        if std::env::var_os("SYKLI_TEST_PAUSE_AT").is_some_and(|at| at == point) {
            if let Some(dir) = std::env::var_os("SYKLI_TEST_PAUSE_DIR").map(PathBuf::from) {
                let pid = dir.join(format!("{point}.pid"));
                let partial = dir.join(format!(".{point}.pid.{}", std::process::id()));
                let _ = fs::write(&partial, std::process::id().to_string())
                    .and_then(|()| fs::rename(&partial, &pid));
                let _ = fs::read(dir.join(format!("{point}.release")));
            }
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = point;
    Ok(())
}

/// Run a contract: record the run durably before any task is dispatched
/// (Unix), decide every task, and write the receipt from what was accepted.
#[allow(clippy::too_many_arguments)]
fn run(
    contract_path: &Path,
    contract: &Contract,
    levels: &[Vec<usize>],
    contract_hash: String,
    lock_digest: Option<String>,
    json: bool,
    detach: bool,
) -> Result<Ran, String> {
    // Stamped before any task runs, so the receipt's run time brackets every
    // task time it contains.
    let run_started_at_ms = unix_time_ms();
    authored(contract)?;
    placed(contract, contract_path)?;
    #[cfg(not(unix))]
    if detach {
        return Err(DURABLE_RUNS_UNSUPPORTED.into());
    }
    if let Some(refusal) = undeclared_refusal(contract, contract_path)? {
        return Err(refusal);
    }
    let subject = subject(contract, contract_path)?;
    // Before the run is recorded: a directory that cannot be made or held
    // is a run that cannot be evaluated, not a task that errored.
    worktree_cache::prepare(contract)?;
    let runtime = graph_runtime(contract)?;
    let cache = LocalCache::new(Path::new(&subject.repository));
    cache.bound();
    let progress = Progress::of(json);
    #[cfg(unix)]
    {
        checkpoint("before-record")?;
        let request = runs::Request::new(
            canonical_contract(&contract_hash)?,
            contract_path,
            contract_hash,
            lock_digest,
            &subject,
            contract
                .tasks
                .iter()
                .map(|task| task.name.clone())
                .collect(),
            runtime.fingerprint.clone(),
            run_started_at_ms,
            detach,
        )?;
        let coordinator = runs::Coordinator::create(request)?;
        if detach {
            return coordinator.detach().map(|run| Ran::Detached { run });
        }
        progress.line("run", coordinator.id());
        checkpoint("after-record")?;
        coordinate(&coordinator, contract, levels, &runtime, &cache, progress)
    }
    #[cfg(not(unix))]
    {
        let _ = (contract_path, detach);
        let undecided = std::iter::repeat_with(|| None)
            .take(contract.tasks.len())
            .collect();
        let tasks = execute(
            contract, levels, &runtime, &cache, progress, None, &subject, undecided,
        )?
        .into_iter()
        .map(|record| record.ok_or("a task was left undecided"))
        .collect::<Result<Vec<_>, _>>()?;
        let receipt = Receipt {
            schema: "sykli-receipt.v1",
            contract_hash,
            lock_digest,
            started_at_ms: run_started_at_ms,
            subject,
            tasks,
            outcome: Outcome::Passed,
            run: None,
        };
        conclude(contract, receipt, None, &cache, progress)
    }
}

/// Decide every task of a recorded run that is not decided yet, then
/// conclude it, or record it stopped when a signal stopped it first. A run
/// that is resumed or detached comes here too: everything it needs is in
/// its record.
#[cfg(unix)]
fn coordinate(
    coordinator: &runs::Coordinator,
    contract: &Contract,
    levels: &[Vec<usize>],
    runtime: &ShellRuntime,
    cache: &LocalCache,
    progress: Progress,
) -> Result<Ran, String> {
    runs::handle_signals();
    let request = coordinator.request();
    let subject = request.subject();
    let decided = execute(
        contract,
        levels,
        runtime,
        cache,
        progress,
        Some(coordinator),
        &subject,
        coordinator.decided()?,
    )?;
    // A run that was signalled stops, even when every task got decided:
    // its caller asked it to, and a receipt now would answer a stop with a
    // verdict. The outcomes it accepted stay, and a resume concludes it.
    let mut signal = runs::signalled();
    if signal.is_none() && decided.iter().all(Option::is_some) {
        // The last task may have been stopped first, by a signal that
        // reaches sykli only now: wait out the window before concluding.
        coordinator.wait_out_stop_window();
        signal = runs::signalled();
    }
    if signal.is_some() || decided.iter().any(Option::is_none) {
        coordinator.withdraw_before_stop()?;
        coordinator.stop(signal, "stopped on a signal")?;
        return Ok(Ran::Interrupted {
            run: coordinator.id().into(),
            signal: signal.unwrap_or(15),
        });
    }
    // The receipt is built from the journal's accepted records alone.
    let tasks = coordinator
        .decided()?
        .into_iter()
        .map(|record| record.ok_or("a decided task has no accepted record"))
        .collect::<Result<Vec<_>, _>>()?;
    let receipt = Receipt {
        schema: "sykli-receipt.v1",
        contract_hash: request.contract_hash.clone(),
        lock_digest: request.lock_digest.clone(),
        started_at_ms: request.started_at_ms,
        subject,
        tasks,
        outcome: Outcome::Passed,
        run: Some(coordinator.id().into()),
    };
    conclude(contract, receipt, Some(coordinator), cache, progress)
}

/// Write the receipt (its outcome follows from its tasks), record the run
/// concluded, then store what can be cached. A run whose conclusion could
/// not be recorded is not reported as concluded.
fn conclude(
    contract: &Contract,
    mut receipt: Receipt,
    coordinator: Option<&runs::Coordinator>,
    cache: &LocalCache,
    progress: Progress,
) -> Result<Ran, String> {
    let tasks = &receipt.tasks;
    receipt.outcome = if tasks.iter().any(|task| task.outcome == Outcome::Errored) {
        Outcome::Errored
    } else if !tasks.is_empty() && tasks.iter().all(|task| task.outcome == Outcome::Cached) {
        Outcome::Cached
    } else if tasks
        .iter()
        .all(|task| matches!(task.outcome, Outcome::Passed | Outcome::Cached))
    {
        Outcome::Passed
    } else {
        Outcome::Failed
    };
    // Every task, executed or reused, records the inputs as this run read
    // them, the same records the subject's inputs digest covers, and the
    // contract inputs its key covers.
    for record in &mut receipt.tasks {
        record.inputs = receipt.subject.inputs.get(&record.name).cloned();
        record.contract_inputs = receipt.subject.contract_inputs.get(&record.name).cloned();
    }
    let (path, digest, bytes) = write_receipt(&receipt)?;
    checkpoint("after-receipt")?;
    if let Some(coordinator) = coordinator {
        coordinator.conclude(&path, &digest, &bytes, receipt.outcome)?;
    }
    let receipt_name = path.file_name().unwrap().to_string_lossy();
    for (task, record) in contract.tasks.iter().zip(&receipt.tasks) {
        if cacheable(record) {
            if let Err(error) = cache.store(task, record, &receipt_name) {
                eprintln!("cache write failed for {}: {error}", task.name);
            }
        }
    }
    // Once for the run, not once per task. Every task here cites the same
    // receipt, so the answer cannot change between them, and each call would
    // otherwise re-read and re-parse `entry.json` for every entry in the cache.
    cache.prune_receipts();
    match progress {
        Progress::Json => {
            serde_json::to_writer(io::stdout().lock(), &receipt)
                .map_err(|error| error.to_string())?;
            println!();
        }
        Progress::Human => println!("receipt: {}", path.display()),
        Progress::Quiet => {}
    }
    // What this run set aside when it was recorded (the directories of gone
    // worktrees), deleted only now: their size never delays a run's start.
    #[cfg(unix)]
    worktree_cache::remove_pruned();
    Ok(Ran::Concluded {
        passed: matches!(receipt.outcome, Outcome::Passed | Outcome::Cached),
        receipt: path,
    })
}

/// Contracts parsed in this process, by hash: the canonical JSON whose
/// SHA-256 the hash is, so a run can keep exactly the contract it ran.
static CANONICAL_CONTRACTS: std::sync::Mutex<Vec<(String, Vec<u8>)>> =
    std::sync::Mutex::new(Vec::new());

#[cfg_attr(not(unix), allow(dead_code))]
fn canonical_contract(hash: &str) -> Result<String, String> {
    CANONICAL_CONTRACTS
        .lock()
        .unwrap()
        .iter()
        .find(|(known, _)| known == hash)
        .and_then(|(_, bytes)| String::from_utf8(bytes.clone()).ok())
        .ok_or_else(|| format!("contract {hash} was not parsed by this process"))
}

/// Decide every task not yet decided (`decided`), level by level: blocked
/// behind a failed dependency, errored before it could run, reused from the
/// cache, or run as a new attempt. With a coordinator, each decision is
/// journaled as it is made, and an attempt is dispatched durably before its
/// shell is spawned. A signal stops dispatching; attempts it stopped stay
/// undecided (None). A journal that cannot be written is an error, never a
/// pass.
#[allow(clippy::too_many_arguments)]
fn execute(
    contract: &Contract,
    levels: &[Vec<usize>],
    runtime: &ShellRuntime,
    cache: &dyn Cache,
    progress: Progress,
    coordinator: Option<&runs::Coordinator>,
    subject: &Subject,
    mut decided: Vec<Option<TaskReceipt>>,
) -> Result<Vec<Option<TaskReceipt>>, String> {
    let names: HashMap<_, _> = contract
        .tasks
        .iter()
        .enumerate()
        .map(|(index, task)| (task.name.as_str(), index))
        .collect();
    let baseline = Baseline::new(contract, subject);

    for level in levels {
        if runs::signalled().is_some() {
            break;
        }
        let mut runnable = Vec::new();
        for &index in level {
            if decided[index].is_some() {
                continue;
            }
            let task = &contract.tasks[index];
            let settle = |record: TaskReceipt, key: Option<&str>| -> Result<TaskReceipt, String> {
                if let Some(coordinator) = coordinator {
                    coordinator.settle(&task.name, key, &record)?;
                }
                Ok(record)
            };
            if task.after.iter().any(|name| {
                !matches!(
                    decided[names[name.as_str()]].as_ref().map(|r| r.outcome),
                    Some(Outcome::Passed | Outcome::Cached)
                )
            }) {
                progress.line("blocked", &task.name);
                decided[index] = Some(settle(
                    empty_task_receipt(
                        task,
                        runtime,
                        Outcome::Blocked,
                        "dependency failed",
                        "dependency_failed",
                        false,
                    ),
                    None,
                )?);
                continue;
            }
            let after: BTreeMap<&str, &str> = task
                .after
                .iter()
                .map(|name| {
                    let key = decided[names[name.as_str()]]
                        .as_ref()
                        .and_then(|record| record.cache_key.as_deref())
                        .unwrap_or("");
                    (name.as_str(), key)
                })
                .collect();
            let errored = |error: &str, class: &'static str| {
                eprintln!("errored: {}", task.name);
                empty_task_receipt(task, runtime, Outcome::Errored, error, class, false)
            };
            // The run read its inputs once, for its subject; the task must
            // find them unchanged, or what it decides is about another tree.
            if let Some(changed) = baseline.drifted(task) {
                decided[index] = Some(settle(errored(&changed, "input_changed"), None)?);
                continue;
            }
            match cache_key(task, runtime, &after, &subject.contract_inputs) {
                Ok(key) => {
                    if let Some(mut record) = cache.restore(task, &key, runtime) {
                        progress.line("cached", &task.name);
                        record.cache_key = Some(key.clone());
                        if let Some(coordinator) = coordinator {
                            coordinator.reuse(&task.name, &key, &record)?;
                        }
                        decided[index] = Some(record);
                    } else {
                        runnable.push((index, key));
                    }
                }
                Err(error) => {
                    decided[index] = Some(settle(errored(&error, "input_error"), None)?);
                }
            }
        }

        let results = thread::scope(|scope| {
            runnable
                .into_iter()
                .map(|(index, key)| {
                    let task = &contract.tasks[index];
                    let baseline = &baseline;
                    (
                        index,
                        scope.spawn(move || {
                            attempt_task(task, key, runtime, progress, coordinator, baseline)
                        }),
                    )
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|(index, handle)| {
                    let result = handle
                        .join()
                        .unwrap_or_else(|_| Err("a task's thread panicked".to_string()));
                    (index, result)
                })
                .collect::<Vec<_>>()
        });

        let mut failure = None;
        for (index, result) in results {
            let task = &contract.tasks[index];
            match result {
                Ok(Some(record)) => {
                    match record.outcome {
                        Outcome::Passed => progress.line("passed", &task.name),
                        Outcome::Failed => eprintln!("failed: {}", task.name),
                        Outcome::Errored => eprintln!("errored: {}", task.name),
                        Outcome::Cached | Outcome::Blocked => unreachable!(),
                    }
                    decided[index] = Some(record);
                }
                Ok(None) => eprintln!("interrupted: {}", task.name),
                Err(error) => {
                    // The run could not record a step of it (or a test hook
                    // stopped it): not the task's outcome.
                    eprintln!("not recorded: {}: {error}", task.name);
                    failure.get_or_insert(error);
                }
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
    }
    Ok(decided)
}

/// Run one task as a new attempt: dispatched durably, its shell in its own
/// process group holding the attempt's lease, its start and end journaled,
/// and its record kept before it is accepted. None when sykli's stop
/// signalled its group: its outcome is unknown whatever it exited with (a
/// task that traps TERM and exits 0 never finished its work), and it is
/// recorded as interrupted, never passed.
#[allow(clippy::too_many_arguments)]
fn attempt_task(
    task: &Task,
    key: String,
    runtime: &ShellRuntime,
    progress: Progress,
    coordinator: Option<&runs::Coordinator>,
    baseline: &Baseline<'_>,
) -> Result<Option<TaskReceipt>, String> {
    let attempt = coordinator
        .map(|coordinator| coordinator.dispatch(&task.name, &key))
        .transpose()?;
    checkpoint("after-dispatch")?;
    let subject = baseline.subject;
    let declared = subject
        .contract_inputs
        .get(&task.name)
        .map(|records| (subject.contract_directory.as_path(), records));
    let launched = launch(
        task,
        runtime,
        progress,
        MAX_CAPTURE_BYTES,
        attempt.as_ref(),
        declared,
    )?;
    let (mut record, ended) = match launched {
        Launched::Ran { record, ended } => (*record, ended),
        Launched::Stopped { exit_code, signal } => {
            checkpoint("during-cancel")?;
            if let Some(attempt) = &attempt {
                attempt.interrupted(runs::signalled().unwrap_or(15), exit_code, signal)?;
            }
            return Ok(None);
        }
    };
    record.cache_key = Some(key);
    if let Some(changed) = baseline.drifted(task) {
        record.outcome = Outcome::Errored;
        record.importable = false;
        record.class = Some("input_changed".into());
        record.error = Some(changed);
    }
    if let Some(attempt) = &attempt {
        attempt.finished(&record, ended)?;
    }
    Ok(Some(record))
}

/// What a run's tasks must still find while it runs: the bytes of each
/// declared input as its subject recorded them, and each input's execute
/// bits as they were on disk when this coordinator began executing. A task
/// that read anything else would make the receipt claim a result for a tree
/// that did not produce it. Inputs another task declares as outputs are made
/// during the run, and are not compared.
struct Baseline<'a> {
    subject: &'a Subject,
    generated: HashSet<PathBuf>,
    modes: HashMap<PathBuf, Option<u32>>,
}

impl<'a> Baseline<'a> {
    fn new(contract: &Contract, subject: &'a Subject) -> Self {
        let generated = contract
            .tasks
            .iter()
            .flat_map(|task| {
                let root = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
                task.outputs
                    .iter()
                    .filter_map(move |output| absolute(&root.join(output)).ok())
            })
            .collect();
        let modes = contract
            .tasks
            .iter()
            .flat_map(|task| {
                let root = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
                task.inputs.iter().map(move |input| root.join(input))
            })
            .filter_map(|path| Some((absolute(&path).ok()?, execute_bits(&path))))
            .collect();
        Baseline {
            subject,
            generated,
            modes,
        }
    }

    /// The task's declared inputs that changed, as an error, or None.
    fn drifted(&self, task: &Task) -> Option<String> {
        let recorded = self.subject.inputs.get(&task.name)?;
        let root = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
        let changed = task
            .inputs
            .iter()
            .filter(|input| {
                let path = root.join(input);
                let Ok(full) = absolute(&path) else {
                    return true;
                };
                if self.generated.contains(&full) {
                    return false;
                }
                // Read as the record was: a link to no file by its text.
                let bytes = input_record(&path).ok().map(|record| record.sha256);
                let then = recorded
                    .get(*input)
                    .cloned()
                    .flatten()
                    .map(|record| record.sha256);
                bytes != then || self.modes.get(&full).copied().flatten() != execute_bits(&path)
            })
            .cloned()
            .chain(
                self.subject
                    .contract_inputs
                    .get(&task.name)
                    .into_iter()
                    .flatten()
                    .filter(|(input, record)| {
                        let path = self.subject.contract_directory.join(input);
                        contract_input_record(&path).as_ref() != record.as_ref()
                    })
                    .map(|(input, _)| format!("{input} (beside the contract)")),
            )
            .collect::<Vec<_>>();
        (!changed.is_empty()).then(|| {
            format!(
                "declared input changed while the run was in progress: {}; its result is not \
                 evidence for the tree the run recorded",
                changed.join(", ")
            )
        })
    }
}

/// A file's execute bits, where files have them.
fn execute_bits(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .ok()
            .map(|meta| meta.permissions().mode() & 0o111)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

fn cacheable(record: &TaskReceipt) -> bool {
    record.outcome == Outcome::Passed && record.importable
}

/// What became of a task's shell.
enum Launched {
    /// It ran to its end, seen at `ended`, and this is what it did.
    Ran {
        record: Box<TaskReceipt>,
        ended: Instant,
    },
    /// A stop reached sykli before it saw the shell's end: sykli signalled
    /// its group, or was signalled itself (the same stop may have reached
    /// the task directly, as a signal to a process group holding both
    /// does). What the shell exited with (its code, or the signal that ended
    /// it) may answer that signal, so what the task did is unknown.
    #[cfg_attr(not(unix), allow(dead_code))]
    Stopped {
        exit_code: Option<i32>,
        signal: Option<i32>,
    },
}

/// Run one task's shell and record what it did. With an attempt, the shell
/// inherits the attempt's lease and its start is journaled; a start that
/// cannot be journaled kills it and is an error. On Unix the shell runs in
/// its own process group: as soon as it exits, every process still in that
/// group is killed, so a straggler there holding the output pipes cannot keep
/// the task open (a process that left the group, by `setsid`, is not
/// reached), and a signal to sykli reaches the group through sykli, which
/// stops it (`Launched::Stopped`).
fn launch(
    task: &Task,
    runtime: &ShellRuntime,
    progress: Progress,
    capture_limit: usize,
    attempt: Option<&runs::Attempt<'_>>,
    contract_inputs: Option<(&Path, &BTreeMap<String, Option<InputRecord>>)>,
) -> Result<Launched, String> {
    progress.line("running", &task.name);
    let declared = worktree_cache::environment(task)?;
    // Before the task starts: its key needed them already (`cache_key`).
    let inherited_keyed_digests = inherited_keyed_digests(task)?;
    let errored = |error: &str, class: &'static str| {
        Ok(Launched::Ran {
            record: Box::new(empty_task_receipt(
                task,
                runtime,
                Outcome::Errored,
                error,
                class,
                false,
            )),
            ended: Instant::now(),
        })
    };
    // Removed when this function returns, once the shell has ended.
    let copy =
        match contract_inputs.map(|(directory, records)| ContractCopy::new(directory, records)) {
            None => None,
            Some(Ok(copy)) => Some(copy),
            Some(Err((error, class))) => return errored(&error, class),
        };
    // Instant for the duration, wall clock for the claim: Instant is monotonic
    // and cannot be read as a time of day, SystemTime can jump but is what a
    // freshness policy compares against.
    let started_at_ms = unix_time_ms();
    let started = Instant::now();
    let mut command = ProcessCommand::new(&runtime.path);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setpgid is async-signal-safe and allocates nothing.
        unsafe {
            command.pre_exec(|| {
                if journal::setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(unix)]
    if let Some(attempt) = attempt {
        attempt.inherit(&mut command);
    }
    #[cfg(unix)]
    worktree_cache::inherit(task, &mut command);
    // `--` keeps a command that starts with `-` from being read as a shell
    // option; a null stdin keeps tasks from prompting or racing for the
    // terminal, and out of the receipt as an undeclared input.
    command
        .arg("-c")
        .arg("--")
        .arg(&task.run)
        .env_clear()
        .stdin(Stdio::null())
        .envs(
            runtime
                .environment
                .iter()
                .map(|(name, value)| (name, value)),
        )
        .envs(declared)
        .envs(
            task.inherit
                .iter()
                .filter_map(|name| std::env::var_os(name).map(|value| (name, value))),
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Last, so neither a declared nor an inherited value replaces it.
    if let Some(copy) = &copy {
        command.env("SYKLI_CONTRACT_DIR", &copy.directory);
    }
    if let Some(workdir) = &task.workdir {
        command.current_dir(workdir);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return errored(&error.to_string(), "spawn_error"),
    };
    let group = child.id() as i32;
    #[cfg(unix)]
    runs::register(group);
    if let Some(attempt) = attempt {
        if let Err(error) = attempt.started(group, started_at_ms) {
            #[cfg(unix)]
            // SAFETY: the group this process just created.
            unsafe {
                journal::killpg(group, 9);
            }
            let _ = child.kill();
            let _ = child.wait();
            #[cfg(unix)]
            runs::unregister(group);
            return Err(error);
        }
    }
    // Each stream is relayed on its own thread until the last process
    // holding it lets go, which may be one that left the group.
    let (sender, received) = std::sync::mpsc::channel();
    let stdout: Box<dyn Read + Send> = Box::new(child.stdout.take().unwrap());
    let stderr: Box<dyn Read + Send> = Box::new(child.stderr.take().unwrap());
    let relays = [(stdout, progress.stdout()), (stderr, progress.stderr())];
    for (index, (reader, writer)) in relays.into_iter().enumerate() {
        let sender = sender.clone();
        thread::spawn(move || {
            let _ = sender.send((index, relay(reader, writer, capture_limit)));
        });
    }
    drop(sender);
    let status = child.wait().map_err(|error| error.to_string());
    let ended = Instant::now();
    // The leader has exited; anything left in its group is a straggler,
    // killed now rather than waited for.
    // SAFETY: killpg on a group id we created; ESRCH when already empty.
    #[cfg(unix)]
    unsafe {
        journal::killpg(group, 9);
    }
    // Once sykli has signalled the group, what the attempt prints no longer
    // matters: it is not waited for, so a process that left the group and
    // still holds a stream cannot hold up the stop.
    let stopping = || {
        #[cfg(unix)]
        {
            runs::stopped(group)
        }
        #[cfg(not(unix))]
        {
            false
        }
    };
    let mut captured: [Option<CapturedOutput>; 2] = [None, None];
    while captured.iter().any(Option::is_none) {
        match received.recv_timeout(std::time::Duration::from_millis(25)) {
            Ok((index, output)) => captured[index] = Some(output),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if !stopping() => {}
            Err(_) => break,
        }
    }
    // Stopped when sykli signalled the group, or saw a stop itself before
    // getting here: the same stop may have reached the task first. (`|`, so
    // the group is always unregistered.) A stop that reaches sykli only
    // after this is `withdraw_before_stop`'s.
    #[cfg(unix)]
    if runs::unregister(group) | runs::signalled().is_some() {
        let status = status.ok();
        return Ok(Launched::Stopped {
            exit_code: status.and_then(|status| status.code()),
            signal: status.as_ref().and_then(exit_signal),
        });
    }
    let [stdout, stderr] = captured.map(|output| {
        output.unwrap_or_else(|| CapturedOutput {
            bytes: Vec::new(),
            digest: sha256(&[]),
            bytes_dropped: 0,
            error: Some("an output reader panicked".into()),
        })
    });

    let CapturedOutput {
        bytes: stdout,
        digest: stdout_digest,
        bytes_dropped: stdout_bytes_dropped,
        error: stdout_error,
    } = stdout;
    let CapturedOutput {
        bytes: stderr,
        digest: stderr_digest,
        bytes_dropped: stderr_bytes_dropped,
        error: stderr_error,
    } = stderr;

    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut output_digests = BTreeMap::new();
    let mut outcome = Outcome::Passed;
    // Truncated captures stay importable: `stdout_digest`/`stderr_digest`
    // cover the full raw streams, and `*_truncated` says what the text omits.
    let mut importable = true;
    let mut class = None;
    let mut retryable = false;
    let capture_error = stdout_error.or(stderr_error);
    let mut error = None;
    let signal = status.as_ref().ok().and_then(exit_signal);
    let exit_code = match status {
        Ok(status) => {
            if !status.success() {
                outcome = Outcome::Failed;
                class = Some("command_failed".into());
                error = Some(status.to_string());
            }
            status.code()
        }
        Err(wait_error) => {
            outcome = Outcome::Errored;
            importable = false;
            class = Some("runtime_error".into());
            retryable = true;
            error = Some(wait_error);
            None
        }
    };

    if let Some(capture_error) = capture_error {
        importable = false;
        if outcome == Outcome::Passed {
            outcome = Outcome::Errored;
            class = Some("capture_error".into());
            retryable = true;
            error = Some(capture_error);
        } else {
            error = Some(format!(
                "{}; output capture failed: {capture_error}",
                error.as_deref().unwrap_or("task failed")
            ));
        }
    }
    if let Err(output_error) = verify_outputs(task, &mut output_digests) {
        if outcome == Outcome::Passed {
            outcome = Outcome::Failed;
            class = Some("missing_output".into());
            error = Some(output_error);
        }
    }

    Ok(Launched::Ran {
        record: Box::new(TaskReceipt {
            name: task.name.clone(),
            command: task.run.clone(),
            runtime_fingerprint: runtime.fingerprint.clone(),
            started_at_ms: Some(started_at_ms),
            exit_code,
            signal,
            duration_ms,
            stdout_truncated: stdout_bytes_dropped != 0,
            stdout_bytes_dropped,
            stderr_truncated: stderr_bytes_dropped != 0,
            stderr_bytes_dropped,
            stdout_digest,
            stderr_digest,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            output_digests,
            inherited_keyed_digests,
            outcome,
            importable,
            class,
            retryable,
            source: "task".into(),
            error,
            provenance: None,
            cache_key: None,
            inputs: None,
            contract_inputs: None,
        }),
        ended,
    })
}

/// The signal that ended a process, where there are signals.
fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

fn relay(mut reader: impl Read, mut writer: impl Write, limit: usize) -> CapturedOutput {
    let mut captured = Vec::new();
    let mut digest = Sha256::new();
    let mut bytes_dropped = 0_u64;
    let mut buffer = [0; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                return CapturedOutput {
                    bytes: captured,
                    digest: format!("{:x}", digest.finalize()),
                    bytes_dropped,
                    error: None,
                };
            }
            Ok(count) => {
                digest.update(&buffer[..count]);
                let retained = count.min(limit.saturating_sub(captured.len()));
                captured.extend_from_slice(&buffer[..retained]);
                bytes_dropped = bytes_dropped
                    .saturating_add(u64::try_from(count - retained).unwrap_or(u64::MAX));
                let _ = writer.write_all(&buffer[..count]);
                let _ = writer.flush();
            }
            Err(error) => {
                return CapturedOutput {
                    bytes: captured,
                    digest: format!("{:x}", digest.finalize()),
                    bytes_dropped,
                    error: Some(error.to_string()),
                };
            }
        }
    }
}

fn verify_outputs(task: &Task, digests: &mut BTreeMap<String, String>) -> Result<(), String> {
    let root = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
    for output in &task.outputs {
        let path = root.join(output);
        // ponytail: declared outputs are files; add directory-tree hashing when a contract needs it.
        if !path.is_file() {
            return Err(format!(
                "declared output {output:?} is missing or not a file"
            ));
        }
        digests.insert(output.clone(), sha256_file(&path)?);
    }
    Ok(())
}

fn empty_task_receipt(
    task: &Task,
    runtime: &ShellRuntime,
    outcome: Outcome,
    error: &str,
    class: &'static str,
    retryable: bool,
) -> TaskReceipt {
    TaskReceipt {
        name: task.name.clone(),
        command: task.run.clone(),
        runtime_fingerprint: runtime.fingerprint.clone(),
        // The task never started, so there is no moment to witness.
        started_at_ms: None,
        exit_code: None,
        signal: None,
        duration_ms: 0,
        stdout: String::new(),
        stdout_truncated: false,
        stdout_bytes_dropped: 0,
        stderr: String::new(),
        stderr_truncated: false,
        stderr_bytes_dropped: 0,
        stdout_digest: sha256(&[]),
        stderr_digest: sha256(&[]),
        output_digests: BTreeMap::new(),
        // A task that errored because the key cannot be read records none.
        inherited_keyed_digests: inherited_keyed_digests(task).unwrap_or_default(),
        outcome,
        importable: outcome != Outcome::Errored,
        class: Some(class.into()),
        retryable,
        source: "task".into(),
        error: Some(error.into()),
        provenance: None,
        cache_key: None,
        inputs: None,
        contract_inputs: None,
    }
}

fn input_digest(path: &Path) -> Result<String, String> {
    input_record(path)?.digest()
}

/// What a run read of one declared input: the SHA-256 of its bytes, and the
/// execute bits (`mode & 0o111`) on platforms that have them. Its digest
/// (`sykli-input.v2`) is the input's part of cache keys and of the inputs
/// digest; the record itself goes into the receipt, so `verify --commit` can
/// compare each input with the commit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct InputRecord {
    sha256: String,
    /// The execute bits as git has them for this input: those read from
    /// the file system when they agree with the run's tree entry (git takes
    /// an entry's bit from the owner's), else the entry's (`0o111` or 0),
    /// as where the file system has none (Windows) or git does not trust
    /// them (`core.filemode=false`). Absent only for a path not in that
    /// tree on a file system without execute bits; the digest then uses 0,
    /// as it always has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<u32>,
    /// The tree entry the run's snapshot has at this input's path, `MODE
    /// ID` as git writes a tree. Absent when the path is not in that tree
    /// (an ignored or generated file). Not part of the digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    object: Option<String>,
    /// The input is a symbolic link to a directory in the repository, or to
    /// nothing, so the run recorded the link itself: `sha256` is that of its
    /// text, as git stores it, and `mode` is absent. Its digest is
    /// `sykli-input-link.v1`. A link to a file, inside the repository or
    /// out of it, is recorded by that file's bytes, as every input always
    /// was, and this is absent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    link: bool,
}

impl InputRecord {
    fn digest(&self) -> Result<String, String> {
        let bytes = if self.link {
            serde_json::to_vec(&("sykli-input-link.v1", &self.sha256))
        } else {
            serde_json::to_vec(&("sykli-input.v2", &self.sha256, self.mode.unwrap_or(0)))
        }
        .map_err(|error| error.to_string())?;
        Ok(sha256(&bytes))
    }
}

/// The text of the symbolic link at `path`, as git stores it, when the run
/// records the link itself: it leads to a directory, or to nothing. None
/// for anything else, a link to a file included: that is read through, as
/// every input always was. A declared link is declared by its text only
/// where what it leads to cannot be read as a file; the files under a
/// directory it leads to must be declared beside it, and one that leads out
/// of the repository is refused (`link_missing_inputs`), both before any
/// task.
fn link_text(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let linked = fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink());
    if !linked || path.is_file() {
        return Ok(None);
    }
    let text = fs::read_link(path)
        .map_err(|error| format!("cannot read the symbolic link {}: {error}", path.display()))?;
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        text.as_os_str().as_bytes().to_vec()
    };
    #[cfg(not(unix))]
    let bytes = text.to_string_lossy().replace('\\', "/").into_bytes();
    Ok(Some(bytes))
}

fn input_record(path: &Path) -> Result<InputRecord, String> {
    if let Some(text) = link_text(path)? {
        return Ok(InputRecord {
            sha256: sha256(&text),
            mode: None,
            object: None,
            link: true,
        });
    }
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        Some(
            fs::metadata(path)
                .map_err(|error| error.to_string())?
                .permissions()
                .mode()
                & 0o111,
        )
    };
    #[cfg(not(unix))]
    let mode = None;
    Ok(InputRecord {
        sha256: sha256_file(path)?,
        mode,
        object: None,
        link: false,
    })
}

/// Every task's declared inputs as the run reads them: each input, as
/// declared, with its record, or None when it cannot be read (missing).
type InputRecords = BTreeMap<String, BTreeMap<String, Option<InputRecord>>>;

/// A task's own copy of the contract inputs it declares, the directory its
/// shell gets as `$SYKLI_CONTRACT_DIR`: each file's bytes copied from beside
/// the contract and hashed as they are written, and required to be the
/// bytes the run recorded (and its key covers). A task therefore runs
/// exactly what was hashed, and reaches no undeclared file beside the
/// contract by that name.
///
/// The copy is its owner's alone: a directory made exclusively under an
/// unguessable name, 0700, its files 0600, or 0700 where the recorded file
/// is executable by its owner (`fresh_directory`, `private_name`). A name
/// another process took, such as a copy a killed sykli left, is passed
/// over and never reused, so a leftover never stops a run.
///
/// Bound: one directory per running task, in the system's temporary
/// directory, removed when the task's shell has ended (on drop). One a
/// killed sykli left is removed by the next run that makes a copy once it
/// has stood unchanged for a day (`prune_stale_copies`, Linux and macOS),
/// else by that directory's own cleanup.
struct ContractCopy {
    directory: PathBuf,
}

impl ContractCopy {
    /// The copy, or why the task errors instead of running, and its class.
    fn new(
        from: &Path,
        records: &BTreeMap<String, Option<InputRecord>>,
    ) -> Result<Self, (String, &'static str)> {
        let failed = |error: String| (error, "input_error");
        let temporary = std::env::temp_dir();
        let names = std::iter::repeat_with(|| format!("sykli-contract-{}", private_name()));
        let directory = fresh_directory(&temporary, names.take(16))
            .map_err(|error| failed(format!("{}: {error}", temporary.display())))?;
        #[cfg(unix)]
        {
            static PRUNED: std::sync::Once = std::sync::Once::new();
            PRUNED.call_once(|| prune_stale_copies(&temporary, &directory));
        }
        let copy = ContractCopy { directory };
        for (input, record) in records {
            let Some(record) = record else {
                return Err(failed(format!("contract input {input} was not read")));
            };
            let target = copy.directory.join(input);
            if let Some(parent) = target.parent() {
                private_builder()
                    .recursive(true)
                    .create(parent)
                    .map_err(|error| failed(format!("{}: {error}", parent.display())))?;
            }
            let source = from.join(input);
            let executable = record.mode.is_some_and(|bits| bits & 0o100 != 0);
            let digest = copy_hashed(&source, &target, executable)
                .map_err(|error| failed(format!("{}: {error}", source.display())))?;
            if digest != record.sha256 {
                return Err((
                    format!(
                        "contract input {input} changed beside the contract after the run read \
                         it; its result would not be evidence for what the run recorded"
                    ),
                    "input_changed",
                ));
            }
        }
        Ok(copy)
    }
}

/// A `DirBuilder` whose directories are their owner's alone (0700), where
/// files have modes.
fn private_builder() -> fs::DirBuilder {
    #[allow(unused_mut)]
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
}

/// How long a copy may stand unchanged before a later run takes it for one
/// a killed sykli left.
#[cfg(unix)]
const STALE_COPY: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Remove the copies a killed sykli left: `sykli-contract-*` directories in
/// `base` owned by the owner of `ours` (the copy this process just made)
/// and unchanged for over a day (`STALE_COPY`). Done once per process,
/// before its first copy is used. Another user's directories, a symbolic
/// link and anything younger, which a task may still be reading, are never
/// touched.
#[cfg(unix)]
fn prune_stale_copies(base: &Path, ours: &Path) {
    use std::os::unix::fs::MetadataExt;
    let Ok(owner) = fs::metadata(ours).map(|meta| meta.uid()) else {
        return;
    };
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == ours
            || !entry
                .file_name()
                .to_string_lossy()
                .starts_with("sykli-contract-")
        {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        let stale = meta
            .modified()
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > STALE_COPY);
        if meta.is_dir() && meta.uid() == owner && stale {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

/// A new directory in `base`, its owner's alone, named by the first of
/// `names` that nothing there has: made exclusively, so a name another
/// process holds is passed over, never entered.
fn fresh_directory(base: &Path, names: impl IntoIterator<Item = String>) -> io::Result<PathBuf> {
    let mut taken = io::Error::new(io::ErrorKind::AlreadyExists, "no name was free");
    for name in names {
        let path = base.join(name);
        match private_builder().create(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => taken = error,
            Err(error) => return Err(error),
        }
    }
    Err(taken)
}

/// 32 hex digits no other process can guess: two hashers that std keys
/// with the operating system's randomness, over this process, the time and
/// a counter.
fn private_name() -> String {
    use std::hash::{BuildHasher, Hasher};
    let words = [0, 1].map(|_| {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u32(std::process::id());
        hasher.write_u64(unix_time_ms());
        hasher.write_u64(canonical::NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        hasher.finish()
    });
    format!("{:016x}{:016x}", words[0], words[1])
}

/// Copy `source` to a new file `target` (0600, or 0700 when `executable`,
/// where files have modes), hashing the bytes as they are written: the
/// SHA-256 of exactly what the copy holds.
fn copy_hashed(source: &Path, target: &Path, executable: bool) -> io::Result<String> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(if executable { 0o700 } else { 0o600 });
    }
    #[cfg(not(unix))]
    let _ = executable;
    let mut written = options.open(target)?;
    let mut read = fs::File::open(source)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192];
    loop {
        let count = read.read(&mut buffer)?;
        if count == 0 {
            return Ok(format!("{:x}", hasher.finalize()));
        }
        hasher.update(&buffer[..count]);
        written.write_all(&buffer[..count])?;
    }
}

impl Drop for ContractCopy {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// The digest a receipt's subject records over all declared inputs: each
/// task's inputs and their `sykli-input.v2` digests (null when missing).
fn inputs_digest_of(records: &InputRecords) -> Result<String, String> {
    let digests = records
        .iter()
        .map(|(task, inputs)| {
            let inputs = inputs
                .iter()
                .map(|(input, record)| {
                    let digest = record.as_ref().map(InputRecord::digest).transpose()?;
                    Ok((input, digest))
                })
                .collect::<Result<BTreeMap<_, _>, String>>()?;
            Ok((task, inputs))
        })
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    serde_json::to_vec(&digests)
        .map(|bytes| sha256(&bytes))
        .map_err(|error| error.to_string())
}

/// A task's content address: its declaration, its declared inputs' digests,
/// the runtime, and the keys of the tasks it runs `after`. The last term makes
/// an `after` edge carry identity: when an upstream task re-runs for new
/// inputs, everything downstream misses the cache instead of reusing a pass
/// recorded against outputs it never saw.
/// Wall-clock milliseconds since the Unix epoch, or 0 if the clock is before
/// it. Receipts carry this beside `duration_ms` so a receipt can say *when* it
/// witnessed a run, not only how long the run took.
fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn cache_key(
    task: &Task,
    runtime: &ShellRuntime,
    after: &BTreeMap<&str, &str>,
    contract_inputs: &InputRecords,
) -> Result<String, String> {
    #[derive(Serialize)]
    struct Key<'a> {
        task: &'a Task,
        inputs: BTreeMap<&'a str, String>,
        runtime: &'a str,
        after: &'a BTreeMap<&'a str, &'a str>,
        inherited: BTreeMap<&'a str, String>,
        /// Absent when the task declares none, so no other key changes.
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        contract_inputs: BTreeMap<&'a str, String>,
    }

    let root = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
    let mut inputs = BTreeMap::new();
    for input in &task.inputs {
        let path = root.join(input);
        // ponytail: declared inputs are files, or symbolic links to anything
        // (`link_text`); add glob/tree hashing when a contract needs it.
        let linked = fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink());
        if !path.is_file() && !linked {
            return Err(format!("declared input {input:?} is missing or not a file"));
        }
        inputs.insert(input.as_str(), input_digest(&path)?);
    }
    // From the records the run read once, never read again here: the key
    // is what the receipt records, and the task gets exactly those bytes.
    let mut declared = BTreeMap::new();
    for input in &task.contract_inputs {
        let record = contract_inputs
            .get(&task.name)
            .and_then(|records| records.get(input))
            .cloned()
            .flatten()
            .ok_or_else(|| {
                format!(
                    "declared contract input {input:?} is missing beside the contract or not a file"
                )
            })?;
        declared.insert(input.as_str(), record.digest()?);
    }
    let bytes = serde_json::to_vec(&Key {
        task,
        inputs,
        runtime: &runtime.fingerprint,
        after,
        inherited: inherited_keyed_digests(task)?
            .iter()
            .map(|(name, digest)| (name.as_str(), digest.clone()))
            .collect(),
        contract_inputs: declared,
    })
    .map_err(|error| error.to_string())?;
    Ok(sha256(&bytes))
}

/// Digests of the inherited variables' current values, never the values,
/// as a receipt's `inherited_keyed_digests`: each is HMAC-SHA256 of
/// `sykli-inherited.v1`, the name and the value (NUL-separated) under the
/// clone's own key (`inherit_key`). An unset variable digests as `absent`,
/// which is not any value's digest.
///
/// A plain SHA-256 of a value, which receipts recorded as
/// `inherited_digests` before, is a check anyone holding a receipt can run
/// a guess against, and CI uploads receipts: a short token falls to trying
/// every candidate. A salt written into the receipt would not stop that, as
/// the salt is then as public as the digest. Keyed by a secret no receipt
/// holds, the digest still tells two runs of one clone apart, so the cache
/// misses when a value changes, and tells nobody else anything. The price:
/// digests are comparable only within one clone (another clone, a fresh
/// CI checkout, has its own key), and nobody can check a receipt's digest
/// against a value they know. The key is only as secret as the clone: any
/// task the clone runs can read it (on a runner that keeps `.git` between
/// jobs, a pull request's code too), and whoever has read it can try
/// guesses against every digest the clone publishes.
///
/// A task that inherits nothing asks for no key; one that does makes it
/// when it is missing, and errors when it cannot be read or made.
fn inherited_keyed_digests(task: &Task) -> Result<BTreeMap<String, String>, String> {
    if task.inherit.is_empty() {
        return Ok(BTreeMap::new());
    }
    let key = inherit_key()?;
    Ok(task
        .inherit
        .iter()
        .map(|name| {
            let digest = match std::env::var_os(name) {
                Some(value) => {
                    let mut message = b"sykli-inherited.v1\0".to_vec();
                    message.extend_from_slice(name.as_bytes());
                    message.push(0);
                    message.extend_from_slice(value.as_encoded_bytes());
                    hmac_sha256(key, &message)
                }
                None => "absent".to_string(),
            };
            (name.clone(), digest)
        })
        .collect())
}

/// HMAC-SHA256 (RFC 2104) of `message` under `key`, as lowercase hex.
fn hmac_sha256(key: &[u8], message: &[u8]) -> String {
    const BLOCK: usize = 64;
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let padded = |pad: u8| block.map(|byte| byte ^ pad);
    let inner = Sha256::new()
        .chain_update(padded(0x36))
        .chain_update(message)
        .finalize();
    let outer = Sha256::new()
        .chain_update(padded(0x5c))
        .chain_update(inner)
        .finalize();
    format!("{outer:x}")
}

/// Where the key that inherited values are digested under is kept: in the
/// common git directory, beside the runs, so every worktree of a clone
/// shares it, and never in a worktree's `.sykli/`, which a person may copy,
/// cache or upload with its receipts.
const INHERIT_KEY: &str = "sykli/inherit-key";

/// The clone's key, read once per process; the key a run made first.
static KEY: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();

/// The clone's key for `inherited_keyed_digests`, made by the first run that
/// needs it.
///
/// Bound: one file of 64 hex digits (32 bytes) per clone, made once and
/// never rewritten; it goes when the clone does. Deleting it costs a cache
/// miss for each task that inherits, and makes earlier digests incomparable
/// with later ones. A fresh clone makes its own, so a CI job that restores
/// `.sykli/` into a fresh clone misses the cache for every task that
/// inherits, on every run.
fn inherit_key() -> Result<&'static [u8; 32], String> {
    if let Some(key) = KEY.get() {
        return Ok(key);
    }
    let key = read_or_make_key(&inherit_key_path()?)?;
    Ok(KEY.get_or_init(|| key))
}

/// The clone's key if a run has made it, never making one: for
/// `plan --explain`, which only reads. Without one, nothing is cached under
/// it.
fn made_inherit_key() -> Result<Option<&'static [u8; 32]>, String> {
    if let Some(key) = KEY.get() {
        return Ok(Some(key));
    }
    let key = read_key(&inherit_key_path()?)?;
    Ok(key.map(|key| KEY.get_or_init(|| key)))
}

fn inherit_key_path() -> Result<PathBuf, String> {
    let repository = repo::repository().map_err(|_| {
        "a task inherits a variable, whose value is digested under a key kept in the git \
         common directory, and there is no git repository here"
            .to_string()
    })?;
    Ok(repository.common_dir.join(INHERIT_KEY))
}

/// The key at `path`, or None when there is none.
fn read_key(path: &Path) -> Result<Option<[u8; 32]>, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let mut key = [0u8; 32];
    let decoded = text.len() == 64
        && text.bytes().all(|byte| byte.is_ascii_hexdigit())
        && key
            .iter_mut()
            .zip(text.as_bytes().chunks(2))
            .all(|(byte, pair)| {
                std::str::from_utf8(pair)
                    .ok()
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                    .map(|value| *byte = value)
                    .is_some()
            });
    if !decoded {
        return Err(format!(
            "{}: not a key sykli wrote; remove it, and the next run makes a new one",
            path.display()
        ));
    }
    Ok(Some(key))
}

#[cfg(windows)]
mod windows_random {
    #[link(name = "bcrypt")]
    unsafe extern "system" {
        pub(super) fn BCryptGenRandom(
            algorithm: *mut std::ffi::c_void,
            buffer: *mut u8,
            size: u32,
            flags: u32,
        ) -> i32;
    }
}

/// 32 bytes from the operating system's generator: `/dev/urandom` on Unix,
/// and on Windows `BCryptGenRandom` with the system-preferred generator,
/// declared here (`bcrypt.dll`, part of Windows), so no crate is needed.
fn random_key() -> io::Result<[u8; 32]> {
    let mut key = [0u8; 32];
    #[cfg(unix)]
    fs::File::open("/dev/urandom")?.read_exact(&mut key)?;
    #[cfg(windows)]
    {
        const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x2;
        // SAFETY: no algorithm handle, with the flag that asks for the
        // system's generator; `key` is writable for its whole length.
        let status = unsafe {
            windows_random::BCryptGenRandom(
                std::ptr::null_mut(),
                key.as_mut_ptr(),
                key.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status != 0 {
            return Err(io::Error::other(format!(
                "BCryptGenRandom failed with NTSTATUS {status:#x}"
            )));
        }
    }
    #[cfg(not(any(unix, windows)))]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no source of randomness for the inherit key on this platform",
    ));
    #[cfg(any(unix, windows))]
    Ok(key)
}

/// The key at `path`, made there first when there is none: 32 bytes from
/// the operating system (`random_key`), written whole as hex to a private
/// temporary, synced, and linked into place, so a reader never sees part of
/// one and two processes that race agree on the first. Where the file
/// system has no hard links (exFAT, some FUSE or SMB mounts), it is renamed
/// into place unless a key is there already (`place_by_rename`): two first
/// runs that race there may each digest one run under their own key, and
/// the later one is kept, which costs a cache miss. On Linux and macOS a
/// run never gets here, as its journal already needs hard links in the same
/// directory (`publish_with`); it is kept for Windows, where a run is not
/// journaled and nothing else there needs them.
fn read_or_make_key(path: &Path) -> Result<[u8; 32], String> {
    if let Some(key) = read_key(path)? {
        return Ok(key);
    }
    let shown = |error: io::Error| format!("{}: {error}", path.display());
    let directory = path.parent().ok_or("the key has no directory")?;
    fs::create_dir_all(directory).map_err(shown)?;
    let key = random_key().map_err(shown)?;
    let text = key
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let temporary = directory.join(format!(".inherit-key-{}", private_name()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options
        .open(&temporary)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())?;
            canonical::sync(&file, canonical::Durability::Fsync)
        })
        .and_then(|()| match fs::hard_link(&temporary, path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(_) => place_by_rename(&temporary, path),
        });
    // The temporary never outlives this call, whatever happened.
    let _ = fs::remove_file(&temporary);
    written.map_err(shown)?;
    let _ = canonical::sync_directory(directory, canonical::Durability::Fsync);
    read_key(path)?.ok_or_else(|| format!("{}: made, then gone", path.display()))
}

/// Move `temporary` to `path` unless something is there already, for a file
/// system without hard links. The check and the rename are two steps, so a
/// key another process places between them is replaced.
fn place_by_rename(temporary: &Path, path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path).is_ok() {
        return Ok(());
    }
    fs::rename(temporary, path)
}

/// Every task's declared inputs as the run reads them, each with the tree
/// entry the run's snapshot has at its path (`facts`, by path from the top
/// level, as `input_paths` gives them in `declared`), and its execute bits
/// as git has them: the file system's when they agree with the entry of
/// the file the input reads (its own, or a symbolic link's declared
/// target), else that entry's.
fn declared_input_records(
    contract: &Contract,
    declared: &[(String, String, String)],
    facts: &PathFacts,
) -> InputRecords {
    let paths = declared
        .iter()
        .map(|(task, input, path)| ((task.as_str(), input.as_str()), path))
        .collect::<HashMap<_, _>>();
    contract
        .tasks
        .iter()
        .map(|task| {
            let root = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
            let files = task
                .inputs
                .iter()
                .map(|input| {
                    let path = paths.get(&(task.name.as_str(), input.as_str()));
                    let record = input_record(&root.join(input)).ok().map(|mut record| {
                        if let Some(path) = path {
                            if let Some(Found::Entry(entry)) = facts.found.get(*path) {
                                record.object = Some(entry.clone());
                            }
                            // A link recorded by its text has no execute bits.
                            if let Some((_, file)) =
                                facts.file_behind(path).ok().filter(|_| !record.link)
                            {
                                record.mode = Some(git_execute_bits(record.mode, &file));
                            }
                        }
                        record
                    });
                    (input.clone(), record)
                })
                .collect();
            (task.name.clone(), files)
        })
        .collect()
}

/// The execute bits a run records for a file whose tree entry is `entry`:
/// those it read (`read`) when git agrees with them, else git's own. Git
/// takes a file's bit from its owner's execute bit, and ignores the file
/// system where it has no execute bits or is told not to trust them
/// (`core.filemode=false`), keeping the bit it had; the bit a commit stores
/// is then the one to bind.
fn git_execute_bits(read: Option<u32>, entry: &str) -> u32 {
    let executable = entry.starts_with("100755 ");
    match read {
        Some(bits) if (bits & 0o100 != 0) == executable => bits,
        _ if executable => 0o111,
        _ => 0,
    }
}

impl LocalCache {
    fn new(repository: &Path) -> Self {
        Self {
            cache: repository.join(".sykli/cache"),
            receipts: repository.join(".sykli/receipts"),
            budget: cache_budget(),
            receipt_budget: receipt_budget(),
        }
    }

    /// Drop unreferenced receipts until the directory is inside its budget,
    /// and clear staging files no run will finish.
    ///
    /// Runs after cache eviction, because evicting a cache entry is what
    /// releases the receipt it cited. Oldest first, and never a receipt a
    /// surviving cache entry still names.
    ///
    /// Nothing written in the last `IN_FLIGHT` is touched. A run writes its
    /// receipt before storing the cache entries that cite it, so between those
    /// two moments the receipt is on disk and unreferenced. Another process
    /// pruning in that window would delete it, and the first would then store
    /// an entry citing a receipt that no longer exists — a cache entry that can
    /// never hit again, which is the one state this design must not produce.
    fn prune_receipts(&self) {
        const IN_FLIGHT: std::time::Duration = std::time::Duration::from_secs(300);

        let budget = self.receipt_budget;
        if budget == 0 {
            return;
        }
        let referenced = referenced_receipts(&self.cache);
        let Ok(entries) = fs::read_dir(&self.receipts) else {
            return;
        };
        let now = std::time::SystemTime::now();
        let mut files: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
        // Referenced receipts consume the budget without being candidates, but
        // only the ones actually on disk: counting a name whose file is gone
        // would shrink the budget and destroy a live receipt for nothing.
        let mut present_referenced = 0usize;
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
                continue;
            };
            let recent = now
                .duration_since(modified)
                .map(|age| age < IN_FLIGHT)
                .unwrap_or(true);
            if !name.starts_with("rcpt_") {
                // `write_receipt` stages through `.{hash}.tmp-{pid}`. A run
                // killed between the write and the rename leaves one behind,
                // and nothing else ever removes it — so the directory grew
                // without limit despite having a budget.
                if name.starts_with('.') && name.contains(".tmp-") && !recent {
                    let _ = fs::remove_file(&path);
                }
                continue;
            }
            if referenced.contains(&name) {
                present_referenced += 1;
                continue;
            }
            if recent {
                continue;
            }
            files.push((path, modified));
        }
        let total = files.len() + present_referenced;
        if total <= budget {
            return;
        }
        files.sort_by_key(|(_, modified)| *modified);
        // Never more than there are candidates. When referenced receipts alone
        // exceed the budget, `total - budget` runs past the end of the list and
        // would take every candidate regardless of age — the budget cannot be
        // met by deleting things it is not allowed to delete, and pretending
        // otherwise just destroys the newest receipts too.
        let over = (total - budget).min(files.len());
        for (path, _) in files.iter().take(over) {
            let _ = fs::remove_file(path);
        }
    }

    /// Bring the cache and the receipts directory inside their budgets.
    ///
    /// Called when a run starts as well as when it stores. Enforcing only on
    /// store means a repository that stops producing new entries keeps whatever
    /// it had forever — the budget would bound growth but never the standing
    /// total, and a repository that has just been cloned or has had its budget
    /// lowered would never act on it.
    fn bound(&self) {
        // Nothing is being written, so no entry is exempt.
        self.evict(Path::new(""));
        self.prune_receipts();
    }

    /// Drop least recently used entries until the cache is inside its budget.
    ///
    /// Best effort, and never fails a run: losing a cache entry costs a re-run,
    /// while failing here would lose the work that just succeeded. `keep` is the
    /// entry this store wrote — evicting it immediately would mean the cache
    /// could never hold anything larger than the budget's last increment.
    fn evict(&self, keep: &Path) {
        if self.budget == 0 {
            return;
        }
        let mut entries = cache_entries(&self.cache);
        let mut total: u64 = entries.iter().map(|(_, size, _)| size).sum();
        if total <= self.budget {
            return;
        }
        entries.sort_by_key(|(_, _, modified)| *modified);
        for (path, size, _) in entries {
            if total <= self.budget {
                break;
            }
            if path == keep {
                continue;
            }
            if fs::remove_dir_all(&path).is_ok() {
                total = total.saturating_sub(size);
            }
        }
    }

    /// Validate evidence without restoring outputs or creating any files.
    fn inspect(&self, task: &Task, key: &str, fingerprint: &str) -> Result<CacheEntry, CacheMiss> {
        let directory = self.cache.join(key);
        let bytes = fs::read(directory.join("entry.json")).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                CacheMiss::EntryMissing
            } else {
                CacheMiss::EntryUnreadable
            }
        })?;
        let entry: CacheEntry =
            serde_json::from_slice(&bytes).map_err(|_| CacheMiss::EntryInvalid)?;
        if entry.outputs.len() != task.outputs.len()
            || !entry.outputs.iter().zip(&task.outputs).enumerate().all(
                |(index, (output, path))| {
                    output.path == *path && output.artifact == index.to_string()
                },
            )
        {
            return Err(CacheMiss::EntryInvalid);
        }
        if !self.provenance_valid(task, &entry, fingerprint) {
            return Err(CacheMiss::ProvenanceInvalid);
        }
        for output in &entry.outputs {
            let artifact = directory.join("outputs").join(&output.artifact);
            let metadata = fs::metadata(&artifact).map_err(|error| {
                if error.kind() == io::ErrorKind::NotFound {
                    CacheMiss::ArtifactMissing
                } else {
                    CacheMiss::ArtifactUnreadable
                }
            })?;
            if !metadata.is_file() {
                return Err(CacheMiss::ArtifactInvalid);
            }
            if sha256_file(&artifact).map_err(|_| CacheMiss::ArtifactUnreadable)? != output.digest {
                return Err(CacheMiss::ArtifactInvalid);
            }
        }
        Ok(entry)
    }

    fn provenance_valid(&self, task: &Task, entry: &CacheEntry, fingerprint: &str) -> bool {
        let Some(expected) = entry
            .receipt
            .strip_prefix("rcpt_")
            .and_then(|name| name.strip_suffix(".json"))
        else {
            return false;
        };
        if Path::new(&entry.receipt)
            .file_name()
            .and_then(|name| name.to_str())
            != Some(entry.receipt.as_str())
        {
            return false;
        }
        let Ok(bytes) = fs::read(self.receipts.join(&entry.receipt)) else {
            return false;
        };
        if sha256(&bytes) != expected {
            return false;
        }
        let Ok(receipt) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return false;
        };
        receipt["tasks"].as_array().is_some_and(|tasks| {
            tasks.iter().any(|record| {
                record["name"] == task.name
                    && record["command"] == task.run
                    && record["runtime_fingerprint"] == fingerprint
                    && record["outcome"] == "passed"
                    && record["importable"] == true
                    && entry
                        .outputs
                        .iter()
                        .all(|output| record["output_digests"][&output.path] == output.digest)
            })
        })
    }
}

/// Copy a cached artifact into place. The copy is hashed before it replaces
/// anything: the entry may be replaced between its inspection and this copy,
/// and a restored file must be exactly the bytes its receipt names.
fn restore_file(source: &Path, target: &Path, suffix: &str, digest: &str) -> Result<(), String> {
    let parent = target
        .parent()
        .ok_or_else(|| format!("output {} has no parent", target.display()))?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = parent.join(format!(".sykli-cache-{}-{suffix}", std::process::id()));
    let _ = fs::remove_file(&temporary);
    if let Err(error) = fs::copy(source, &temporary) {
        let _ = fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    if sha256_file(&temporary)? != digest {
        let _ = fs::remove_file(&temporary);
        return Err("the cached artifact changed while it was restored".into());
    }
    if let Err(error) = fs::rename(&temporary, target) {
        let _ = fs::remove_file(temporary);
        return Err(error.to_string());
    }
    Ok(())
}

impl Cache for LocalCache {
    fn restore(&self, task: &Task, key: &str, runtime: &ShellRuntime) -> Option<TaskReceipt> {
        let started = Instant::now();
        let directory = self.cache.join(key);
        let entry = self.inspect(task, key, &runtime.fingerprint).ok()?;

        let root = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
        for output in &entry.outputs {
            let artifact = directory.join("outputs").join(&output.artifact);
            restore_file(
                &artifact,
                &root.join(&output.path),
                &output.artifact,
                &output.digest,
            )
            .ok()?;
        }

        // Record the use. Restoring only reads the stored artifacts, so without
        // this an entry hit by every run looks exactly as old as the day it was
        // written, and eviction would drop the hottest entries first — the run
        // then recomputes that work, stores it again, and goes over budget
        // again. Touching a marker is what makes "least recently used" mean
        // used rather than written.
        let _ = fs::write(directory.join("used"), b"");

        let output_digests = entry
            .outputs
            .iter()
            .map(|output| (output.path.clone(), output.digest.clone()))
            .collect();
        Some(TaskReceipt {
            name: task.name.clone(),
            command: task.run.clone(),
            runtime_fingerprint: runtime.fingerprint.clone(),
            // The original run's moment, not now: this evidence was witnessed
            // then and is being reused.
            started_at_ms: entry.witnessed_at_ms,
            exit_code: None,
            signal: None,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            stdout: String::new(),
            stdout_truncated: false,
            stdout_bytes_dropped: 0,
            stderr: String::new(),
            stderr_truncated: false,
            stderr_bytes_dropped: 0,
            stdout_digest: sha256(&[]),
            stderr_digest: sha256(&[]),
            output_digests,
            inherited_keyed_digests: inherited_keyed_digests(task).ok()?,
            outcome: Outcome::Cached,
            importable: true,
            class: None,
            retryable: false,
            source: "cache".into(),
            error: None,
            provenance: Some(entry.receipt),
            cache_key: None,
            inputs: None,
            contract_inputs: None,
        })
    }

    /// Store a passing task's outputs under its key, durably and without a
    /// window in which another process can lose or corrupt the entry.
    ///
    /// The entry is built in a private temporary directory: each output is
    /// copied and re-hashed (it must still be the bytes the receipt names),
    /// then every file and directory is synced, then the directory is
    /// renamed into place. An entry is never deleted in place: when the key
    /// is taken, an entry whose evidence still verifies is as good as this
    /// one and is kept; one that does not is renamed aside, atomically, and
    /// only then removed. Readers therefore see the old entry, the new one,
    /// or none (a miss), never a mixture.
    fn store(&self, task: &Task, record: &TaskReceipt, receipt: &str) -> Result<(), String> {
        let Some(key) = &record.cache_key else {
            return Ok(());
        };
        let error = |error: std::io::Error| error.to_string();
        fs::create_dir_all(&self.cache).map_err(error)?;
        let unique = || {
            format!(
                "{}-{}",
                std::process::id(),
                canonical::NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            )
        };
        let temporary = self.cache.join(format!(".{key}.tmp-{}", unique()));
        let built = (|| -> Result<(), String> {
            fs::create_dir_all(temporary.join("outputs")).map_err(error)?;
            let root = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
            let mut outputs = Vec::new();
            for (index, output) in task.outputs.iter().enumerate() {
                let digest = record
                    .output_digests
                    .get(output)
                    .ok_or_else(|| format!("missing digest for output {output:?}"))?;
                let artifact = index.to_string();
                let copy = temporary.join("outputs").join(&artifact);
                fs::copy(root.join(output), &copy).map_err(error)?;
                if &sha256_file(&copy)? != digest {
                    return Err(format!(
                        "output {output:?} changed after the task ran; not cached"
                    ));
                }
                // Writable: Windows flushes only a handle opened for writing.
                let copied = fs::OpenOptions::new()
                    .write(true)
                    .open(&copy)
                    .map_err(error)?;
                canonical::sync(&copied, canonical::Durability::Fsync).map_err(error)?;
                outputs.push(CacheOutput {
                    path: output.clone(),
                    artifact,
                    digest: digest.clone(),
                });
            }
            let entry = serde_json::to_vec_pretty(&CacheEntry {
                receipt: receipt.into(),
                outputs,
                witnessed_at_ms: record.started_at_ms,
            })
            .map_err(|error| error.to_string())?;
            let mut file = fs::File::create(temporary.join("entry.json")).map_err(error)?;
            file.write_all(&entry).map_err(error)?;
            canonical::sync(&file, canonical::Durability::Fsync).map_err(error)?;
            canonical::sync_directory(&temporary.join("outputs"), canonical::Durability::Fsync)?;
            canonical::sync_directory(&temporary, canonical::Durability::Fsync)?;
            checkpoint("cache-store")
        })();
        if let Err(failed) = built {
            let _ = fs::remove_dir_all(&temporary);
            return Err(failed);
        }
        let target = self.cache.join(key);
        for _ in 0..3 {
            match fs::rename(&temporary, &target) {
                Ok(()) => {
                    canonical::sync_directory(&self.cache, canonical::Durability::Fsync)?;
                    self.evict(&target);
                    return Ok(());
                }
                // The target was there: still, or until a concurrent store
                // moved it aside after this rename failed (so `exists` alone
                // can miss it, and the store failed with "Directory not
                // empty"). Either way, look again.
                Err(failed)
                    if target.exists()
                        || matches!(
                            failed.kind(),
                            io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::AlreadyExists
                        ) =>
                {
                    if self.inspect(task, key, &record.runtime_fingerprint).is_ok() {
                        let _ = fs::remove_dir_all(&temporary);
                        return Ok(());
                    }
                    let aside = self.cache.join(format!(".{key}.old-{}", unique()));
                    if fs::rename(&target, &aside).is_ok() {
                        let _ = fs::remove_dir_all(aside);
                    }
                }
                Err(failed) => {
                    let _ = fs::remove_dir_all(&temporary);
                    return Err(failed.to_string());
                }
            }
        }
        let _ = fs::remove_dir_all(&temporary);
        Err(format!(
            "cache entry {key} kept changing while it was stored"
        ))
    }
}

/// The `sh` that tasks run under, as the shell itself resolves it.
#[cfg(not(windows))]
fn resolve_shell() -> Result<PathBuf, String> {
    let output = ProcessCommand::new("sh")
        .args(["-c", "command -v sh"])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err("cannot resolve shell runtime".into());
    }
    fs::canonicalize(String::from_utf8_lossy(&output.stdout).trim())
        .map_err(|error| error.to_string())
}

/// On Windows a POSIX `sh` (Git for Windows, MSYS2, Busybox) reports its own
/// location as a POSIX path, so search `PATH` the way process creation does.
#[cfg(windows)]
fn resolve_shell() -> Result<PathBuf, String> {
    let paths = std::env::var_os("PATH").unwrap_or_default();
    let found = std::env::split_paths(&paths)
        .map(|directory| directory.join("sh.exe"))
        .find(|candidate| candidate.is_file())
        .ok_or("cannot resolve shell runtime: no sh.exe on PATH; install Git for Windows or another POSIX sh")?;
    fs::canonicalize(found).map_err(|error| error.to_string())
}

fn shell_runtime() -> Result<ShellRuntime, String> {
    let path = resolve_shell()?;
    let environment = without_git_prepend(
        inherited_environment(std::env::vars_os()),
        std::env::var_os("GIT_EXEC_PATH"),
    );
    let fingerprint = format!(
        "shell:{}:sha256:{}:env:sha256:{environment}",
        path.display(),
        sha256_file(&path)?,
        environment = environment_digest(environment.clone()),
    );
    Ok(ShellRuntime {
        path,
        fingerprint,
        environment,
    })
}

/// The runtime a graph's tasks run on. For a Cargo graph that includes the
/// compiler (sykli#64): `cargo` is usually rustup's proxy, which resolves a
/// toolchain from `rust-toolchain.toml` or rustup's default, and moving the
/// default changes neither the shell, `PATH` nor any declared input. So what
/// the task shell resolves `rustc -vV` and `cargo -V` to, in every workdir the
/// graph uses and with the tasks' environment, joins the fingerprint. `run`
/// and `plan --explain` both build their runtime here, so they cannot
/// disagree about a key.
fn graph_runtime(contract: &Contract) -> Result<ShellRuntime, String> {
    let runtime = shell_runtime()?;
    if !is_cargo_graph(contract)? {
        return Ok(runtime);
    }
    let workdirs: BTreeSet<&Path> = contract
        .tasks
        .iter()
        .map(|task| task.workdir.as_deref().unwrap_or_else(|| Path::new(".")))
        .collect();
    let mut reports = Vec::new();
    for workdir in workdirs {
        let output = ProcessCommand::new(&runtime.path)
            .args(["-c", "--", "rustc -vV && cargo -V"])
            .current_dir(workdir)
            .env_clear()
            .envs(
                runtime
                    .environment
                    .iter()
                    .map(|(name, value)| (name, value)),
            )
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("cannot resolve the Rust toolchain: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "cannot resolve the Rust toolchain of this Cargo graph in {}: {}",
                workdir.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        reports.push(serde_json::json!([
            workdir.to_string_lossy(),
            sha256(&output.stdout)
        ]));
    }
    let digest = sha256(&serde_json::to_vec(&reports).map_err(|error| error.to_string())?);
    Ok(ShellRuntime {
        fingerprint: format!("{}:rust:sha256:{digest}", runtime.fingerprint),
        ..runtime
    })
}

fn inherited_environment(
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    // Windows spells PATH as `Path` and needs SystemRoot and friends for any
    // process to start; names there are case-insensitive.
    #[cfg(windows)]
    const INHERITED: &[&str] = &[
        "PATH",
        "HOME",
        "TMPDIR",
        "SYSTEMROOT",
        "SYSTEMDRIVE",
        "WINDIR",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "PATHEXT",
        "COMSPEC",
    ];
    #[cfg(not(windows))]
    const INHERITED: &[&str] = &["PATH", "HOME", "TMPDIR"];
    environment
        .into_iter()
        .filter(|(name, _)| {
            name.to_str().is_some_and(|name| {
                INHERITED.iter().any(|inherited| {
                    if cfg!(windows) {
                        name.eq_ignore_ascii_case(inherited)
                    } else {
                        name == *inherited
                    }
                })
            })
        })
        .collect()
}

/// Git runs hooks with `GIT_EXEC_PATH` set and that directory prepended to
/// `PATH`. Undo exactly that prepend, so a run inside a hook gives its tasks,
/// and its cache keys, the `PATH` the same run has by hand; otherwise the two
/// never share a cached result. Nothing changes outside a hook, or when the
/// first `PATH` entry is not git's.
fn without_git_prepend(
    mut environment: Vec<(OsString, OsString)>,
    git_exec_path: Option<OsString>,
) -> Vec<(OsString, OsString)> {
    let Some(exec_path) = git_exec_path.filter(|path| !path.is_empty()) else {
        return environment;
    };
    for (name, value) in &mut environment {
        let is_path = name
            .to_str()
            .is_some_and(|name| name.eq_ignore_ascii_case("PATH"));
        if !is_path {
            continue;
        }
        let mut entries = std::env::split_paths(value).collect::<Vec<_>>();
        if entries
            .first()
            .is_some_and(|first| first.as_path() == Path::new(&exec_path))
        {
            entries.remove(0);
            if let Ok(joined) = std::env::join_paths(entries) {
                *value = joined;
            }
        }
    }
    environment
}

fn environment_digest(mut environment: Vec<(OsString, OsString)>) -> String {
    environment.sort();
    let mut bytes = Vec::new();
    for (name, value) in environment {
        for part in [name.as_encoded_bytes(), value.as_encoded_bytes()] {
            bytes.extend_from_slice(&part.len().to_le_bytes());
            bytes.extend_from_slice(part);
        }
    }
    sha256(&bytes)
}

fn subject(contract: &Contract, contract_path: &Path) -> Result<Subject, String> {
    let repository = repo::repository().map_err(|_| {
        "not inside a git repository; sykli binds every receipt to a git tree".to_string()
    })?;
    let contract_directory = contract_directory(contract_path)?;
    let contract_inputs = contract_input_records(contract, &contract_directory);
    let declared = input_paths(contract)?;
    let paths = declared
        .iter()
        .map(|(_, _, path)| path.clone())
        .collect::<Vec<_>>();
    let Snapshot {
        tree: tree_oid,
        head_tree: head_tree_oid,
        head_commit,
        facts,
    } = snapshot(repository, &paths)?;
    let inputs = declared_input_records(contract, &declared, &facts);
    let inputs_digest = inputs_digest_of(&inputs)?;
    let submodules_modified = submodules_modified(&repository.root)?;
    let dirty = tree_oid != head_tree_oid || submodules_modified;
    Ok(Subject {
        repository: repository.root.to_string_lossy().into_owned(),
        tree_oid,
        inputs_digest,
        head_tree_oid,
        dirty,
        submodules_modified,
        inputs,
        head_commit,
        contract_directory,
        contract_inputs,
    })
}

/// Whether a submodule has uncommitted changes, tracked or untracked. A tree
/// records each submodule as its gitlink, the commit it is at, so content the
/// submodule has not committed is invisible to the tree that ran.
fn submodules_modified(repository: &Path) -> Result<bool, String> {
    if !repository.join(".gitmodules").is_file() {
        return Ok(false);
    }
    let output = repo::git_command()
        .arg("-C")
        .arg(repository)
        .args(["status", "--porcelain=v2", "-z", "--ignore-submodules=none"])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().into());
    }
    // Changed entries are `1 XY SUB ...`, `2 XY SUB ... PATH` followed by the
    // original path, and `u XY SUB ...`; SUB is `S<c><m><u>` for a submodule,
    // with `M` for tracked and `U` for untracked changes inside it.
    let mut entries = output.stdout.split(|byte| *byte == 0);
    while let Some(entry) = entries.next() {
        let fields = entry.splitn(4, |byte| *byte == b' ').collect::<Vec<_>>();
        if !matches!(fields.first(), Some(&(b"1" | b"2" | b"u"))) {
            continue;
        }
        if let Some(&[b'S', _, tracked, untracked]) = fields.get(2).copied() {
            if tracked == b'M' || untracked == b'U' {
                return Ok(true);
            }
        }
        if fields[0] == b"2" {
            entries.next();
        }
    }
    Ok(false)
}

/// The working tree and HEAD's tree, as a receipt's subject records them:
/// content-addressed, with `.sykli` always excluded so receipts and cache
/// entries never perturb the tree they witness, and a checkout that tracks
/// its own receipts or evidence is not reported dirty.
///
/// One `git cat-file --batch` gives HEAD's tree and whether HEAD tracks
/// `.sykli` (the usual answer is no, and then HEAD's tree is the answer).
/// The working tree is staged into an ephemeral index seeded from HEAD, so
/// tracked files that also match .gitignore stay in the tree as they do in
/// HEAD; `add --all` then stages every change and deletion except under
/// the top-level `.sykli`, and git writes the tree: three more processes, built once per
/// snapshot. A HEAD that tracks `.sykli` costs a removal and one more write.
///
/// The same `cat-file` then reads what the tree just written has at each
/// of `paths` (the declared inputs), from the temporary object directory:
/// git's view of each input the run reads, its mode and blob id, for the
/// receipt to record and `verify --commit` to compare with a commit, and,
/// in one more round only when some input is a symbolic link, the target
/// each link names. An input with no entry is not in the tree (ignored,
/// generated, or spelled differently from git).
fn snapshot(repository: &repo::Repository, paths: &[String]) -> Result<Snapshot, String> {
    let root = &repository.root;
    let state = TemporaryGitState::new(&object_directory_of(repository)?)?;
    let mut session = repository.session(vec![
        (
            "GIT_OBJECT_DIRECTORY",
            state.objects.clone().into_os_string(),
        ),
        ("GIT_ALTERNATE_OBJECT_DIRECTORIES", state.alternates.clone()),
    ]);
    let head = session.ask(&[
        "HEAD^{tree}".into(),
        "HEAD:.sykli".into(),
        "HEAD^{commit}".into(),
    ])?;
    let head_commit = head[2].as_ref().map(|commit| commit.id.clone());
    let Some(head_tree) = &head[0] else {
        return Err("repository has no commits; commit once before running sykli".into());
    };
    git_with_temporary_state(root, &state, &["read-tree", "HEAD"])?;
    let head_tree = match head[1] {
        None => head_tree.id.clone(),
        Some(_) => {
            exclude_sykli(root, &state)?;
            git_with_temporary_state(root, &state, &["write-tree"])?
        }
    };
    // A literal exclusion of an ignored `.sykli` makes `git add` complain
    // that the path is ignored; the same exclusion as a glob does not, and
    // stages exactly what `add --all` and then removing `.sykli` did.
    git_with_temporary_state(
        root,
        &state,
        &[
            "add",
            "--all",
            "--",
            ".",
            ":(exclude,glob).syk[l]i",
            ":(exclude,glob).syk[l]i/**",
        ],
    )?;
    let tree = git_with_temporary_state(root, &state, &["write-tree"])?;
    let names = paths.iter().flat_map(|path| tree_names(&tree, path));
    let mut answers = ask_in(&mut session, names.collect())?;
    let mut facts = PathFacts::default();
    for path in paths {
        facts
            .found
            .insert(path.clone(), found_at(&tree, path, &answers));
    }
    let links = facts.links_to_read(&tree);
    if !links.is_empty() {
        answers.extend(ask_in(&mut session, links)?);
        facts.read_links(&tree, &answers);
    }
    session.finish()?;
    Ok(Snapshot {
        tree,
        head_tree,
        head_commit,
        facts,
    })
}

/// What `snapshot` read: the working tree, HEAD's tree and commit, and
/// what the tree has at each declared input.
struct Snapshot {
    tree: String,
    head_tree: String,
    head_commit: Option<String>,
    facts: PathFacts,
}

/// A commit's tree with `.sykli` excluded, exactly as every receipt tree is.
/// Where `.sykli` is untracked (the usual case) this is `COMMIT^{tree}`. Any
/// tree-ish works, so the pre-commit hook passes the index's `write-tree`.
fn commit_tree_oid(repository: &Path, commit: &str) -> Result<String, String> {
    let state = TemporaryGitState::new(&object_directory()?)?;
    git_with_temporary_state(repository, &state, &["read-tree", commit])
        .and_then(|_| exclude_sykli(repository, &state))
        .and_then(|_| git_with_temporary_state(repository, &state, &["write-tree"]))
}

fn exclude_sykli(repository: &Path, state: &TemporaryGitState) -> Result<String, String> {
    git_with_temporary_state(
        repository,
        state,
        &["rm", "--cached", "-r", "-q", "--ignore-unmatch", ".sykli"],
    )
}

struct TemporaryGitState {
    directory: PathBuf,
    index: PathBuf,
    objects: PathBuf,
    alternates: OsString,
}

impl TemporaryGitState {
    fn new(alternate_objects: &Path) -> Result<Self, String> {
        let temporary = std::env::temp_dir();
        for attempt in 0..100 {
            let directory = temporary.join(format!(
                "sykli-git-{}-{}-{attempt}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|error| error.to_string())?
                    .as_nanos()
            ));
            match fs::create_dir(&directory) {
                Ok(()) => {
                    let objects = directory.join("objects");
                    if let Err(error) = fs::create_dir(&objects) {
                        let _ = fs::remove_dir(&directory);
                        return Err(error.to_string());
                    }
                    return Ok(Self {
                        index: directory.join("index"),
                        objects,
                        alternates: std::env::join_paths([alternate_objects])
                            .map_err(|error| error.to_string())?,
                        directory,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.to_string()),
            }
        }
        Err("could not create temporary Git state".into())
    }
}

impl Drop for TemporaryGitState {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// The repository's object directory, known from finding the repository: an
/// ephemeral index writes its trees beside it, never into it.
fn object_directory() -> Result<PathBuf, String> {
    object_directory_of(repo::repository()?)
}

fn object_directory_of(repository: &repo::Repository) -> Result<PathBuf, String> {
    let objects = repository.object_directory();
    if !objects.is_dir() {
        return Err(format!(
            "Git object directory {} is unavailable",
            objects.display()
        ));
    }
    Ok(objects)
}

/// Settings git reads from the environment that change what a pathspec
/// means. Git exports them to hooks (`git --literal-pathspecs commit` sets
/// `GIT_LITERAL_PATHSPECS`), so the snapshot drops them: its own pathspecs
/// (the glob that leaves `.sykli` out) must mean what they say.
const PATHSPEC_SETTINGS: [&str; 4] = [
    "GIT_LITERAL_PATHSPECS",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
];

fn git_with_temporary_state(
    repository: &Path,
    state: &TemporaryGitState,
    args: &[&str],
) -> Result<String, String> {
    let mut command = repo::git_command();
    command
        .arg("-C")
        .arg(repository)
        .args(args)
        .env("GIT_INDEX_FILE", &state.index)
        .env("GIT_OBJECT_DIRECTORY", &state.objects)
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &state.alternates);
    for setting in PATHSPEC_SETTINGS {
        command.env_remove(setting);
    }
    let output = command.output().map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().into())
}

fn git(args: &[&str]) -> Result<String, String> {
    let output = repo::git_command()
        .args(args)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().into())
}

/// The fields verify checks; unknown receipt fields are deliberately ignored
/// so older sykli binaries can verify receipts from newer ones.
#[derive(Deserialize)]
struct ReceiptSummary {
    schema: String,
    contract_hash: String,
    /// Checked only for a contract outside the repository; None on receipts
    /// that ran without a lock and on receipts written before the field.
    #[serde(default)]
    lock_digest: Option<String>,
    subject: SubjectSummary,
    tasks: Vec<TaskSummary>,
    outcome: String,
}

#[derive(Deserialize)]
struct SubjectSummary {
    tree_oid: String,
    inputs_digest: String,
    /// Read only by `verify --commit`, which refuses a receipt without them.
    #[serde(default)]
    head_tree_oid: Option<String>,
    #[serde(default)]
    dirty: Option<bool>,
    #[serde(default)]
    submodules_modified: bool,
}

#[derive(Deserialize)]
struct TaskSummary {
    name: String,
    command: String,
    outcome: String,
    importable: bool,
    exit_code: Option<i32>,
    error: Option<String>,
    /// Each declared input as the run read it; absent from older receipts.
    #[serde(default)]
    inputs: Option<BTreeMap<String, Option<InputRecord>>>,
    /// Each declared contract input as the run read it.
    #[serde(default)]
    contract_inputs: Option<BTreeMap<String, Option<InputRecord>>>,
}

/// A receipt's records must agree with themselves and with the contract they
/// name: a `passed` task exited 0 without error, a `failed` one did not, and
/// each recorded command is the contract's. This catches corruption and
/// careless forgery; it cannot establish who wrote the receipt.
fn record_contradiction(record: &TaskSummary, task: &Task) -> Option<String> {
    if record.name != task.name {
        return Some(format!("task {} recorded as {}", task.name, record.name));
    }
    if record.command != task.run {
        return Some(format!(
            "task {} ran a different command than the contract declares",
            task.name
        ));
    }
    match record.outcome.as_str() {
        // A cache-restored record carries no exit code of its own; the pass
        // it restores was recorded with exit code 0 at the time.
        "passed" if record.exit_code != Some(0) || record.error.is_some() => Some(format!(
            "task {} is passed but recorded exit code {:?} and error {:?}",
            task.name, record.exit_code, record.error
        )),
        "cached" if !matches!(record.exit_code, None | Some(0)) || record.error.is_some() => {
            Some(format!(
                "task {} is cached but recorded exit code {:?} and error {:?}",
                task.name, record.exit_code, record.error
            ))
        }
        "failed" if record.exit_code == Some(0) => {
            Some(format!("task {} failed with exit code 0", task.name))
        }
        "passed" | "cached" | "failed" | "errored" | "blocked" => None,
        other => Some(format!("task {} has unknown outcome {other:?}", task.name)),
    }
}

struct Check {
    name: &'static str,
    expected: String,
    actual: String,
    ok: bool,
    /// Exit code when this is the first failing stage.
    failure_code: u8,
}

impl Check {
    fn equals(
        name: &'static str,
        failure_code: u8,
        expected: impl Into<String>,
        actual: impl Into<String>,
    ) -> Self {
        let (expected, actual) = (expected.into(), actual.into());
        let ok = expected == actual;
        Check {
            name,
            expected,
            actual,
            ok,
            failure_code,
        }
    }
}

/// Verify proves consistency, not authenticity: the receipt matches this exact
/// working tree and the pinned contract, and its outcome was success. It
/// cannot prove the commands truly ran — that would take attestation.
///
/// With a commit, the subject is that commit and no input is read from the
/// checkout: the contract and its lock pin come from the commit, the
/// receipt's tree must be the commit's tree, and each declared input the run
/// read must be what the commit has at its path (`inputs_check`). A receipt
/// binds a tree, so this is what ties it to a commit someone pushes,
/// whatever is checked out or uncommitted when it is asked. An input the
/// commit cannot show at its path (ignored, untracked, inside a submodule,
/// spelled differently) cannot be judged, so a receipt whose run read one is
/// refused; an input a task outputs is generated by the run.
///
/// A contract outside the repository is no part of any commit, so it and its
/// lock are read from where they are, with or without a commit. It travels
/// with its lock: the receipt must also have run with that lock file.
fn verify(
    receipt_path: &Path,
    contract_path: &Path,
    commit: Option<&str>,
) -> Result<Vec<Check>, String> {
    let Some(rev) = commit else {
        return verify_checkout(receipt_path, contract_path);
    };
    // Everything git knows about the commit, asked before the receipt is
    // read but reported after it, in the order verify always reported.
    let facts = repo::repository()
        .map_err(|_| "not inside a git repository; --commit names a commit".to_string())
        .and_then(|repository| commit_facts(repository, rev, contract_path));
    judge(
        receipt_path,
        facts
            .as_ref()
            .map(|(judged, at)| (judged, at))
            .map_err(String::as_str),
    )
}

/// `verify` without a commit: the subject is the checkout.
fn verify_checkout(receipt_path: &Path, contract_path: &Path) -> Result<Vec<Check>, String> {
    let (receipt, mut checks) = read_receipt(receipt_path)?;
    if checks.iter().any(|check| !check.ok) {
        return Ok(checks);
    }
    let outside = outside_repository(contract_path).is_some();
    let (contract, _, contract_hash) = load_unlocked(contract_path)?;
    let (lock, digest) = read_lock_digest(contract_path)?;
    let beside_contract = outside;
    let outside = outside.then_some((contract_path, digest));
    if !contract_checks(&mut checks, &receipt, &contract_hash, lock, outside) {
        return Ok(checks);
    }
    // A receipt's own input records must hash to its inputs digest, the
    // one the checkout's files are compared with below.
    if let Some(check) = records_check(&receipt)? {
        checks.push(check);
        return Ok(checks);
    }
    let subject = subject(&contract, contract_path)?;
    checks.extend([
        Check::equals(
            "tree",
            TREE_MISMATCH,
            subject.tree_oid,
            &receipt.subject.tree_oid,
        ),
        Check::equals(
            "inputs",
            TREE_MISMATCH,
            subject.inputs_digest,
            &receipt.subject.inputs_digest,
        ),
    ]);
    if checks.iter().any(|check| !check.ok) {
        return Ok(checks);
    }
    // Each record, entry included, must be what the checkout gives now.
    if let Some(check) = checkout_records_check(&receipt, &subject.inputs) {
        checks.push(check);
        return Ok(checks);
    }
    // Outside the repository, the contract inputs as they are beside it now.
    let beside = beside_contract.then_some((
        subject.contract_directory.as_path(),
        &subject.contract_inputs,
    ));
    if let Some(check) = contract_inputs_check(&receipt, &contract, beside) {
        let ok = check.ok;
        checks.push(check);
        if !ok {
            return Ok(checks);
        }
    }
    record_checks(&mut checks, &receipt, &contract);
    Ok(checks)
}

/// For a receipt with per-task input records, judged against the checkout:
/// a failing `inputs` check for the first record that is not what the
/// checkout gives now (`fresh`), the tree entry included, which the inputs
/// digest does not cover; None when every record is.
fn checkout_records_check(receipt: &ReceiptSummary, fresh: &InputRecords) -> Option<Check> {
    let show = |record: Option<&InputRecord>| match record {
        Some(record) => format!(
            "sha256 {}, mode {}, entry {}",
            record.sha256,
            record
                .mode
                .map_or("none".into(), |bits| format!("{bits:#o}")),
            record.object.as_deref().unwrap_or("none")
        ),
        None => "nothing".into(),
    };
    for task in &receipt.tasks {
        let Some(records) = &task.inputs else {
            continue;
        };
        let now = fresh.get(&task.name);
        for (input, record) in records {
            let checkout = now.and_then(|now| now.get(input)).and_then(Option::as_ref);
            if checkout != record.as_ref() {
                return Some(Check {
                    name: "inputs",
                    expected: format!(
                        "task {}'s input {input} as the checkout has it: {}",
                        task.name,
                        show(checkout)
                    ),
                    actual: format!("{} recorded by the run", show(record.as_ref())),
                    ok: false,
                    failure_code: TREE_MISMATCH,
                });
            }
        }
    }
    None
}

/// A receipt and its schema check.
fn read_receipt(receipt_path: &Path) -> Result<(ReceiptSummary, Vec<Check>), String> {
    let receipt: ReceiptSummary =
        serde_json::from_slice(&fs::read(receipt_path).map_err(|error| error.to_string())?)
            .map_err(|error| format!("invalid {}: {error}", receipt_path.display()))?;
    let checks = vec![Check::equals(
        "schema",
        NOT_VERIFIABLE,
        "sykli-receipt.v1",
        &receipt.schema,
    )];
    Ok((receipt, checks))
}

/// The contract a receipt is judged by: its hash, its lock's pin, and for a
/// contract outside the repository its path and lock file digest.
pub(crate) struct Judged {
    contract: Contract,
    hash: String,
    lock: Option<String>,
    outside: Option<(PathBuf, Option<String>)>,
    /// For a contract outside the repository, its directory and the
    /// contract inputs there now; None for one inside it.
    contract_inputs: Option<(PathBuf, InputRecords)>,
}

impl Judged {
    /// A contract read from its file, with the lock beside it.
    fn from_file(path: &Path, outside: bool) -> Result<Self, String> {
        let (contract, _, hash) = load_unlocked(path)?;
        let (lock, digest) = read_lock_digest(path)?;
        let contract_inputs = if outside {
            let directory = contract_directory(path)?;
            let records = contract_input_records(&contract, &directory);
            Some((directory, records))
        } else {
            None
        };
        Ok(Judged {
            contract,
            hash,
            lock,
            outside: outside.then(|| (path.to_path_buf(), digest)),
            contract_inputs,
        })
    }

    /// Whether the contract differs from its own lock pin: then no run can
    /// pass until it is re-locked.
    pub(crate) fn drifted(&self) -> bool {
        self.lock.as_ref().is_some_and(|lock| lock != &self.hash)
    }
}

/// A commit as `verify --commit` judges it: its id, and its tree with
/// `.sykli` excluded, as every receipt tree is.
pub(crate) struct CommitAt {
    id: String,
    tree: String,
    /// The contract's declared inputs as this commit stores them, read in a
    /// second round once the contract is known.
    inputs: Option<InputsAt>,
}

/// What one `git cat-file --batch` said about each name asked of it.
pub(crate) type Answers = HashMap<String, Option<repo::Object>>;

/// Ask every name once, as one round of a session that may be asked again,
/// and keep the answers by name.
pub(crate) fn ask_in(session: &mut repo::Session, names: Vec<String>) -> Result<Answers, String> {
    ask_in_bounded(session, names, usize::MAX)
}

/// The most of a declared input's blob that is kept when a commit's inputs
/// are read: enough for a symbolic link's target (a path), while a larger
/// file is only hashed as it is read, so a large input costs time, never
/// its size in memory.
pub(crate) const INPUT_BLOB_KEPT: usize = 64 * 1024;

/// `ask_in`, keeping a blob's contents only up to `keep` bytes (each blob
/// is hashed either way: `repo::Object::sha256`).
pub(crate) fn ask_in_bounded(
    session: &mut repo::Session,
    names: Vec<String>,
    keep: usize,
) -> Result<Answers, String> {
    let mut seen = HashSet::new();
    let unique = names
        .into_iter()
        .filter(|name| seen.insert(name.clone()))
        .collect::<Vec<_>>();
    let answers = session.ask_bounded(&unique, keep)?;
    Ok(unique.into_iter().zip(answers).collect())
}

/// The answer to `name`, when it was asked and names an object.
pub(crate) fn answer<'a>(answers: &'a Answers, name: &str) -> Option<&'a repo::Object> {
    answers.get(name).and_then(Option::as_ref)
}

/// The names to ask `git cat-file --batch` about a commit, `REV`: the commit,
/// its tree and whether it tracks `.sykli`. Their answers give a `CommitAt`.
pub(crate) fn commit_names(rev: &str) -> [String; 3] {
    [
        format!("{rev}^{{commit}}"),
        format!("{rev}^{{tree}}"),
        format!("{rev}:.sykli"),
    ]
}

/// The commit `commit_names(rev)` asked about, from git's answers. Its tree
/// is the commit's own unless it tracks `.sykli` (rare), which is then left
/// out in an ephemeral index.
pub(crate) fn commit_from(rev: &str, answers: &Answers) -> Result<CommitAt, String> {
    let [commit, tree, sykli] = commit_names(rev).map(|name| answer(answers, &name));
    let (Some(commit), Some(tree)) = (commit, tree) else {
        return Err(format!("{rev:?} does not name a commit in this repository"));
    };
    let tree = match sykli {
        None => tree.id.clone(),
        Some(_) => commit_tree_oid(&repo::repository()?.root, &commit.id)?,
    };
    Ok(CommitAt {
        id: commit.id.clone(),
        tree,
        inputs: None,
    })
}

/// What a tree has at a path, looked up by the path's exact bytes, as
/// `REV:PATH` names it: git never folds case or Unicode normalization, nor
/// follows a symbolic link or enters a submodule on the way.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Found {
    /// The entry at the path, `MODE ID` as git writes a tree.
    Entry(String),
    /// Nothing at the path, and as far as the tree shows, why.
    Absent(Absence),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Absence {
    /// The directory that would hold it has no entry by that name, nor one
    /// that differs from it only in case.
    Nothing,
    /// An entry whose name differs only in case: the path as git spells it.
    Spelled(String),
    /// A part of the path is not a directory in the tree: that part's path
    /// and its entry, a submodule (`160000`), a symbolic link (`120000`) or a
    /// file.
    Under(String, String),
}

impl Absence {
    /// Why a tree has nothing at `path`, in words.
    fn reason(&self, path: &str) -> String {
        match self {
            Absence::Nothing => format!(
                "nothing at {path}: a file it does not track (deleted, ignored, generated or \
                 untracked), or one it spells differently (such as by Unicode normalization)"
            ),
            Absence::Spelled(spelled) => {
                format!("nothing at {path}; it spells that file {spelled}")
            }
            Absence::Under(part, entry) => format!(
                "nothing at {path}: {part} is {} there, which git does not look inside",
                kind_of(entry)
            ),
        }
    }
}

/// What a tree entry is, in words.
fn kind_of(entry: &str) -> String {
    let (mode, id) = entry.split_once(' ').unwrap_or((entry, ""));
    match mode {
        "160000" => format!("a submodule, recorded only as its commit {id}"),
        "120000" => "a symbolic link".into(),
        "40000" => "a directory".into(),
        _ => "a file".into(),
    }
}

/// Whether `entry` is a regular file's: `100644`, or `100755` (executable).
fn is_file(entry: &str) -> bool {
    entry.starts_with("100644 ") || entry.starts_with("100755 ")
}

/// The names to ask `git cat-file --batch` to find `path` in `tree_ish` by
/// its exact bytes: the tree of every directory on the way, from the top.
fn tree_names(tree_ish: &str, path: &str) -> Vec<String> {
    let parts = path.split('/').collect::<Vec<_>>();
    let mut names = vec![format!("{tree_ish}^{{tree}}")];
    names.extend((1..parts.len()).map(|end| format!("{tree_ish}:{}", parts[..end].join("/"))));
    names
}

/// What `tree_ish` has at `path`, from the answers to `tree_names`.
fn found_at(tree_ish: &str, path: &str, answers: &Answers) -> Found {
    let parts = path.split('/').collect::<Vec<_>>();
    let mut tree = format!("{tree_ish}^{{tree}}");
    for (index, part) in parts.iter().enumerate() {
        let Some(object) = answer(answers, &tree).filter(|object| object.kind == "tree") else {
            break;
        };
        let entries = repo::tree_entries(&object.bytes, object.id.len() / 2);
        let Some((_, entry)) = entries.iter().find(|(name, _)| name == part) else {
            let folded = part.to_lowercase();
            let spelled = entries
                .iter()
                .find(|(name, _)| name.to_lowercase() == folded)
                .map(|(name, _)| {
                    let mut spelled = parts.clone();
                    spelled[index] = name;
                    Absence::Spelled(spelled.join("/"))
                });
            return Found::Absent(spelled.unwrap_or(Absence::Nothing));
        };
        let here = parts[..=index].join("/");
        if index + 1 == parts.len() {
            return Found::Entry(entry.clone());
        }
        if !entry.starts_with("40000 ") {
            return Found::Absent(Absence::Under(here, entry.clone()));
        }
        tree = format!("{tree_ish}:{here}");
    }
    Found::Absent(Absence::Nothing)
}

/// Where a symbolic link at `link` naming `target` points, from the top
/// level, by the names alone, or why the names alone cannot say. A `..`
/// may leave only the link's own directories, which the tree shows are
/// directories (the link was found by walking through them). A `..` after
/// a name the target itself gives is resolved by the file system through
/// whatever that name is, a symbolic link to a directory included, so it is
/// refused, as are an absolute target and one that leaves the repository.
fn link_target(link: &str, target: &str) -> Result<String, String> {
    let outside = || "outside the repository".to_string();
    if target.is_empty() || target.starts_with('/') {
        return Err(outside());
    }
    let mut parts = link.split('/').collect::<Vec<_>>();
    parts.pop();
    let mut named = false;
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." if named => {
                return Err(
                    "a `..` after a name in the target, which the file system resolves \
                     through whatever that name is (a symbolic link included), so the \
                     names alone cannot say which file is read"
                        .into(),
                );
            }
            ".." => {
                parts.pop().ok_or_else(outside)?;
            }
            part => {
                parts.push(part);
                named = true;
            }
        }
    }
    if parts.is_empty() {
        return Err(outside());
    }
    Ok(parts.join("/"))
}

/// What one tree has at every declared input's path, and the target each
/// symbolic link among them names.
#[derive(Default)]
struct PathFacts {
    found: HashMap<String, Found>,
    links: HashMap<String, String>,
    /// What the tree has where a declared symbolic link leads, for each
    /// target no task declares (`link_target_names`): whether a link judged
    /// by its own text leads to a file.
    around: HashMap<String, Found>,
}

impl PathFacts {
    /// The names to ask for the targets of the symbolic links found.
    fn links_to_read(&self, tree_ish: &str) -> Vec<String> {
        self.found
            .iter()
            .filter(
                |(_, found)| matches!(found, Found::Entry(entry) if entry.starts_with("120000 ")),
            )
            .map(|(path, _)| format!("{tree_ish}:{path}"))
            .collect()
    }

    /// Keep each symbolic link's target from the answers to `links_to_read`.
    fn read_links(&mut self, tree_ish: &str, answers: &Answers) {
        for (path, found) in &self.found {
            let Found::Entry(entry) = found else { continue };
            let id = entry.split(' ').nth(1).unwrap_or_default();
            if let Some(object) = answer(answers, &format!("{tree_ish}:{path}"))
                .filter(|object| entry.starts_with("120000 ") && object.id == id)
            {
                self.links.insert(
                    path.clone(),
                    String::from_utf8_lossy(&object.bytes).into_owned(),
                );
            }
        }
    }

    /// The regular file whose bytes a read of the declared input at `path`
    /// gives, as the tree has it: the input's own entry, or for a symbolic
    /// link the declared input it names, followed through further declared
    /// links. Its path and entry, or why a read of `path` cannot be traced
    /// to a file in the tree.
    fn file_behind(&self, path: &str) -> Result<(String, String), String> {
        let mut at = path.to_string();
        let mut seen = HashSet::new();
        while seen.insert(at.clone()) {
            let found = self
                .found
                .get(&at)
                .ok_or_else(|| format!("{at} was not looked up"))?;
            let entry = match found {
                Found::Entry(entry) => entry,
                Found::Absent(absence) => return Err(absence.reason(&at)),
            };
            if is_file(entry) {
                return Ok((at, entry.clone()));
            }
            if !entry.starts_with("120000 ") {
                return Err(format!("{at} is {}", kind_of(entry)));
            }
            let target = self
                .links
                .get(&at)
                .ok_or_else(|| format!("the target of the symbolic link {at} was not read"))?;
            let next = link_target(&at, target)
                .map_err(|why| format!("{at} is a symbolic link to {target}: {why}"))?;
            if !self.found.contains_key(&next) {
                return Err(format!(
                    "{at} is a symbolic link to {next}, which no task declares; declare {next} \
                     as well, so the bytes read through the link can be judged"
                ));
            }
            at = next;
        }
        Err(format!("{path} is in a loop of symbolic links"))
    }
}

/// Every task's declared inputs as one commit stores them, read from git,
/// never from the receipt being judged: each input's path from the top
/// level, what the commit has there, and the bytes of every file and
/// symbolic link among them, all from the second round of the one
/// `git cat-file --batch` that read the commit.
pub(crate) struct InputsAt {
    /// Each task's inputs, as declared, with their paths from the top level.
    tasks: BTreeMap<String, BTreeMap<String, String>>,
    facts: PathFacts,
    /// The SHA-256 of the bytes the commit stores at each declared path
    /// that is a file or a symbolic link (the target's name).
    stored: HashMap<String, String>,
    /// Each path a task declares as an output, by that task: an input there
    /// is generated by the run.
    outputs: Vec<(String, String)>,
    /// The commit itself, whose attributes decide how a checkout converts
    /// an input (`checked_out`).
    commit: String,
    /// What a checkout of the commit gives for a blob at a path, by path
    /// and blob: one `git cat-file --filters` each, only when the bytes the
    /// run read are not the stored ones.
    checkouts: std::cell::RefCell<HashMap<(String, String), String>>,
}

/// Each declared input of `contract`: its task, the input as declared, and
/// its path from the top level (`declared_path`: from the current
/// directory, through the task's workdir).
fn input_paths(contract: &Contract) -> Result<Vec<(String, String, String)>, String> {
    let repository = repo::repository()?;
    let mut paths = Vec::new();
    for task in &contract.tasks {
        let root = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
        for input in &task.inputs {
            let path = declared_path(repository, &root.join(input))?;
            paths.push((task.name.clone(), input.clone(), path));
        }
    }
    Ok(paths)
}

/// A declared path from the repository's top level, by its names alone:
/// from where the current directory is in the repository, with `.` and `..`
/// taken as names and nothing below the top level resolved on disk. A
/// symbolic link in the checkout (a directory replaced by a link to
/// another) therefore cannot make a declared input name another path than
/// the one a commit is asked about.
fn declared_path(repository: &repo::Repository, path: &Path) -> Result<String, String> {
    let full = absolute(path)?;
    let root = repository.root_here()?;
    full.strip_prefix(&root)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .map_err(|_| {
            format!(
                "{} is outside the repository at {}",
                path.display(),
                repository.root.display()
            )
        })
}

/// The names to ask about `contract`'s declared inputs in commit `rev`:
/// the trees on the way to each, and the object at each path.
pub(crate) fn input_names(rev: &str, contract: &Contract) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for (_, _, path) in input_paths(contract)? {
        names.extend(tree_names(rev, &path));
        names.push(format!("{rev}:{path}"));
    }
    Ok(names)
}

/// The names to ask once the answers to `input_names(rev, contract)` are
/// in: for each declared symbolic link whose target no task declares, the
/// trees on the way to where it leads, so a link judged by its own text can
/// be seen to lead to no file (`InputsAt::judge_link`). Empty, and no round
/// asked, when no declared input is such a link.
pub(crate) fn link_target_names(
    rev: &str,
    contract: &Contract,
    answers: &Answers,
) -> Result<Vec<String>, String> {
    let declared = input_paths(contract)?
        .into_iter()
        .map(|(_, _, path)| path)
        .collect::<BTreeSet<_>>();
    let mut names = BTreeSet::new();
    for path in &declared {
        let Found::Entry(entry) = found_at(rev, path, answers) else {
            continue;
        };
        let id = entry.split(' ').nth(1).unwrap_or_default();
        let text = answer(answers, &format!("{rev}:{path}")).filter(|object| {
            entry.starts_with("120000 ")
                && object.kind == "blob"
                && object.id == id
                && object.sha256.as_deref() == Some(sha256(&object.bytes).as_str())
        });
        let target = text
            .and_then(|object| link_target(path, &String::from_utf8_lossy(&object.bytes)).ok())
            .filter(|target| !declared.contains(target));
        if let Some(target) = target {
            names.extend(tree_names(rev, &target));
        }
    }
    Ok(names.into_iter().collect())
}

/// The inputs `input_names(rev, contract)` asked about, from git's answers.
pub(crate) fn inputs_from(
    rev: &str,
    contract: &Contract,
    answers: &Answers,
) -> Result<InputsAt, String> {
    let repository = repo::repository()?;
    let mut facts = PathFacts::default();
    let mut stored = HashMap::new();
    let mut tasks: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (task, input, path) in input_paths(contract)? {
        if !facts.found.contains_key(&path) {
            let found = found_at(rev, &path, answers);
            if let Found::Entry(entry) = &found {
                let id = entry.split(' ').nth(1).unwrap_or_default();
                if let Some(object) = answer(answers, &format!("{rev}:{path}"))
                    .filter(|object| object.kind == "blob" && object.id == id)
                {
                    if let Some(digest) = &object.sha256 {
                        stored.insert(path.clone(), digest.clone());
                        // A link's target is its blob, kept whole when it
                        // is no longer than a path; one that was not kept
                        // is never followed.
                        if entry.starts_with("120000 ") && &sha256(&object.bytes) == digest {
                            facts.links.insert(
                                path.clone(),
                                String::from_utf8_lossy(&object.bytes).into_owned(),
                            );
                        }
                    }
                }
            }
            facts.found.insert(path.clone(), found);
        }
        tasks.entry(task).or_default().insert(input, path);
    }
    // Where each declared link leads, when no task declares that path, and
    // only when `link_target_names` asked for it: one not asked is never
    // taken to lead to nothing.
    let mut around = HashMap::new();
    for (path, text) in &facts.links {
        let Ok(target) = link_target(path, text) else {
            continue;
        };
        let asked = tree_names(rev, &target)
            .iter()
            .all(|name| answers.contains_key(name));
        if !facts.found.contains_key(&target) && asked {
            let found = found_at(rev, &target, answers);
            around.insert(target, found);
        }
    }
    facts.around = around;
    let mut outputs = Vec::new();
    for task in &contract.tasks {
        let root = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
        for output in &task.outputs {
            if let Ok(path) = declared_path(repository, &root.join(output)) {
                outputs.push((path, task.name.clone()));
            }
        }
    }
    let commit = answer(answers, &format!("{rev}^{{commit}}"))
        .map_or_else(|| rev.to_string(), |commit| commit.id.clone());
    Ok(InputsAt {
        commit,
        tasks,
        facts,
        stored,
        outputs,
        checkouts: std::cell::RefCell::new(HashMap::new()),
    })
}

/// How one declared input fared against the commit.
enum Judgment {
    /// What the run read there is what the commit has.
    Judged,
    /// What the run read there is what a checkout here makes of the
    /// commit's blob (line endings, a smudge filter): that file's path.
    Converted(String),
    /// The commit has nothing there, and a task declares it as an output:
    /// the run generated it. Its bytes are the receipt's claim; only the
    /// inputs of the task that produces it are judged.
    Generated(String),
    /// Refused: what the commit has, and what the receipt says the run read.
    Refused(String, String),
}

impl InputsAt {
    /// The task that declares `path`, or a directory holding it, as an
    /// output.
    fn producer(&self, path: &str) -> Option<&str> {
        self.outputs
            .iter()
            .find(|(output, _)| {
                path == output
                    || path
                        .strip_prefix(output.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            })
            .map(|(_, task)| task.as_str())
    }

    /// The SHA-256 of what a checkout of the commit gives for blob `id` at
    /// `path`: `git --attr-source=COMMIT cat-file --filters`, which applies
    /// the line-ending conversion and smudge filter that the commit's own
    /// `.gitattributes` and this repository's configuration (including
    /// `.git/info/attributes` and `core.attributesFile`) give that path, as
    /// a checkout of the commit would; the checkout's own `.gitattributes`,
    /// edited or not, plays no part. Hashed as it is read, once per path and
    /// blob. `--attr-source` needs git 2.40 or later; an older git cannot
    /// do this, and that is an error (exit 2), never a pass.
    fn checked_out(&self, path: &str, id: &str) -> Result<String, String> {
        let key = (path.to_string(), id.to_string());
        if let Some(digest) = self.checkouts.borrow().get(&key) {
            return Ok(digest.clone());
        }
        let repository = repo::repository()?;
        let mut child = repo::git_command()
            .arg("-C")
            .arg(&repository.root)
            .arg(format!("--attr-source={}", self.commit))
            .args(["cat-file", "--filters", &format!("--path={path}"), id])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| error.to_string())?;
        let mut stderr = child.stderr.take().ok_or("git cat-file has no stderr")?;
        let reason = thread::spawn(move || {
            let mut reason = String::new();
            let _ = stderr.read_to_string(&mut reason);
            reason
        });
        let mut stdout = child.stdout.take().ok_or("git cat-file has no stdout")?;
        let mut hasher = Sha256::new();
        let mut piece = [0; 64 * 1024];
        let read = loop {
            match stdout.read(&mut piece) {
                Ok(0) => break Ok(()),
                Ok(count) => hasher.update(&piece[..count]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => break Err(error.to_string()),
            }
        };
        drop(stdout);
        let status = child.wait().map_err(|error| error.to_string())?;
        let reason = reason.join().unwrap_or_default();
        let reason = reason.trim();
        read?;
        if !status.success() {
            let needs = if reason.contains("attr-source") {
                " (checking out an input the way a commit's attributes say needs git 2.40 or \
                 later, for --attr-source)"
            } else {
                ""
            };
            return Err(format!(
                "git cat-file --filters cannot check out {path} as commit {} would{needs}: {reason}",
                &self.commit[..self.commit.len().min(12)]
            ));
        }
        let digest = format!("{:x}", hasher.finalize());
        self.checkouts.borrow_mut().insert(key, digest.clone());
        Ok(digest)
    }

    /// Judge a record of a symbolic link's own text (`InputRecord::link`),
    /// already found to name the commit's entry at `path`: the commit must
    /// have a link there with that text, leading, by its names, inside the
    /// repository to no regular file (a directory, a submodule, nothing, or
    /// a link a task declares, judged on its own), as the run found it. A
    /// link to a file is judged by the file, never by its text: a run reads
    /// the file. A link out of the repository is refused.
    fn judge_link(&self, path: &str, entry: &str, record: &InputRecord, short: &str) -> Judgment {
        let read = format!(
            "the text of a symbolic link (sha256 {}) recorded by the run",
            record.sha256
        );
        if !entry.starts_with("120000 ") {
            return Judgment::Refused(format!("{path} as commit {short} has it: {entry}"), read);
        }
        if self.stored.get(path) != Some(&record.sha256) {
            return Judgment::Refused(
                format!(
                    "the text of the symbolic link {path} in commit {short} (sha256 {})",
                    self.stored.get(path).map_or("unread", String::as_str)
                ),
                read,
            );
        }
        let Some(text) = self.facts.links.get(path) else {
            return Judgment::Refused(
                format!("the target of the symbolic link {path} in commit {short}, unread"),
                read,
            );
        };
        // A link out of the repository leads where no commit can show, so
        // its text vouches for nothing a run read: refused, as a link to a
        // file outside the repository always was.
        let target = match link_target(path, text) {
            Ok(target) => target,
            Err(why) => {
                return Judgment::Refused(
                    format!("where {path} leads in commit {short}: to {text}, {why}"),
                    read,
                );
            }
        };
        let found = match self.facts.found.get(&target) {
            Some(found) => Some((found, true)),
            None => self.facts.around.get(&target).map(|found| (found, false)),
        };
        match found {
            None => Judgment::Refused(
                format!("where {path} leads in commit {short}: {target}, which was not looked up"),
                read,
            ),
            Some((Found::Entry(at), _)) if is_file(at) => Judgment::Refused(
                format!(
                    "{path} leads to {target}, a file in commit {short}, which a run reads \
                     rather than the link's text"
                ),
                read,
            ),
            Some((Found::Entry(at), false)) if at.starts_with("120000 ") => Judgment::Refused(
                format!(
                    "{path} leads to the symbolic link {target} in commit {short}, which no task \
                     declares; declare it too, so where {path} leads can be judged"
                ),
                read,
            ),
            Some(_) => Judgment::Judged,
        }
    }

    /// Judge the declared input at `path` by the record a run wrote for it
    /// (None: the run found nothing it could read there).
    fn judge(
        &self,
        path: &str,
        record: Option<&InputRecord>,
        short: &str,
    ) -> Result<Judgment, String> {
        let found = self
            .facts
            .found
            .get(path)
            .ok_or_else(|| format!("{path} was not read from commit {short}"))?;
        let Some(record) = record else {
            return Ok(match found {
                Found::Absent(_) => Judgment::Judged,
                Found::Entry(entry) => Judgment::Refused(
                    format!("{path} as commit {short} has it: {entry}"),
                    "nothing the run could read there".into(),
                ),
            });
        };
        let entry = match found {
            Found::Absent(Absence::Nothing) if self.producer(path).is_some() => {
                return Ok(Judgment::Generated(
                    self.producer(path).unwrap_or_default().to_string(),
                ));
            }
            Found::Absent(absence) => {
                return Ok(Judgment::Refused(
                    format!(
                        "{path} judged against commit {short}, which has {}",
                        absence.reason(path)
                    ),
                    format!(
                        "a file the run read there (sha256 {}); a commit can only vouch for an \
                         input it has at the path declared",
                        record.sha256
                    ),
                ));
            }
            Found::Entry(entry) => entry,
        };
        if record.object.as_deref() != Some(entry) {
            return Ok(Judgment::Refused(
                format!("{path} as commit {short} has it: {entry}"),
                format!(
                    "{} in the tree the task ran",
                    record.object.as_deref().unwrap_or("nothing")
                ),
            ));
        }
        if record.link {
            return Ok(self.judge_link(path, entry, record, short));
        }
        // A symbolic link is judged only by the file it leads to. A checkout
        // without symbolic links (`core.symlinks=false`) holds a file with
        // the target's name instead, other bytes than the link gives; such a
        // run is refused, and the refusal says why.
        let read = if entry.starts_with("120000 ") && self.stored.get(path) == Some(&record.sha256)
        {
            format!(
                "the link's own text (sha256 {}), as a checkout without symbolic links \
                 (core.symlinks=false) reads it",
                record.sha256
            )
        } else {
            format!("a file the run read there (sha256 {})", record.sha256)
        };
        let (file, file_entry) = match self.facts.file_behind(path) {
            Ok(file) => file,
            Err(reason) => {
                return Ok(Judgment::Refused(
                    format!("the file a read of {path} gives in commit {short}: {reason}"),
                    read,
                ));
            }
        };
        let stored = self
            .stored
            .get(&file)
            .ok_or_else(|| format!("the bytes of {file} were not read from commit {short}"))?;
        let converted = stored != &record.sha256;
        if converted {
            let id = file_entry.split(' ').nth(1).unwrap_or_default();
            let checked_out = self.checked_out(&file, id)?;
            if checked_out != record.sha256 {
                return Ok(Judgment::Refused(
                    format!(
                        "the bytes of {file} in commit {short}: sha256 {stored} as stored, \
                         {checked_out} as a checkout here gives them"
                    ),
                    if file == path {
                        format!("{} recorded by the run", record.sha256)
                    } else {
                        format!("{read}, through {path}")
                    },
                ));
            }
        }
        let executable = file_entry.starts_with("100755 ");
        if record
            .mode
            .is_none_or(|bits| (bits & 0o100 != 0) != executable)
        {
            return Ok(Judgment::Refused(
                format!(
                    "the execute bit of {file} in commit {short}: {}",
                    if executable {
                        "executable (100755)"
                    } else {
                        "not executable (100644)"
                    }
                ),
                match record.mode {
                    Some(bits) => format!("mode {bits:#o} recorded by the run"),
                    None => "no execute bits recorded by the run".into(),
                },
            ));
        }
        Ok(if converted {
            Judgment::Converted(file)
        } else {
            Judgment::Judged
        })
    }
}

/// The `inputs` stage of `verify --commit`, read from the commit, never
/// taken from the receipt. A receipt records, for each task's declared
/// inputs, what its run read: the SHA-256 of the bytes, the execute bits,
/// and the tree entry its own snapshot had at that path. Each is judged
/// against the commit at the input's exact path (`InputsAt::judge`): the
/// entry must be the commit's; the bytes must be the commit's blob, or what
/// a checkout here makes of it (line endings, smudge filters); the execute
/// bit must be the commit's. A symbolic link is judged by its entry and by
/// the bytes of the declared input it names. An input the commit does not
/// have at that path cannot be judged, and a receipt whose run read a file
/// there is refused, unless a task declares the path as an output (the run
/// generated it). The records must hash to the receipt's inputs digest. An
/// older receipt, which records only that digest, must match the digest
/// recomputed from the commit's files. A mismatch is a tree-or-input
/// mismatch: exit 3, as without `--commit`.
fn inputs_check(
    receipt: &ReceiptSummary,
    contract: &Contract,
    at: &CommitAt,
) -> Result<Check, String> {
    let short = &at.id[..at.id.len().min(12)];
    let inputs = at
        .inputs
        .as_ref()
        .ok_or_else(|| format!("the inputs of commit {short} were not read"))?;
    let refuse = |expected: String, actual: String| Check {
        name: "inputs",
        expected,
        actual,
        ok: false,
        failure_code: TREE_MISMATCH,
    };
    let recorded = receipt
        .tasks
        .iter()
        .filter_map(|task| {
            task.inputs
                .as_ref()
                .map(|inputs| (task.name.as_str(), inputs))
        })
        .collect::<HashMap<_, _>>();
    if recorded.is_empty() {
        return older_inputs_check(receipt, contract, inputs, short);
    }
    let mut judged = 0;
    let mut converted = BTreeSet::new();
    let mut generated = BTreeSet::new();
    for task in &contract.tasks {
        let Some(records) = recorded.get(task.name.as_str()) else {
            if receipt.tasks.iter().any(|record| record.name == task.name) {
                return Ok(refuse(
                    format!("task {}'s input records", task.name),
                    "none recorded".into(),
                ));
            }
            continue;
        };
        let declared = task.inputs.iter().collect::<BTreeSet<_>>();
        if records.keys().collect::<BTreeSet<_>>() != declared {
            return Ok(refuse(
                format!(
                    "task {}'s inputs as the contract declares them, {declared:?}",
                    task.name
                ),
                format!("{:?}", records.keys().collect::<Vec<_>>()),
            ));
        }
        for (input, path) in inputs.tasks.get(&task.name).into_iter().flatten() {
            let record = records.get(input).and_then(Option::as_ref);
            match inputs.judge(path, record, short)? {
                Judgment::Judged => judged += 1,
                Judgment::Converted(file) => {
                    judged += 1;
                    converted.insert(file);
                }
                Judgment::Generated(by) => {
                    generated.insert(format!("{path} (an output of task {by})"));
                }
                Judgment::Refused(expected, actual) => {
                    return Ok(refuse(
                        format!("task {}'s input {input}: {expected}", task.name),
                        actual,
                    ));
                }
            }
        }
    }
    if let Some(check) = records_check(receipt)? {
        return Ok(check);
    }
    let mut actual = format!(
        "{}: {judged} declared input{} judged against commit {short}",
        receipt.subject.inputs_digest,
        if judged == 1 { "" } else { "s" }
    );
    if !converted.is_empty() {
        let converted = converted.into_iter().collect::<Vec<_>>().join(", ");
        actual.push_str(&format!(
            "; the bytes of {converted} as a checkout of the commit converts them, by its \
             attributes and this repository's configuration"
        ));
    }
    if !generated.is_empty() {
        let generated = generated.into_iter().collect::<Vec<_>>().join(", ");
        actual.push_str(&format!(
            "; not judged: generated by the run and not in the commit, so their bytes are the \
             receipt's claim (the inputs of the tasks that produce them are judged): {generated}"
        ));
    }
    Ok(Check {
        name: "inputs",
        expected: actual.clone(),
        actual,
        ok: true,
        failure_code: TREE_MISMATCH,
    })
}

/// For a receipt with per-task input records, a failing `inputs` check when
/// they do not hash to the receipt's own inputs digest; None when they do,
/// or when it has none (an older receipt).
fn records_check(receipt: &ReceiptSummary) -> Result<Option<Check>, String> {
    let records = receipt
        .tasks
        .iter()
        .filter_map(|task| {
            task.inputs
                .clone()
                .map(|inputs| (task.name.clone(), inputs))
        })
        .collect::<InputRecords>();
    if records.is_empty() {
        return Ok(None);
    }
    let digest = inputs_digest_of(&records)?;
    Ok((digest != receipt.subject.inputs_digest).then(|| Check {
        name: "inputs",
        expected: format!("{digest} (the digest of the receipt's own input records)"),
        actual: receipt.subject.inputs_digest.clone(),
        ok: false,
        failure_code: TREE_MISMATCH,
    }))
}

/// What a verified receipt establishes, and what it cannot, printed after
/// every successful verify and stated in `verify --help`.
const TRUST: &str = "established: a valid receipt schema; records consistent with each other and \
     the contract; the contract's identity; the tree, and every declared input's bytes and execute \
     bit, read from the checkout, or with --commit from the commit (save an input the run itself \
     generates), never from the receipt (an input a checkout converts, as a checkout of the commit \
     converts it); every contract input's, read from beside a contract outside the repository, \
     with or without --commit\n\
     not established: where the receipt came from, who produced it, that its tasks ran, or \
     anything they read that is not a declared input. Receipts are unsigned: an outcome rewritten \
     to passed, or a receipt recomputed in full for a commit with consistent digests, verifies; \
     with --commit, the bytes of an input a task outputs are the receipt's claim (only that \
     task's own inputs are established); and a converted input is bound only as the commit's \
     attributes and this repository's configuration convert it";

/// `inputs_check` for a receipt written before per-task input records: its
/// inputs digest is recomputed from the bytes of the commit's files. That
/// digest folds in each file's execute bits as the run's file system had
/// them, which a commit records only as executable or not, so the usual
/// executable modes are tried for an executable file (`0o111`, `0o110`,
/// `0o100`) and none for the others: never an execute bit the commit does
/// not have. A file a checkout converts (line endings, smudge filters) was
/// read converted, and where the run's file system had no execute bits
/// (Windows) an executable file was read as not executable, so such a
/// receipt cannot be shown to match and is refused, as is one with a
/// declared input that is not a regular file in the commit; a new run
/// records what this check needs.
fn older_inputs_check(
    receipt: &ReceiptSummary,
    contract: &Contract,
    inputs: &InputsAt,
    short: &str,
) -> Result<Check, String> {
    let refuse = |expected: String, actual: String| Check {
        name: "inputs",
        expected,
        actual,
        ok: false,
        failure_code: TREE_MISMATCH,
    };
    let mut files = InputRecords::new();
    for task in &contract.tasks {
        let records = files.entry(task.name.clone()).or_default();
        for (input, path) in inputs.tasks.get(&task.name).into_iter().flatten() {
            let regular = match inputs.facts.found.get(path) {
                Some(Found::Entry(entry)) if is_file(entry) => Some(entry),
                _ => None,
            };
            let Some(entry) = regular else {
                return Ok(refuse(
                    format!(
                        "task {}'s input {input} as a file in commit {short} ({path}), to recompute \
                         the inputs of a receipt that predates per-task input records",
                        task.name
                    ),
                    "not a file there; run sykli again for a receipt that records its inputs"
                        .into(),
                ));
            };
            let sha256 = inputs
                .stored
                .get(path)
                .cloned()
                .ok_or_else(|| format!("commit {short} has no object {entry}"))?;
            // The execute bits are filled in per attempt below.
            let mode = entry.starts_with("100755 ").then_some(1);
            records.insert(
                input.clone(),
                Some(InputRecord {
                    sha256,
                    mode,
                    object: None,
                    link: false,
                }),
            );
        }
    }
    let mut first = None;
    for bits in [0o111, 0o110, 0o100] {
        let mut attempt = files.clone();
        for record in attempt
            .values_mut()
            .flat_map(BTreeMap::values_mut)
            .flatten()
        {
            record.mode = Some(if record.mode.is_some() { bits } else { 0 });
        }
        let digest = inputs_digest_of(&attempt)?;
        if digest == receipt.subject.inputs_digest {
            return Ok(Check::equals(
                "inputs",
                TREE_MISMATCH,
                digest,
                &receipt.subject.inputs_digest,
            ));
        }
        first.get_or_insert(digest);
    }
    Ok(refuse(
        format!(
            "{} (recomputed from the files of commit {short})",
            first.unwrap_or_default()
        ),
        receipt.subject.inputs_digest.clone(),
    ))
}

/// A file's path in the repository, from its top level, as `REV:PATH` names
/// it in a commit. An absolute path is compared where it is on disk, so a
/// spelling through a symbolic link (macOS's /var for /private/var) names
/// the same file; a relative one is relative to the current directory, as
/// `--contract` is.
fn path_in_repository(repository: &repo::Repository, path: &Path) -> Result<String, String> {
    let outside = || {
        format!(
            "{} is outside the repository at {}",
            path.display(),
            repository.root.display()
        )
    };
    let full = absolute(path)?;
    let relative = match (
        full.parent().and_then(located),
        full.file_name(),
        repository.resolved(),
    ) {
        (Some(parent), Some(name), Some(root)) => parent
            .join(name)
            .strip_prefix(root)
            .map(Path::to_path_buf)
            .map_err(|_| outside())?,
        _ => full
            .strip_prefix(&repository.root)
            .map(Path::to_path_buf)
            .map_err(|_| outside())?,
    };
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

/// A contract inside the repository as a commit carries it: the names to ask
/// for the contract (and, for `sykli.rs`, the `sykli.json` beside it, as
/// `run` falls back to it) and its lock. JSON only: an emitter would have to
/// be run.
pub(crate) struct ContractNames {
    path: PathBuf,
    contract: String,
    fallback: Option<(PathBuf, String)>,
    lock: String,
}

impl ContractNames {
    pub(crate) fn new(rev: &str, contract: &Path) -> Result<Self, String> {
        let repository = repo::repository()?;
        let fallback = contract
            .file_name()
            .is_some_and(|name| name == "sykli.rs")
            .then(|| {
                let json = contract.with_file_name("sykli.json");
                path_in_repository(repository, &json).map(|spec| (json, format!("{rev}:{spec}")))
            })
            .transpose()?;
        Ok(ContractNames {
            path: contract.to_path_buf(),
            contract: format!("{rev}:{}", path_in_repository(repository, contract)?),
            fallback,
            lock: format!(
                "{rev}:{}",
                path_in_repository(repository, &lock_path(contract))?
            ),
        })
    }

    pub(crate) fn names(&self) -> Vec<String> {
        let mut names = vec![self.contract.clone(), self.lock.clone()];
        names.extend(self.fallback.iter().map(|(_, name)| name.clone()));
        names
    }

    /// Whether the commit carries the contract at the path it was named by.
    pub(crate) fn carried(&self, answers: &Answers) -> bool {
        answer(answers, &self.contract).is_some()
    }

    /// The contract the answers to `names()` give, judged as `verify --commit`
    /// judges it, with the errors it has always given.
    pub(crate) fn judged(&self, answers: &Answers, commit: &str) -> Result<Judged, String> {
        let short = &commit[..commit.len().min(12)];
        let blob = |name: &str| -> Result<Option<Vec<u8>>, String> {
            match answer(answers, name) {
                Some(object) if object.kind == "blob" => Ok(Some(object.bytes.clone())),
                Some(_) => Err(format!("{name} is not a file")),
                None => Ok(None),
            }
        };
        let mut path = self.path.clone();
        let mut bytes = blob(&self.contract)?;
        if bytes.is_none() {
            if let Some((json, name)) = &self.fallback {
                path = json.clone();
                bytes = blob(name)?;
            }
        }
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        {
            return Err(format!(
                "verify --commit reads a JSON contract from the commit; {} is not one",
                path.display()
            ));
        }
        let bytes = bytes.ok_or_else(|| format!("commit {short} has no {}", path.display()))?;
        let (contract, _, hash) = parse_contract(&bytes)
            .map_err(|error| format!("{} in commit {short}: {error}", path.display()))?;
        let lock = match blob(&self.lock)? {
            Some(bytes) => Some(
                pinned(
                    parse_lock(&bytes, &format!("sykli.lock in commit {short}"))?,
                    &path,
                )
                .map_err(|error| format!("commit {short}: {error}"))?,
            ),
            None => None,
        };
        Ok(Judged {
            contract,
            hash,
            lock,
            outside: None,
            contract_inputs: None,
        })
    }
}

/// Everything `verify --commit REV` needs from git, from one
/// `git cat-file --batch`: the commit, its tree, and for a contract inside
/// the repository the contract and lock the commit carries. A contract
/// outside the repository is read from its file.
fn commit_facts(
    repository: &repo::Repository,
    rev: &str,
    contract_path: &Path,
) -> Result<(Judged, CommitAt), String> {
    let outside = outside_repository(contract_path).is_some();
    // A path outside the repository named as inside cannot be asked about;
    // that is reported after the commit, as it always was.
    let inside = (!outside).then(|| ContractNames::new(rev, contract_path));
    let mut names = commit_names(rev).to_vec();
    if let Some(Ok(inside)) = &inside {
        names.extend(inside.names());
    }
    // One process, two rounds: the commit and its contract, then the inputs
    // that contract declares (and a third where a declared link leads).
    let mut session = repository.session(Vec::new());
    let mut answers = ask_in(&mut session, names)?;
    let mut at = commit_from(rev, &answers)?;
    let judged = match inside {
        None => Judged::from_file(contract_path, true)?,
        Some(inside) => inside?.judged(&answers, &at.id)?,
    };
    answers.extend(ask_in_bounded(
        &mut session,
        input_names(rev, &judged.contract)?,
        INPUT_BLOB_KEPT,
    )?);
    // A third round, only for declared symbolic links: where they lead.
    let around = link_target_names(rev, &judged.contract, &answers)?;
    if !around.is_empty() {
        answers.extend(ask_in(&mut session, around)?);
    }
    session.finish()?;
    at.inputs = Some(inputs_from(rev, &judged.contract, &answers)?);
    Ok((judged, at))
}

/// A receipt judged as evidence for a commit, against facts already read:
/// the schema, the contract and lock, the commit's tree, the records and the
/// outcome, each stage refusing as `verify --commit` always has. A fact git
/// could not give is an error, reported after the receipt's own.
pub(crate) fn judge(
    receipt_path: &Path,
    facts: Result<(&Judged, &CommitAt), &str>,
) -> Result<Vec<Check>, String> {
    let (receipt, mut checks) = read_receipt(receipt_path)?;
    if checks.iter().any(|check| !check.ok) {
        return Ok(checks);
    }
    let (judged, at) = facts.map_err(str::to_string)?;
    if !contract_checks(
        &mut checks,
        &receipt,
        &judged.hash,
        judged.lock.clone(),
        judged
            .outside
            .as_ref()
            .map(|(path, digest)| (path.as_path(), digest.clone())),
    ) {
        return Ok(checks);
    }
    commit_checks(&mut checks, &receipt, at);
    if checks.iter().any(|check| !check.ok) {
        return Ok(checks);
    }
    checks.push(inputs_check(&receipt, &judged.contract, at)?);
    if checks.iter().any(|check| !check.ok) {
        return Ok(checks);
    }
    let beside = judged
        .contract_inputs
        .as_ref()
        .map(|(directory, records)| (directory.as_path(), records));
    if let Some(check) = contract_inputs_check(&receipt, &judged.contract, beside) {
        let ok = check.ok;
        checks.push(check);
        if !ok {
            return Ok(checks);
        }
    }
    record_checks(&mut checks, &receipt, &judged.contract);
    Ok(checks)
}

/// The contract inputs stage, when the contract or the receipt has any:
/// each task's records name exactly the contract inputs it declares, and
/// each is what the file beside the contract is now (`beside`: the
/// contract's directory and what was read there). With or without
/// `--commit` alike, since no commit carries them, as the contract and its
/// lock are read from their files. A contract inside the repository
/// (`beside` None) cannot declare them, and a receipt claiming them for one
/// cannot be judged. A stale or missing record is exit 3, as an input's is.
fn contract_inputs_check(
    receipt: &ReceiptSummary,
    contract: &Contract,
    beside: Option<(&Path, &InputRecords)>,
) -> Option<Check> {
    let declares = contract
        .tasks
        .iter()
        .any(|task| !task.contract_inputs.is_empty());
    if !declares
        && receipt
            .tasks
            .iter()
            .all(|task| task.contract_inputs.is_none())
    {
        return None;
    }
    let refuse = |expected: String, actual: String, failure_code: u8| {
        Some(Check {
            name: "contract inputs",
            expected,
            actual,
            ok: false,
            failure_code,
        })
    };
    let show = |record: Option<&InputRecord>| match record {
        Some(record) => format!(
            "sha256 {}, mode {}",
            record.sha256,
            record
                .mode
                .map_or("none".into(), |bits| format!("{bits:#o}"))
        ),
        None => "nothing".into(),
    };
    let Some((directory, now)) = beside else {
        return refuse(
            "contract inputs only for a contract outside the repository, read from beside it"
                .into(),
            "contract inputs declared or recorded for a contract inside the repository".into(),
            NOT_VERIFIABLE,
        );
    };
    let none = BTreeMap::new();
    let mut judged = 0;
    for task in &contract.tasks {
        let Some(record) = receipt.tasks.iter().find(|record| record.name == task.name) else {
            continue;
        };
        let records = record.contract_inputs.as_ref().unwrap_or(&none);
        let declared = task.contract_inputs.iter().collect::<BTreeSet<_>>();
        if records.keys().collect::<BTreeSet<_>>() != declared {
            return refuse(
                format!(
                    "task {}'s contract inputs as the contract declares them, {declared:?}",
                    task.name
                ),
                format!("{:?} recorded", records.keys().collect::<Vec<_>>()),
                TREE_MISMATCH,
            );
        }
        for (input, ran) in records {
            let here = now
                .get(&task.name)
                .and_then(|records| records.get(input))
                .and_then(Option::as_ref);
            if here != ran.as_ref() {
                return refuse(
                    format!(
                        "task {}'s contract input {input} as it is beside the contract now: {}",
                        task.name,
                        show(here)
                    ),
                    format!("{} recorded by the run", show(ran.as_ref())),
                    TREE_MISMATCH,
                );
            }
            judged += 1;
        }
    }
    let actual = format!(
        "{judged} contract input{} judged beside the contract in {}",
        if judged == 1 { "" } else { "s" },
        directory.display()
    );
    Some(Check {
        name: "contract inputs",
        expected: actual.clone(),
        actual,
        ok: true,
        failure_code: TREE_MISMATCH,
    })
}

/// The contract and lock stages; whether they passed. `outside` is a contract
/// outside the repository and the SHA-256 of its lock file (None without one):
/// a person's contract and its lock are one version of their gate, so the
/// receipt must also have run with that lock file, byte for byte. A receipt
/// from before receipts recorded their lock cannot show that.
fn contract_checks(
    checks: &mut Vec<Check>,
    receipt: &ReceiptSummary,
    contract_hash: &str,
    lock: Option<String>,
    outside: Option<(&Path, Option<String>)>,
) -> bool {
    if let Some(lock) = lock {
        checks.push(Check::equals(
            "contract lock",
            CONTRACT_MISMATCH,
            contract_hash,
            lock,
        ));
    }
    let mut contract = Check::equals(
        "contract",
        CONTRACT_MISMATCH,
        contract_hash,
        &receipt.contract_hash,
    );
    if let Some((path, digest)) = outside {
        if !contract.ok {
            contract.expected =
                format!("{} (the contract at {})", contract.expected, path.display());
        }
        checks.push(contract);
        let named = |digest: Option<&str>| digest.unwrap_or("none").to_string();
        let mut lock = Check::equals(
            "lock",
            CONTRACT_MISMATCH,
            named(digest.as_deref()),
            named(receipt.lock_digest.as_deref()),
        );
        if !lock.ok {
            lock.expected = format!(
                "{} (sha256 of {})",
                lock.expected,
                lock_path(path).display()
            );
        }
        checks.push(lock);
    } else {
        checks.push(contract);
    }
    checks.iter().all(|check| check.ok)
}

/// Whether `verify --commit` also requires a clean run (`dirty: false`).
///
/// This is policy, not evidence. Once the receipt's tree is the commit's
/// tree, the run checked exactly the commit's content whatever HEAD was; a
/// clean run adds that the commit was checked out when it ran. Requiring it
/// costs a second, cached run after committing, because the pre-commit hook's
/// run happens before HEAD moves and is always dirty. So it is off: the
/// `clean` check is still reported but does not refuse, and the pre-commit
/// run counts for the push. Submodule changes are refused by their own check
/// either way. Turning it on flips the end-to-end hook tests that cover the
/// single run (`installed_hooks_gate_real_commits_and_pushes`,
/// `pre_push_refuses_a_failing_commit_on_main` and the dirty-run
/// `verify_commit_*` test).
const COMMIT_REQUIRES_CLEAN_RUN: bool = false;

/// The receipt's tree is the commit's, and the run was clean.
fn commit_checks(checks: &mut Vec<Check>, receipt: &ReceiptSummary, at: &CommitAt) {
    let mut check = Check::equals(
        "commit",
        TREE_MISMATCH,
        at.tree.clone(),
        &receipt.subject.tree_oid,
    );
    check.expected = format!("{} (the tree of commit {})", check.expected, &at.id[..12]);
    checks.push(check);
    // Refused whatever the clean rule is: the tree cannot show this content.
    if receipt.subject.submodules_modified {
        checks.push(Check {
            name: "submodules",
            expected: "submodules as the commit records them".into(),
            actual: "a submodule had uncommitted changes when it ran".into(),
            ok: false,
            failure_code: TREE_MISMATCH,
        });
    }
    // `dirty` alone is one editable field; HEAD's recorded tree must agree.
    let clean = receipt.subject.dirty == Some(false)
        && receipt.subject.head_tree_oid.as_deref() == Some(&receipt.subject.tree_oid);
    let dirty = receipt
        .subject
        .dirty
        .map_or("absent".into(), |dirty| dirty.to_string());
    checks.push(Check {
        name: "clean",
        expected: "a clean run (dirty false: the tree that ran was HEAD's); reported, not required"
            .into(),
        actual: match receipt.subject.head_tree_oid.as_deref() {
            _ if clean => "dirty false".into(),
            Some(head) if head != receipt.subject.tree_oid => format!(
                "dirty {dirty}: tree {} ran with HEAD at tree {head}",
                receipt.subject.tree_oid
            ),
            Some(_) => format!("dirty {dirty}"),
            None => format!("dirty {dirty} and no HEAD tree recorded"),
        },
        ok: clean || !COMMIT_REQUIRES_CLEAN_RUN,
        failure_code: TREE_MISMATCH,
    });
}

/// The records and outcome stages, against the contract that was judged.
fn record_checks(checks: &mut Vec<Check>, receipt: &ReceiptSummary, contract: &Contract) {
    let contradiction = if receipt.tasks.len() != contract.tasks.len() {
        Some(format!(
            "{} task records for {} declared tasks",
            receipt.tasks.len(),
            contract.tasks.len()
        ))
    } else {
        receipt
            .tasks
            .iter()
            .zip(&contract.tasks)
            .find_map(|(record, task)| record_contradiction(record, task))
            .or_else(|| {
                let all_good = receipt
                    .tasks
                    .iter()
                    .all(|r| matches!(r.outcome.as_str(), "passed" | "cached"));
                let claims_good = matches!(receipt.outcome.as_str(), "passed" | "cached");
                (all_good != claims_good).then(|| {
                    format!(
                        "receipt outcome {} disagrees with its task records",
                        receipt.outcome
                    )
                })
            })
    };
    checks.push(Check {
        name: "records",
        expected: "task records consistent with each other and the contract".into(),
        actual: contradiction.clone().unwrap_or_else(|| "consistent".into()),
        ok: contradiction.is_none(),
        failure_code: NOT_VERIFIABLE,
    });
    if contradiction.is_some() {
        return;
    }
    let tasks_ok = receipt
        .tasks
        .iter()
        .all(|record| record.importable && matches!(record.outcome.as_str(), "passed" | "cached"));
    checks.push(Check {
        name: "outcome",
        expected: "passed or cached with complete task records".into(),
        actual: format!(
            "{} with {} task records",
            receipt.outcome,
            receipt.tasks.len()
        ),
        ok: matches!(receipt.outcome.as_str(), "passed" | "cached") && tasks_ok,
        failure_code: OUTCOME_FAILED,
    });
}

/// Verify exit codes are stages, like a CI pipeline: checks run in order and
/// the first failing stage decides, so gates can branch on the code.
/// 1 — the work is bad; 2 — cannot verify (not a receipt, unreadable input,
/// git or contract error; matches the house exit-2 misuse convention);
/// 3 — receipt is stale for this tree, re-run sykli; 4 — the contract
/// drifted from the receipt, re-lock or investigate.
const OUTCOME_FAILED: u8 = 1;
const NOT_VERIFIABLE: u8 = 2;
const TREE_MISMATCH: u8 = 3;
const CONTRACT_MISMATCH: u8 = 4;

fn report(checks: &[Check]) -> ExitCode {
    for check in checks {
        if check.ok {
            println!("ok: {} {}", check.name, check.actual);
        } else {
            println!(
                "mismatch: {} expected {} but got {}",
                check.name, check.expected, check.actual
            );
        }
    }
    match checks.iter().find(|check| !check.ok) {
        None => {
            println!("verified: receipt matches this tree and contract");
            println!("{TRUST}");
            ExitCode::SUCCESS
        }
        Some(first) => {
            eprintln!("refused: {}", refusal(first));
            ExitCode::from(first.failure_code)
        }
    }
}

/// The first failing check as one line, for stderr and for the hooks.
fn refusal(check: &Check) -> String {
    format!(
        "{} expected {} but got {}",
        check.name, check.expected, check.actual
    )
}

/// Write a receipt as `.sykli/receipts/rcpt_<sha256 of its bytes>.json`,
/// durably: the bytes are synced in a temporary, then renamed into place,
/// then the directory is synced. A receipt already there has these bytes, by
/// its name. Returns its path, digest and bytes.
fn write_receipt(receipt: &Receipt) -> Result<(PathBuf, String, Vec<u8>), String> {
    let bytes = serde_json::to_vec_pretty(receipt).map_err(|error| error.to_string())?;
    let hash = sha256(&bytes);
    let directory = Path::new(&receipt.subject.repository).join(".sykli/receipts");
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let path = directory.join(format!("rcpt_{hash}.json"));
    if !path.exists() {
        let temporary = directory.join(format!(".{hash}.tmp-{}", std::process::id()));
        let written = fs::File::create(&temporary)
            .and_then(|mut file| {
                file.write_all(&bytes)
                    .and_then(|()| canonical::sync(&file, canonical::Durability::Fsync))
            })
            .map_err(|error| error.to_string());
        if let Err(error) = written.and_then(|()| checkpoint("during-receipt")) {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        if let Err(error) = fs::rename(&temporary, &path) {
            if path.exists() {
                let _ = fs::remove_file(temporary);
            } else {
                return Err(error.to_string());
            }
        }
        canonical::sync_directory(&directory, canonical::Durability::Fsync)?;
    }
    Ok((spelled(&path), hash, bytes))
}

/// `path` as the file system spells it (symbolic links resolved, as on
/// macOS, where /var is /private/var), so a path sykli reports has one
/// spelling however a caller spelled the repository (`--repo-root`). A path
/// that is gone keeps its directory resolved; one that cannot be resolved
/// at all is as given.
fn spelled(path: &Path) -> PathBuf {
    fs::canonicalize(path)
        .or_else(|_| {
            let name = path.file_name().ok_or(io::ErrorKind::NotFound)?;
            fs::canonicalize(path.parent().ok_or(io::ErrorKind::NotFound)?)
                .map(|parent| parent.join(name))
        })
        .unwrap_or_else(|_| path.to_path_buf())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if count == 0 {
            return Ok(format!("{:x}", hasher.finalize()));
        }
        hasher.update(&buffer[..count]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A task launched with no journal, as its receipt: a launch error or a stop
    /// becomes an errored one.
    fn run_task(
        task: &Task,
        runtime: &ShellRuntime,
        progress: Progress,
        capture_limit: usize,
    ) -> TaskReceipt {
        launch(task, runtime, progress, capture_limit, None, None)
            .and_then(|launched| match launched {
                Launched::Ran { record, .. } => Ok(*record),
                Launched::Stopped { .. } => Err("stopped on a signal".to_string()),
            })
            .unwrap_or_else(|error| {
                empty_task_receipt(
                    task,
                    runtime,
                    Outcome::Errored,
                    &error,
                    "runtime_error",
                    false,
                )
            })
    }

    /// Every task decided without a journal, as a run on Windows does.
    fn execute_all(
        contract: &Contract,
        levels: &[Vec<usize>],
        runtime: &ShellRuntime,
        cache: &dyn Cache,
    ) -> Vec<TaskReceipt> {
        let subject = Subject {
            repository: String::new(),
            tree_oid: String::new(),
            inputs_digest: String::new(),
            head_tree_oid: String::new(),
            dirty: false,
            submodules_modified: false,
            inputs: BTreeMap::new(),
            head_commit: None,
            contract_directory: PathBuf::new(),
            contract_inputs: BTreeMap::new(),
        };
        let undecided = std::iter::repeat_with(|| None)
            .take(contract.tasks.len())
            .collect();
        execute(
            contract,
            levels,
            runtime,
            cache,
            Progress::Json,
            None,
            &subject,
            undecided,
        )
        .unwrap()
        .into_iter()
        .map(Option::unwrap)
        .collect()
    }

    #[test]
    fn a_directory_is_named_by_its_path_not_by_a_longer_name() {
        let directory = "/home/a/gate";
        for text in [
            "sh /home/a/gate/check.sh",
            "sh '/home/a/gate/check.sh'",
            "cd /home/a/gate && sh check.sh",
            "PATH=/home/a/gate:$PATH check",
            "/home/a/gate",
        ] {
            assert!(names_directory(text, directory, None), "{text}");
        }
        for text in [
            "sh /home/a/gate-old/check.sh",
            "sh /home/a/gate.d/check.sh",
            "sh /x/home/a/gate/check.sh",
            "sh home/a/gate/check.sh",
            "sh \"$SYKLI_CONTRACT_DIR/check.sh\"",
        ] {
            assert!(!names_directory(text, directory, None), "{text}");
        }
        // A directory that holds the repository: paths in the repository
        // are the repository's, unless they climb back out.
        let root = Some("/home/a/gate/repo");
        for text in [
            "CARGO_TARGET_DIR=/home/a/gate/repo/target cargo test",
            "cd '/home/a/gate/repo' && true",
            "PATH=/home/a/gate/repo/bin:$PATH x",
        ] {
            assert!(!names_directory(text, directory, root), "{text}");
        }
        // A spelling with a drive, as on Windows, is a path all the same.
        let (drive, below) = ("C:/a/gate", Some("C:/a/gate/repo"));
        assert!(!names_directory("test -f 'C:/a/gate/repo/x'", drive, below));
        assert!(names_directory("test -f 'C:/a/gate/x'", drive, below));
        for text in [
            "sh /home/a/gate/check.sh",
            "sh /home/a/gate/repo/../check.sh",
            "sh /home/a/gate/repo-old/x.sh",
            "cd /home/a/gate/repo && sh /home/a/gate/check.sh",
        ] {
            assert!(names_directory(text, directory, root), "{text}");
        }
    }

    #[test]
    fn paths_are_spelled_out_as_the_shell_reaches_them() {
        let home = Some("/home/a");
        for (text, spelled) in [
            ("sh $HOME/gate/x", "sh /home/a/gate/x"),
            ("sh \"${HOME}/gate/x\"", "sh /home/a/gate/x"),
            ("sh ~/gate/x", "sh /home/a/gate/x"),
            ("X=~/gate:~ y", "X=/home/a/gate:/home/a y"),
            ("sh /home//a/./gate/.//x", "sh /home/a/gate/x"),
            ("echo $HOMEDIR ~user a~/b", "echo $HOMEDIR ~user a~/b"),
            ("echo $$ $ ~", "echo $$ $ /home/a"),
            // Quotes are removed as the shell removes them.
            ("sh \"$HOME\"/gate/x", "sh /home/a/gate/x"),
            ("sh $HOME\"/gate/x\"", "sh /home/a/gate/x"),
            ("sh '/home/a'/gate/x", "sh /home/a/gate/x"),
            ("printf '%s' \"don't\"", "printf %s dont"),
        ] {
            assert_eq!(spelled_out(text, home), spelled, "{text}");
        }
        assert_eq!(spelled_out("sh $HOME/x ~/y", None), "sh $HOME/x ~/y");
        let escaped = spelled_out("sh /home/a/ga\\te/x", home);
        if cfg!(windows) {
            assert_eq!(escaped, "sh /home/a/ga/te/x");
        } else {
            assert_eq!(escaped, "sh /home/a/gate/x");
        }
        // A variable just before the path may be empty: it hides nothing.
        assert!(names_directory("sh $P/home/a/gate/x", "/home/a/gate", None));
        assert!(names_directory(
            "sh ${P}/home/a/gate/x",
            "/home/a/gate",
            None
        ));
        assert!(!names_directory("sh x/home/a/gate/x", "/home/a/gate", None));
    }

    /// Only this user's copies that stood unchanged for over a day go; a
    /// younger one, another name and a link to an old one stay.
    #[cfg(unix)]
    #[test]
    fn stale_copies_of_this_user_are_pruned_and_nothing_else() {
        let base = std::env::temp_dir().join(format!("sykli-prune-{}", private_name()));
        fs::create_dir(&base).unwrap();
        let ours = fresh_directory(&base, ["sykli-contract-ours".to_string()]).unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86_400);
        let aged = |name: &str| {
            let path = base.join(name);
            fs::create_dir(&path).unwrap();
            fs::write(path.join("f"), "x").unwrap();
            fs::File::open(&path).unwrap().set_modified(old).unwrap();
            path
        };
        let stale = aged("sykli-contract-stale");
        let other = aged("other-old");
        let young = base.join("sykli-contract-young");
        fs::create_dir(&young).unwrap();
        let elsewhere = aged("elsewhere");
        std::os::unix::fs::symlink(&elsewhere, base.join("sykli-contract-link")).unwrap();
        fs::File::open(&ours).unwrap().set_modified(old).unwrap();
        prune_stale_copies(&base, &ours);
        assert!(!stale.exists());
        for kept in [&ours, &other, &young, &elsewhere.join("f")] {
            assert!(kept.exists(), "{}", kept.display());
        }
        assert!(fs::symlink_metadata(base.join("sykli-contract-link")).is_ok());
        fs::remove_dir_all(&base).unwrap();
    }

    /// A name another process holds (as a killed run's copy is) is passed
    /// over for the next one, never entered; the directory is its owner's.
    #[test]
    fn a_fresh_directory_passes_over_a_name_that_is_taken() {
        let base = std::env::temp_dir().join(format!("sykli-fresh-{}", private_name()));
        fs::create_dir(&base).unwrap();
        fs::create_dir(base.join("taken")).unwrap();
        fs::write(base.join("taken/left.txt"), "left").unwrap();
        let made = fresh_directory(&base, ["taken".to_string(), "free".to_string()]).unwrap();
        assert_eq!(made, base.join("free"));
        assert_eq!(fs::read(base.join("taken/left.txt")).unwrap(), b"left");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&made).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        let none = fresh_directory(&base, ["taken".to_string(), "free".to_string()]);
        assert_eq!(none.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_ne!(private_name(), private_name());
        assert_eq!(private_name().len(), 32);
        fs::remove_dir_all(&base).unwrap();
    }

    /// RFC 4231, test cases 1, 2 and 6 (a key longer than a block).
    #[test]
    fn hmac_sha256_matches_rfc_4231() {
        assert_eq!(
            hmac_sha256(&[0x0b; 20], b"Hi There"),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hmac_sha256(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            ),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    /// The key is made once, whole, and read back the same by every process
    /// that asks; one that is not what sykli writes is refused, never
    /// replaced, so a run never digests under a key it did not make.
    #[test]
    fn the_inherit_key_is_made_once_and_never_replaced() {
        let base = std::env::temp_dir().join(format!("sykli-key-{}", private_name()));
        let path = base.join("sykli/inherit-key");
        let keys = std::thread::scope(|scope| {
            (0..4)
                .map(|_| scope.spawn(|| read_or_make_key(&path)))
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| handle.join().unwrap().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(keys.iter().all(|key| *key == keys[0]));
        let hex = keys[0]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(fs::read_to_string(&path).unwrap(), hex);
        assert_eq!(read_key(&path).unwrap(), Some(keys[0]));
        assert_eq!(
            fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1,
            "no temporary is left"
        );
        for bad in [
            "short".to_string(),
            format!("+{}", &hex[1..]),
            format!("{hex}\n"),
        ] {
            fs::write(&path, &bad).unwrap();
            let refused = read_or_make_key(&path).unwrap_err();
            assert!(refused.contains("not a key sykli wrote"), "{refused}");
            assert_eq!(fs::read_to_string(&path).unwrap(), bad);
        }
        fs::remove_dir_all(base).unwrap();
    }

    /// The key is 32 bytes from the operating system, never derived from
    /// anything a process holds: two draws differ.
    #[test]
    fn the_inherit_key_comes_from_the_operating_system() {
        let (one, two) = (random_key().unwrap(), random_key().unwrap());
        assert_eq!(one.len(), 32);
        assert_ne!(one, two);
        assert_ne!(one, [0; 32]);
    }

    /// Without hard links the key is renamed into place, never over one
    /// that is there already.
    #[test]
    fn without_hard_links_a_key_is_renamed_into_place_once() {
        let base = std::env::temp_dir().join(format!("sykli-rename-{}", private_name()));
        fs::create_dir_all(&base).unwrap();
        let (path, first, second) = (base.join("key"), base.join("a"), base.join("b"));
        fs::write(&first, "first").unwrap();
        fs::write(&second, "second").unwrap();
        place_by_rename(&first, &path).unwrap();
        place_by_rename(&second, &path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "first");
        assert!(!first.exists() && second.exists());
        fs::remove_dir_all(base).unwrap();
    }

    /// A task that declares no contract inputs keeps the key it had before
    /// they existed: the same bytes, hashed the same way.
    #[test]
    fn a_task_without_contract_inputs_keeps_its_key() {
        #[derive(Serialize)]
        struct Before<'a> {
            task: &'a Task,
            inputs: BTreeMap<&'a str, String>,
            runtime: &'a str,
            after: &'a BTreeMap<&'a str, &'a str>,
            inherited: BTreeMap<&'a str, String>,
        }
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        let task: Task = serde_json::from_value(serde_json::json!({
            "name": "t", "run": "true", "inputs": [manifest], "env": {"A": "b"},
        }))
        .unwrap();
        let runtime = ShellRuntime {
            path: PathBuf::from("sh"),
            fingerprint: "shell".into(),
            environment: Vec::new(),
        };
        let after = BTreeMap::from([("u", "k")]);
        let before = Before {
            task: &task,
            inputs: BTreeMap::from([(manifest, input_digest(Path::new(manifest)).unwrap())]),
            runtime: "shell",
            after: &after,
            inherited: BTreeMap::new(),
        };
        assert_eq!(
            cache_key(&task, &runtime, &after, &InputRecords::new()).unwrap(),
            sha256(&serde_json::to_vec(&before).unwrap())
        );
    }

    #[test]
    fn the_cache_stays_inside_its_budget_and_drops_the_oldest_first() {
        let root = std::env::temp_dir().join(format!(
            "sykli-budget-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cache = root.join(".sykli/cache");
        fs::create_dir_all(&cache).unwrap();

        // Three entries of 1 KiB, written oldest to newest.
        for name in ["old", "middle", "new"] {
            let entry = cache.join(name);
            fs::create_dir_all(&entry).unwrap();
            fs::write(entry.join("blob"), vec![0u8; 1024]).unwrap();
            // Distinguishable mtimes without sleeping the test. `evict` reads
            // the newest file beneath an entry, so stamping the blob is what
            // decides the order.
            let seconds = match name {
                "old" => 1_000_000,
                "middle" => 2_000_000,
                _ => 3_000_000,
            };
            let stamp = std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds);
            fs::File::options()
                .write(true)
                .open(entry.join("blob"))
                .unwrap()
                .set_modified(stamp)
                .unwrap();
        }

        let mut local = LocalCache::new(&root);
        // Room for two of the three.
        local.budget = 2048;
        local.evict(&cache.join("new"));

        assert!(
            !cache.join("old").exists(),
            "the oldest entry must go first"
        );
        assert!(
            cache.join("middle").exists(),
            "only enough to fit the budget"
        );
        assert!(cache.join("new").exists(), "the entry just written is kept");

        // A zero budget disables eviction entirely.
        local.budget = 0;
        local.evict(&cache.join("new"));
        assert!(cache.join("middle").exists());

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn receipts_are_pruned_oldest_first_but_never_one_a_cache_entry_cites() {
        let root = std::env::temp_dir().join(format!(
            "sykli-receipts-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let receipts = root.join(".sykli/receipts");
        let cache = root.join(".sykli/cache");
        fs::create_dir_all(&receipts).unwrap();
        fs::create_dir_all(&cache).unwrap();

        let stamp = |path: &Path, seconds: u64| {
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
                .unwrap();
        };

        // Four receipts, oldest to newest. The oldest is the one a live cache
        // entry cites, so age alone must not condemn it.
        for (name, seconds) in [
            ("rcpt_aaa.json", 1_000_000),
            ("rcpt_bbb.json", 2_000_000),
            ("rcpt_ccc.json", 3_000_000),
            ("rcpt_ddd.json", 4_000_000),
        ] {
            let path = receipts.join(name);
            fs::write(&path, b"{}").unwrap();
            stamp(&path, seconds);
        }
        let entry = cache.join("some-key");
        fs::create_dir_all(&entry).unwrap();
        fs::write(
            entry.join("entry.json"),
            serde_json::to_vec(&CacheEntry {
                receipt: "rcpt_aaa.json".into(),
                outputs: vec![],
                witnessed_at_ms: None,
            })
            .unwrap(),
        )
        .unwrap();

        // Budget of three across four receipts: exactly one must go, and it
        // cannot be the cited one, so it is the oldest of the rest. The budget
        // is set on the value rather than through the environment, which is
        // process-global and unsound beside a threaded test binary.
        let mut cache_under_test = LocalCache::new(&root);
        cache_under_test.receipt_budget = 3;
        cache_under_test.prune_receipts();

        assert!(
            receipts.join("rcpt_aaa.json").exists(),
            "a receipt a cache entry cites must survive, however old"
        );
        assert!(
            !receipts.join("rcpt_bbb.json").exists(),
            "the oldest removable receipt goes first"
        );
        assert!(receipts.join("rcpt_ccc.json").exists());
        assert!(receipts.join("rcpt_ddd.json").exists());

        fs::remove_dir_all(&root).unwrap();
    }

    /// A receipt written moments ago is never pruned, even when the budget
    /// cannot be met without it.
    ///
    /// A run writes its receipt and only then stores the cache entries that
    /// cite it. In between, the receipt is on disk and unreferenced. A
    /// concurrent prune that deleted it would leave the first run storing an
    /// entry that names a receipt which no longer exists — an entry that can
    /// never hit again, and a cached result with nothing behind it.
    ///
    /// Reachable whenever referenced receipts alone exceed the budget, which
    /// is ordinary for a cache larger than the receipt budget.
    #[test]
    fn a_receipt_still_in_flight_survives_a_prune_that_cannot_meet_its_budget() {
        let root = std::env::temp_dir().join(format!(
            "sykli-inflight-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let receipts = root.join(".sykli/receipts");
        let cache = root.join(".sykli/cache");
        fs::create_dir_all(&receipts).unwrap();
        fs::create_dir_all(&cache).unwrap();

        // Three cited receipts against a budget of two: the budget is already
        // unmeetable before the new receipt is considered.
        for (index, name) in ["rcpt_1.json", "rcpt_2.json", "rcpt_3.json"]
            .into_iter()
            .enumerate()
        {
            fs::write(receipts.join(name), b"{}").unwrap();
            let entry = cache.join(format!("key{index}"));
            fs::create_dir_all(&entry).unwrap();
            fs::write(
                entry.join("entry.json"),
                serde_json::to_vec(&CacheEntry {
                    receipt: name.into(),
                    outputs: vec![],
                    witnessed_at_ms: None,
                })
                .unwrap(),
            )
            .unwrap();
        }
        // Just written, not yet cited — exactly the window.
        let in_flight = receipts.join("rcpt_new.json");
        fs::write(&in_flight, b"{}").unwrap();

        let mut cache_under_test = LocalCache::new(&root);
        cache_under_test.receipt_budget = 2;
        cache_under_test.prune_receipts();

        assert!(
            in_flight.exists(),
            "a receipt written moments ago must survive; deleting it poisons \
             the cache entry about to cite it"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn contract_paths_cannot_escape_the_repository() {
        fn parse(json: &str) -> Contract {
            serde_json::from_str::<Contract>(json).unwrap()
        }
        let refuses = |json: &str, field: &str| {
            let error = validate(&parse(json)).unwrap_err();
            assert!(
                error.contains("must stay inside the repository") && error.contains(field),
                "expected a refusal naming the {field}, got {error:?}"
            );
        };

        // The escape the issue reported: an output written outside the tree.
        refuses(
            r#"{"schema":"sykli-contract.v1","tasks":[
                {"name":"evil","run":"true","outputs":["../../somewhere"]}
            ]}"#,
            "output",
        );
        // An input read from outside it, which would also pull the file into
        // the cache key and so into the receipt.
        refuses(
            r#"{"schema":"sykli-contract.v1","tasks":[
                {"name":"evil","run":"true","inputs":["../../etc/passwd"]}
            ]}"#,
            "input",
        );
        // A workdir that relocates every relative path the task declares.
        refuses(
            r#"{"schema":"sykli-contract.v1","tasks":[
                {"name":"evil","run":"true","workdir":"../.."}
            ]}"#,
            "workdir",
        );
        // Absolute, and a climb buried mid-path rather than at the front:
        // components() preserves `..` instead of resolving it away.
        refuses(
            r#"{"schema":"sykli-contract.v1","tasks":[
                {"name":"evil","run":"true","outputs":["/etc/cron.d/payload"]}
            ]}"#,
            "output",
        );
        refuses(
            r#"{"schema":"sykli-contract.v1","tasks":[
                {"name":"evil","run":"true","inputs":["src/../../../etc/passwd"]}
            ]}"#,
            "input",
        );

        let empty = validate(&parse(
            r#"{"schema":"sykli-contract.v1","tasks":[
                {"name":"evil","run":"true","outputs":[""]}
            ]}"#,
        ))
        .unwrap_err();
        assert!(empty.contains("empty"), "got {empty:?}");

        // Ordinary contracts still validate: nested paths, an explicit `.`,
        // and a leading `./` are all inside the tree.
        assert!(
            validate(&parse(
                r#"{"schema":"sykli-contract.v1","tasks":[
                {"name":"build","run":"true","workdir":"crates/core",
                 "inputs":["./src/lib.rs","src/nested/deep.rs"],
                 "outputs":["target/debug/thing"]}
            ]}"#,
            ))
            .is_ok()
        );
    }

    #[test]
    fn contract_validation() {
        let environment = |pairs: &[(&str, &str)]| {
            environment_digest(
                pairs
                    .iter()
                    .map(|(name, value)| ((*name).into(), (*value).into()))
                    .collect(),
            )
        };
        assert_eq!(
            environment(&[("B", "2"), ("A", "1")]),
            environment(&[("A", "1"), ("B", "2")])
        );
        assert_ne!(environment(&[("A", "1")]), environment(&[("A", "2")]));
        let inherited = inherited_environment(
            [("PATH", "bin"), ("HOME", "home"), ("SECRET", "hidden")]
                .into_iter()
                .map(|(name, value)| (name.into(), value.into())),
        );
        assert_eq!(
            inherited,
            [
                ("PATH".into(), "bin".into()),
                ("HOME".into(), "home".into())
            ]
        );

        // Inside a hook, git's prepended exec path comes off PATH again.
        let joined = |entries: &[&str]| std::env::join_paths(entries).unwrap();
        let hooked = vec![
            ("PATH".into(), joined(&["git-core", "bin", "git-core"])),
            ("HOME".into(), "home".into()),
        ];
        assert_eq!(
            without_git_prepend(hooked.clone(), Some("git-core".into())),
            [
                ("PATH".into(), joined(&["bin", "git-core"])),
                ("HOME".into(), "home".into())
            ]
        );
        assert_eq!(without_git_prepend(hooked.clone(), None), hooked);
        let elsewhere = vec![("PATH".into(), joined(&["bin", "git-core"]))];
        assert_eq!(
            without_git_prepend(elsewhere.clone(), Some("git-core".into())),
            elsewhere
        );

        let parse = |json| serde_json::from_str::<Contract>(json);

        let valid = parse(
            r#"{"schema":"sykli-contract.v1","tasks":[
                {"name":"build","run":"cargo build"},
                {"name":"test","run":"cargo test","after":["build"]}
            ]}"#,
        )
        .unwrap();
        assert_eq!(validate(&valid).unwrap(), vec![vec![0], vec![1]]);

        assert!(parse(r#"{"schema":"sykli-contract.v1","tasks":[],"extra":true}"#).is_err());

        let cycle = parse(
            r#"{"schema":"sykli-contract.v1","tasks":[
                {"name":"a","run":"true","after":["b"]},
                {"name":"b","run":"true","after":["a"]}
            ]}"#,
        )
        .unwrap();
        assert_eq!(validate(&cycle).unwrap_err(), "dependency cycle detected");

        let delta = parse(
            r#"{"schema":"sykli-contract.v1","tasks":[
                {"name":"build","run":"true","inputs":["src/lib.rs"]},
                {"name":"test","run":"true","after":["build"]},
                {"name":"docs","run":"true","inputs":["README.md"]}
            ]}"#,
        )
        .unwrap();
        assert_eq!(
            affected(&delta, &validate(&delta).unwrap(), &["src/lib.rs".into()]).unwrap(),
            ["build", "test"]
        );

        let failure = parse(
            r#"{"schema":"sykli-contract.v1","tasks":[
                {"name":"fails","run":"exit 1"},
                {"name":"blocked","run":"exit 0","after":["fails"]},
                {"name":"missing","run":"exit 0","outputs":["definitely-missing"]}
            ]}"#,
        )
        .unwrap();
        let cache = LocalCache::new(
            &std::env::temp_dir().join(format!("sykli-test-{}", std::process::id())),
        );
        let receipts = execute_all(
            &failure,
            &validate(&failure).unwrap(),
            &shell_runtime().unwrap(),
            &cache,
        );
        assert!(receipts[0].outcome == Outcome::Failed);
        assert!(receipts[1].outcome == Outcome::Blocked);
        assert!(receipts[2].outcome == Outcome::Failed);
        assert!(receipts[2].class.as_deref() == Some("missing_output"));

        let captured = relay(&b"captured"[..], io::sink(), 4);
        assert_eq!(captured.bytes, b"capt");
        assert_eq!(captured.digest, sha256(b"captured"));
        assert_eq!(captured.bytes_dropped, 4);
        assert!(captured.error.is_none());

        let truncates = parse(
            r#"{"schema":"sykli-contract.v1","tasks":[{"name":"chatty","run":"printf captured"}]}"#,
        )
        .unwrap();
        let truncated = run_task(
            &truncates.tasks[0],
            &shell_runtime().unwrap(),
            Progress::Json,
            4,
        );
        assert!(truncated.stdout_truncated);
        assert_eq!(truncated.stdout_bytes_dropped, 4);
        // The digest covers the whole stream, so a truncated capture is
        // still complete evidence: importable and cacheable.
        assert_eq!(truncated.stdout_digest, sha256(b"captured"));
        assert!(truncated.importable);
        assert!(cacheable(&truncated));

        let root = std::env::temp_dir().join(format!(
            "sykli-cache-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let contract_path = root.join("contract.json");
        fs::write(
            &contract_path,
            r#"{"schema":"sykli-contract.v1","tasks":[{"name":"test","run":"true"}]}"#,
        )
        .unwrap();
        write_lock(&contract_path).unwrap();
        assert!(load(&contract_path).is_ok());
        fs::write(
            &contract_path,
            r#"{"schema":"sykli-contract.v1","tasks":[{"name":"test","run":"false"}]}"#,
        )
        .unwrap();
        assert!(
            load(&contract_path)
                .err()
                .unwrap()
                .contains("contract differs")
        );

        let cached_contract = Contract {
            schema: "sykli-contract.v1".into(),
            tasks: vec![Task {
                name: "cached".into(),
                run: "printf cache > result".into(),
                workdir: Some(root.clone()),
                env: BTreeMap::new(),
                after: Vec::new(),
                inputs: Vec::new(),
                outputs: vec!["result".into()],
                runtime: None,
                inherit: vec![],
                contract_inputs: vec![],
            }],
        };
        let runtime = shell_runtime().unwrap();
        let cache = LocalCache::new(&root);
        let first = execute_all(&cached_contract, &[vec![0]], &runtime, &cache);
        let receipt = Receipt {
            schema: "sykli-receipt.v1",
            contract_hash: "test".into(),
            lock_digest: None,
            started_at_ms: unix_time_ms(),
            subject: Subject {
                repository: root.to_string_lossy().into(),
                tree_oid: "test".into(),
                inputs_digest: "test".into(),
                head_tree_oid: "test".into(),
                dirty: false,
                submodules_modified: false,
                inputs: BTreeMap::new(),
                head_commit: None,
                contract_directory: PathBuf::new(),
                contract_inputs: BTreeMap::new(),
            },
            tasks: first,
            outcome: Outcome::Passed,
            run: None,
        };
        let (receipt_path, _, _) = write_receipt(&receipt).unwrap();
        cache
            .store(
                &cached_contract.tasks[0],
                &receipt.tasks[0],
                receipt_path.file_name().unwrap().to_str().unwrap(),
            )
            .unwrap();
        fs::write(root.join("result"), "stale").unwrap();
        let second = execute_all(&cached_contract, &[vec![0]], &runtime, &cache);
        assert!(second[0].outcome == Outcome::Cached);
        assert_eq!(fs::read(root.join("result")).unwrap(), b"cache");

        // A reused task replays when it was witnessed, not now. Stamping the
        // restore with the current time would make a cache hit claim the work
        // was observed at a moment nothing was observed — the one thing the
        // timestamp exists to rule out.
        assert_eq!(
            second[0].started_at_ms, receipt.tasks[0].started_at_ms,
            "a cache hit must replay the original witness time"
        );
        assert!(second[0].started_at_ms.is_some());
        assert_eq!(second[0].source, "cache");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn working_tree_oid_addresses_content_not_head() {
        let root = std::env::temp_dir().join(format!(
            "sykli-tree-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let git = |args: &[&str]| {
            let output = ProcessCommand::new("git")
                .arg("-C")
                .arg(&root)
                .args(["-c", "user.email=t@t", "-c", "user.name=t"])
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?} failed");
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        git(&["init", "--quiet"]);
        fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(root.join("tracked.txt"), "one").unwrap();
        git(&["add", "--all"]);
        git(&["commit", "--quiet", "-m", "init"]);
        let head = git(&["rev-parse", "HEAD^{tree}"]);
        let repository = repo::Repository::at(root.clone(), root.join(".git"));
        let working_tree_oid = |_: &Path| snapshot(&repository, &[]).map(|snapshot| snapshot.tree);

        // A clean checkout addresses to exactly HEAD's tree.
        assert_eq!(working_tree_oid(&root).unwrap(), head);

        // Untracked and modified content change the OID; ignored files and
        // sykli's own working data under .sykli never do.
        fs::write(root.join("ignored.txt"), "invisible").unwrap();
        fs::create_dir_all(root.join(".sykli/receipts")).unwrap();
        fs::write(root.join(".sykli/receipts/rcpt_x.json"), "{}").unwrap();
        assert_eq!(working_tree_oid(&root).unwrap(), head);

        fs::write(root.join("tracked.txt"), "two").unwrap();
        let objects_before = git(&["count-objects", "-v"]);
        let dirty = working_tree_oid(&root).unwrap();
        assert_ne!(dirty, head);
        assert_eq!(git(&["count-objects", "-v"]), objects_before);

        // Same content, same address — regardless of when it is computed.
        assert_eq!(working_tree_oid(&root).unwrap(), dirty);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn contract_hash_is_stable_under_key_order() {
        let root = std::env::temp_dir().join(format!(
            "sykli-hash-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let ordered = root.join("ordered.json");
        let reordered = root.join("reordered.json");
        fs::write(
            &ordered,
            r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"true"}]}"#,
        )
        .unwrap();
        fs::write(
            &reordered,
            r#"{"tasks":[{"run":"true","name":"t"}],"schema":"sykli-contract.v1"}"#,
        )
        .unwrap();
        assert_eq!(
            load_unlocked(&ordered).unwrap().2,
            load_unlocked(&reordered).unwrap().2
        );
        fs::remove_dir_all(root).unwrap();
    }
}
