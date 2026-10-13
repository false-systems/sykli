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

mod assessment;
mod canonical;
mod evidence;
mod github;
mod hook;
mod init;
mod inspect;
#[cfg(unix)]
mod journal;
#[cfg(unix)]
mod production;
mod repo;

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
        1  invalid, unreadable, or drifted from sykli.lock")]
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
        2  could not evaluate: unreadable contract, lock drift, undeclared Cargo inputs, no git or sh\n\n\
        The receipt is written to .sykli/receipts/<hash>.json; --json also prints it.\n\n\
        A contract outside the repository judges the git repository around the current\n\
        directory: its inputs are paths from that repository's root, its lock is the\n\
        sykli.lock beside the contract, and the receipt still goes to the repository's .sykli/.")]
    Run {
        /// Path to a sykli-contract.v1 JSON file or a sykli.rs emitter (sykli.json when only it exists); may be outside the repository
        #[arg(default_value = "sykli.rs")]
        contract: PathBuf,
        /// Print the run receipt as JSON on stdout; progress and task output go to stderr
        #[arg(long)]
        json: bool,
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
        #[arg(long, conflicts_with = "target")]
        explain: bool,
        /// Coverage comparison base (default HEAD); does not filter task selection
        #[arg(long, requires = "explain")]
        base: Option<String>,
        /// Preview an edited JSON contract despite lock drift; never write or accept the lock
        #[arg(long, requires = "explain")]
        preview: bool,
        /// Select a typed production target from the positional contract
        #[arg(long)]
        target: Option<String>,
    },
    /// Discover typed artifact targets without running builds
    #[cfg(unix)]
    #[command(after_help = "Exit codes:\n  0  targets listed\n  2  invalid or unreadable contract")]
    Targets {
        /// sykli-production-contract.v1 file
        #[arg(long, default_value = "sykli.production.json")]
        contract: PathBuf,
        /// Print one versioned JSON document on stdout; errors become sykli-production-error.v1 on stdout, exit code unchanged
        #[arg(long)]
        json: bool,
    },
    /// Produce a typed artifact from captured source (Linux and macOS)
    #[cfg(unix)]
    #[command(after_help = "Exit codes:\n  \
        0  the artifact is available and every required check passed\n  \
        1  unfinished or failed work (also after --stop-after or --prepare)\n  \
        2  invalid contract, unavailable tool, or store failure\n\n\
        The production ID printed is what `status` and `resume` take.")]
    Produce {
        /// Target name from the contract (see `sykli targets`)
        target: String,
        /// sykli-production-contract.v1 file
        #[arg(long, default_value = "sykli.production.json")]
        contract: PathBuf,
        /// Where captured inputs, artifacts and records are kept
        #[arg(long, default_value = ".sykli/production")]
        store: PathBuf,
        /// Stop at a durable operation boundary (incomplete delivery exits 1)
        #[arg(long)]
        stop_after: Option<String>,
        /// Capture the request without executing operations
        #[arg(long, conflicts_with = "stop_after")]
        prepare: bool,
        /// Maximum independent operations per execution wave
        #[arg(long, default_value_t = 1)]
        jobs: usize,
        /// Omit execution records and expose ready work
        #[arg(long)]
        summary: bool,
        /// Print one versioned JSON document on stdout; errors become sykli-production-error.v1 on stdout, exit code unchanged
        #[arg(long)]
        json: bool,
    },
    /// Inspect a pinned production and artifact availability
    #[cfg(unix)]
    #[command(
        after_help = "Exit codes:\n  0  records inspected (work may be unfinished)\n  2  unknown production or unreadable store"
    )]
    Status {
        /// Production ID printed by `produce`
        production: String,
        /// Where captured inputs, artifacts and records are kept
        #[arg(long, default_value = ".sykli/production")]
        store: PathBuf,
        /// Omit execution records and expose ready work
        #[arg(long)]
        summary: bool,
        /// Print one versioned JSON document on stdout; errors become sykli-production-error.v1 on stdout, exit code unchanged
        #[arg(long)]
        json: bool,
    },
    /// Continue a pinned production; failed work requires an explicit retry
    #[cfg(unix)]
    #[command(after_help = "Exit codes:\n  \
        0  the artifact is available and every required check passed\n  \
        1  unfinished or failed work\n  \
        2  unknown production, busy production, or store failure")]
    Resume {
        /// Production ID printed by `produce`
        production: String,
        /// Where captured inputs, artifacts and records are kept
        #[arg(long, default_value = ".sykli/production")]
        store: PathBuf,
        /// Run this failed operation again (requires --jobs 1)
        #[arg(long)]
        retry: Option<String>,
        /// Record a contact-lost attempt as abandoned so its operation can be retried
        #[arg(long)]
        abandon: Option<String>,
        /// Stop after this operation completes (requires --jobs 1)
        #[arg(long)]
        stop_after: Option<String>,
        /// Execute only this operation; dependencies must already be satisfied
        #[arg(long, conflicts_with = "stop_after")]
        operation: Option<String>,
        /// Maximum independent operations per execution wave
        #[arg(long, default_value_t = 1)]
        jobs: usize,
        /// Omit execution records and expose ready work
        #[arg(long)]
        summary: bool,
        /// Print one versioned JSON document on stdout; errors become sykli-production-error.v1 on stdout, exit code unchanged
        #[arg(long)]
        json: bool,
    },
    /// Check production record integrity, bindings, assessment and delivery
    #[cfg(unix)]
    #[command(
        after_help = "Exit codes:\n  0  verified and delivered\n  1  records are consistent but delivery is incomplete\n  2  inconsistent records or unreadable store"
    )]
    VerifyProduction {
        /// Production ID printed by `produce`
        production: String,
        /// Where captured inputs, artifacts and records are kept
        #[arg(long, default_value = ".sykli/production")]
        store: PathBuf,
        /// Print one versioned JSON document on stdout; errors become sykli-production-error.v1 on stdout, exit code unchanged
        #[arg(long)]
        json: bool,
    },
    /// Retrieve recorded diagnostics for one attempt, including historical attempts
    #[cfg(unix)]
    #[command(
        after_help = "Exit codes:\n  0  diagnostics printed\n  2  unknown production or attempt"
    )]
    Diagnostics {
        /// Production ID printed by `produce`
        production: String,
        /// Attempt ID from `status` or `resume` output
        attempt: String,
        /// Where captured inputs, artifacts and records are kept
        #[arg(long, default_value = ".sykli/production")]
        store: PathBuf,
        /// Print one versioned JSON document on stdout; errors become sykli-production-error.v1 on stdout, exit code unchanged
        #[arg(long)]
        json: bool,
    },
    #[cfg(unix)]
    #[command(name = "__production_attempt", hide = true)]
    ProductionAttempt {
        #[arg(long)]
        store: PathBuf,
        #[arg(long)]
        production: String,
        #[arg(long)]
        attempt: String,
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
        #[arg(long, value_name = "PATH", conflicts_with_all = ["path", "production"])]
        contract: Option<PathBuf>,
        /// Overwrite an existing contract
        #[arg(long)]
        force: bool,
        /// Do not pin the written contract in sykli.lock
        #[arg(long = "no-lock")]
        no_lock: bool,
        /// Write a typed executable target for Cargo, Go, or a standalone main.rs
        #[arg(long)]
        production: bool,
        /// Cargo package or Go main package (./cmd/NAME) to select
        #[arg(long, requires = "production")]
        package: Option<String>,
        /// Cargo binary to select for typed production
        #[arg(long, requires = "production")]
        bin: Option<String>,
        /// Shell check of $SYKLI_INPUT_executable (required for Cargo and Go discovery)
        #[arg(long, requires = "production")]
        smoke: Option<String>,
    },
    /// Pin the contract's hash in sykli.lock beside it (one entry per contract file)
    #[command(after_help = "Exit codes:\n  0  locked\n  2  could not read or hash the contract")]
    Lock {
        /// Path to a sykli-contract.v1 JSON file or a sykli.rs emitter (sykli.json when only it exists); may be outside the repository
        #[arg(default_value = "sykli.rs")]
        contract: PathBuf,
    },
    /// Read a pull request's runs and reviews through `gh`, save an evidence bundle, and show what is established
    #[command(after_help = "Exit codes:\n  \
        0  observations saved; with --requirements, every requirement established\n  \
        1  a requirement is refuted\n  \
        2  invalid input, unreadable candidate, or tool failure\n  \
        3  a requirement is unproven\n  \
        4  admitted evidence about one subject conflicts\n\n\
        Read-only and advisory: nothing is merged, triggered, or certified. See docs/agents.md.")]
    Inspect {
        /// Repository as OWNER/NAME on github.com
        #[arg(long)]
        repo: String,
        /// Pull request number
        #[arg(long)]
        pr: u64,
        /// sykli-requirements.v1 file; without it only observations are shown
        #[arg(long)]
        requirements: Option<PathBuf>,
        /// Where evidence bundles are appended
        #[arg(long, default_value = ".sykli/evidence")]
        store: PathBuf,
        /// Print one sykli-inspect.v1 document; errors become sykli-error.v1 on stdout
        #[arg(long)]
        json: bool,
    },
    /// Assess a saved evidence bundle against requirements, offline and deterministically
    #[command(after_help = "Exit codes:\n  \
        0  established   1  refuted   2  invalid input or tool failure\n  \
        3  unproven      4  conflict\n\n\
        Evaluates at the collection's end unless --at names another UTC time.")]
    Assess {
        /// Evidence bundle directory written by `sykli inspect`
        bundle: PathBuf,
        /// sykli-requirements.v1 file
        #[arg(long)]
        requirements: PathBuf,
        /// Evaluation time, YYYY-MM-DDTHH:MM:SSZ; later times can make evidence stale
        #[arg(long)]
        at: Option<String>,
        /// Print the sykli-assessment.v1 document
        #[arg(long)]
        json: bool,
        /// Print a Mermaid graph of the assessment instead of rows
        #[arg(long, conflicts_with = "json", value_parser = ["mermaid"])]
        graph: Option<String>,
        /// Explain one obligation: support, counterevidence, exclusions, missing evidence
        #[arg(long, conflicts_with = "graph")]
        why: Option<String>,
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
        (the receipt's lock_digest).\n\
        With --commit each declared input is judged against the commit at its exact\n\
        path: the tree entry, bytes and execute bit the run recorded must be the\n\
        commit's (bytes as stored, or as a checkout here converts them by this\n\
        repository's configuration and attributes: line endings, smudge filters),\n\
        and the records must hash to the receipt's inputs digest.\n\
        A symbolic link is judged with the declared input it names. A receipt whose\n\
        run read a file where the commit has none (an ignored or untracked file,\n\
        inside a submodule, spelled differently from git) is refused: the commit\n\
        cannot show it. An input a task declares as an output is generated by the\n\
        run and judged through that task's inputs.\n\
        On any failure the first failing check is also printed to stderr as one line.\n\n\
        A verified receipt is consistent, not proven. Verify establishes a valid\n\
        schema, records consistent with each other and the contract, the contract's\n\
        identity, and the tree and every declared input, read from the checkout or\n\
        the commit, never from the receipt. It does not establish where the receipt\n\
        came from, who produced it, or that its tasks ran: receipts are unsigned, so\n\
        an outcome rewritten to passed, or a receipt recomputed in full for a commit\n\
        with consistent digests, verifies. A converted input is bound only as this\n\
        checkout's configuration converts it.")]
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
}

impl InputCoverage {
    /// Why `run` would refuse this graph before executing it, if it would.
    fn refusal(&self) -> Option<String> {
        if !self.cargo_missing_inputs.is_empty() {
            Some(cargo_coverage_error(&self.cargo_missing_inputs))
        } else if !self.folder_missing_inputs.is_empty() {
            Some(folder_coverage_error(&self.folder_missing_inputs))
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
struct Inventory {
    cargo: bool,
    listed: std::cell::OnceCell<Result<Listed, String>>,
}

struct Listed {
    root: PathBuf,
    cached: Vec<PathBuf>,
    others: Vec<PathBuf>,
}

impl Inventory {
    fn new(contract: &Contract) -> Result<Self, String> {
        Ok(Inventory {
            cargo: is_cargo_graph(contract)?,
            listed: std::cell::OnceCell::new(),
        })
    }

    fn listed(&self) -> Result<&Listed, String> {
        self.listed
            .get_or_init(|| {
                let root = coverage_root()?;
                let mut args = vec!["ls-files", "--stage", "-z"];
                if self.cargo {
                    args.extend(["--others", "--exclude-standard"]);
                }
                let output = repo::git_command()
                    .arg("-C")
                    .arg(&root)
                    .args(&args)
                    .output()
                    .map_err(|error| error.to_string())?;
                if !output.status.success() {
                    return Err(String::from_utf8_lossy(&output.stderr).into());
                }
                let (mut cached, mut others) = (Vec::new(), Vec::new());
                for entry in output.stdout.split(|byte| *byte == 0) {
                    if entry.is_empty() {
                        continue;
                    }
                    let entry = std::str::from_utf8(entry)
                        .map_err(|_| "input coverage requires UTF-8 Git paths".to_string())?;
                    // `<mode> <object> <stage>\t<path>` for the index.
                    let staged = entry.split_once('\t').filter(|(stage, _)| {
                        let fields = stage.split(' ').collect::<Vec<_>>();
                        fields.len() == 3
                            && fields[0].len() == 6
                            && fields[0].bytes().all(|byte| matches!(byte, b'0'..=b'7'))
                            && fields[1].bytes().all(|byte| byte.is_ascii_hexdigit())
                            && matches!(fields[2], "0" | "1" | "2" | "3")
                    });
                    match staged {
                        Some((_, path)) => cached.push(absolute(&root.join(path))?),
                        None => others.push(absolute(&root.join(entry))?),
                    }
                }
                Ok(Listed {
                    root,
                    cached,
                    others,
                })
            })
            .as_ref()
            .map_err(Clone::clone)
    }
}

fn cargo_missing_inputs(contract: &Contract, inventory: &Inventory) -> Result<Vec<String>, String> {
    // This is checked on every invocation, before execution can reuse cached passes. Its
    // input is the current file inventory, not the contract's existing list.
    if !inventory.cargo {
        return Ok(Vec::new());
    }
    let listed = inventory.listed()?;
    let root = &listed.root;
    let declared = declared_inputs(contract)?;
    let mut missing = BTreeSet::new();
    for path in listed.cached.iter().chain(&listed.others) {
        let path = path.clone();
        if path.starts_with(root.join(".sykli")) {
            continue;
        }
        let relative = path.strip_prefix(root).unwrap_or(&path);
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let cargo_input = path.extension().is_some_and(|ext| ext == "rs")
            || matches!(
                name,
                "Cargo.toml"
                    | "Cargo.lock"
                    | "rust-toolchain"
                    | "rust-toolchain.toml"
                    | "rustfmt.toml"
                    | ".rustfmt.toml"
                    | "clippy.toml"
                    | ".clippy.toml"
            )
            || (path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == ".cargo")
                && matches!(name, "config" | "config.toml"))
            || relative
                .components()
                .any(|part| part.as_os_str() == "tests");
        if cargo_input && !declared.contains_key(&path) {
            match fs::symlink_metadata(&path) {
                Ok(_) => {
                    missing.insert(relative.to_string_lossy().into_owned());
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("cannot inspect {}: {error}", path.display())),
            }
        }
    }
    Ok(missing.into_iter().collect())
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
/// working directory is a folder like any other.
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
        if file.starts_with(root.join(".sykli")) {
            continue;
        }
        let Some(folder) = file.parent() else {
            continue;
        };
        for (name, declared, folders) in &tasks {
            if !folders.contains(folder) || declared.contains(file) {
                continue;
            }
            match fs::symlink_metadata(file) {
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
    let inventory = Inventory::new(contract)?;
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
    Ok(InputCoverage {
        base_commit,
        paths,
        cargo_missing_inputs: cargo_missing_inputs(contract, &inventory)?,
        folder_missing_inputs: folder_missing_inputs(contract, &inventory)?,
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
    levels: &[Vec<usize>],
    changed: &[PathBuf],
    selected: &[String],
) -> Result<Vec<TaskExplanation>, String> {
    let repository = git(&["rev-parse", "--show-toplevel"])
        .map_err(|_| "plan --explain requires a git repository".to_string())?;
    let runtime = graph_runtime(contract)?;
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
            match cache_key(task, &runtime, &after) {
                Err(message) => CacheExplanation::InputError { message },
                Ok(key) => match cache.inspect(task, &key, &runtime) {
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
        Some(resolved.join(path.strip_prefix(ancestor).ok()?))
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

#[cfg(unix)]
unsafe extern "C" {
    fn signal(signal: i32, handler: usize) -> usize;
}

fn main() -> ExitCode {
    // Rust ignores SIGPIPE, which turns `sykli plan --json | head` into a
    // panic on the first write after the reader leaves. Restore the default
    // so a closed pipe ends the process quietly, as it does for other tools.
    #[cfg(unix)]
    // SAFETY: SIGPIPE (13) with SIG_DFL (0); no handler code runs.
    unsafe {
        signal(13, 0);
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
            target: None,
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
        Command::Targets { contract, json } => {
            production::report(production::targets(&contract), json, false)
        }
        #[cfg(unix)]
        Command::Produce {
            target,
            contract,
            store,
            stop_after,
            prepare,
            jobs,
            summary,
            json,
        } => production::report(
            production::produce(
                &store,
                &contract,
                &target,
                stop_after.as_deref(),
                prepare,
                jobs,
            )
            .map(|v| if summary { production::compact(v) } else { v }),
            json,
            !prepare,
        ),
        #[cfg(unix)]
        Command::Status {
            production: id,
            store,
            summary,
            json,
        } => production::report(
            production::inspect(&store, &id)
                .map(|v| if summary { production::compact(v) } else { v }),
            json,
            false,
        ),
        #[cfg(unix)]
        Command::Diagnostics {
            production: id,
            attempt,
            store,
            json,
        } => production::report(production::diagnostics(&store, &id, &attempt), json, false),
        #[cfg(unix)]
        Command::VerifyProduction {
            production: id,
            store,
            json,
        } => production::report(production::inspect(&store, &id), json, true),
        #[cfg(unix)]
        Command::Resume {
            production: id,
            store,
            retry,
            abandon,
            stop_after,
            operation,
            jobs,
            summary,
            json,
        } => production::report(
            production::resume(
                &store,
                &id,
                abandon.as_deref(),
                retry.as_deref(),
                stop_after.as_deref(),
                operation.as_deref(),
                jobs,
            )
            .map(|v| if summary { production::compact(v) } else { v }),
            json,
            true,
        ),
        #[cfg(unix)]
        Command::ProductionAttempt {
            store,
            production: id,
            attempt,
            lease_fd,
        } => match production::executor(&store, &id, &attempt, lease_fd) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("executor: {error}");
                ExitCode::from(2)
            }
        },
        #[cfg(unix)]
        Command::Plan {
            contract,
            target: Some(target),
            changed,
            json,
            ..
        } => production::report(
            if changed.is_empty() {
                production::plan(contract.as_ref().unwrap(), &target)
            } else {
                Err("typed planning pins all selected inputs; --changed applies to declared graphs (sykli-contract.v1)".into())
            },
            json,
            false,
        ),
        Command::Validate { contract, json } => match load(&contract) {
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
            Err(error) => {
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
                ExitCode::FAILURE
            }
        },
        Command::Run { contract, json } => match load_pinned(&contract)
            .and_then(|((contract, levels, hash), lock)| run(&contract, &levels, hash, lock, json))
        {
            Ok((true, _)) => ExitCode::SUCCESS,
            Ok((false, _)) => ExitCode::FAILURE,
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
            target,
            explain,
            base,
            preview,
        } => {
            let contract = contract.unwrap();
            if target.is_some() {
                eprintln!("typed production requires Linux or macOS");
                return ExitCode::from(2);
            }
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
                let tasks = affected(&loaded, &levels, &changed)?;
                let mut explanations = if explain {
                    Some(explain_plan(&loaded, &levels, &changed, &tasks)?)
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
            production: typed,
            package,
            bin,
            smoke,
        } => {
            #[cfg(unix)]
            if typed {
                let path = if path == Path::new("sykli.json") {
                    PathBuf::from("sykli.production.json")
                } else {
                    path
                };
                return production::report(
                    production::init(
                        &path,
                        force,
                        package.as_deref(),
                        bin.as_deref(),
                        smoke.as_deref(),
                    ),
                    false,
                    false,
                );
            }
            #[cfg(not(unix))]
            let _ = (&package, &bin, &smoke);
            if typed {
                eprintln!("typed production requires Linux or macOS");
                return ExitCode::from(2);
            }
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
        Command::Inspect {
            repo,
            pr,
            requirements,
            store,
            json,
        } => inspect::inspect(&repo, pr, requirements.as_deref(), &store, json),
        Command::Assess {
            bundle,
            requirements,
            at,
            json,
            graph,
            why,
        } => inspect::assess(
            &bundle,
            &requirements,
            at.as_deref(),
            graph.as_deref(),
            why.as_deref(),
            json,
        ),
        Command::Verify {
            receipt,
            contract,
            commit,
        } => match verify(&receipt, &contract, commit.as_deref()) {
            Ok(checks) => report(&checks),
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(NOT_VERIFIABLE)
            }
        },
    }
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

/// The hash a lock file pins for this contract, if it pins one.
fn pinned(lock: LockFile, contract: &Path) -> Option<String> {
    match lock.contract_hash {
        Some(hash) => Some(hash),
        None => lock.contracts.get(&lock_key(contract)).cloned(),
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
        Some((lock, digest)) => (pinned(lock, path), Some(digest)),
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
    let hash = sha256(&serde_json::to_vec(&value).map_err(|error| error.to_string())?);
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
    let (_, _, contract_hash) = load_unlocked(contract)?;
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
/// property of the tree at execution time, not of the contract, and is tracked
/// separately.
fn validate_contract_path(task: &str, field: &str, path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err(format!("task {task:?} declares an empty {field} path"));
    }
    for component in path.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "task {task:?} {field} path {:?} must stay inside the repository",
                    path.display().to_string()
                ));
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    Ok(())
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

#[derive(Clone, Copy, PartialEq, Serialize)]
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
}

#[derive(Serialize)]
struct TaskReceipt {
    name: String,
    command: String,
    runtime_fingerprint: String,
    exit_code: Option<i32>,
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
    /// Digests of inherited environment values, by name; `absent` when unset.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    inherited_digests: BTreeMap<String, String>,
    outcome: Outcome,
    importable: bool,
    class: Option<&'static str>,
    retryable: bool,
    source: &'static str,
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

/// Whether every task passed or was cached, and where the receipt was written.
fn run(
    contract: &Contract,
    levels: &[Vec<usize>],
    contract_hash: String,
    lock_digest: Option<String>,
    json: bool,
) -> Result<(bool, PathBuf), String> {
    // Stamped before any task runs, so the receipt's run time brackets every
    // task time it contains.
    let run_started_at_ms = unix_time_ms();
    let inventory = Inventory::new(contract)?;
    let missing = cargo_missing_inputs(contract, &inventory)?;
    if !missing.is_empty() {
        return Err(cargo_coverage_error(&missing));
    }
    let folders = folder_missing_inputs(contract, &inventory)?;
    if !folders.is_empty() {
        return Err(folder_coverage_error(&folders));
    }
    let subject = subject(contract)?;
    let runtime = graph_runtime(contract)?;
    let cache = LocalCache::new(Path::new(&subject.repository));
    cache.bound();
    let tasks = execute(contract, levels, &runtime, &cache, json);
    let outcome = if tasks.iter().any(|task| task.outcome == Outcome::Errored) {
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
    // them, the same records the subject's inputs digest covers.
    let mut tasks = tasks;
    for record in &mut tasks {
        record.inputs = subject.inputs.get(&record.name).cloned();
    }
    let receipt = Receipt {
        schema: "sykli-receipt.v1",
        contract_hash,
        lock_digest,
        started_at_ms: run_started_at_ms,
        subject,
        tasks,
        outcome,
    };
    let path = write_receipt(&receipt)?;
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
    if json {
        serde_json::to_writer(io::stdout().lock(), &receipt).map_err(|error| error.to_string())?;
        println!();
    } else {
        println!("receipt: {}", path.display());
    }
    Ok((matches!(outcome, Outcome::Passed | Outcome::Cached), path))
}

fn execute(
    contract: &Contract,
    levels: &[Vec<usize>],
    runtime: &ShellRuntime,
    cache: &dyn Cache,
    json: bool,
) -> Vec<TaskReceipt> {
    let names: HashMap<_, _> = contract
        .tasks
        .iter()
        .enumerate()
        .map(|(index, task)| (task.name.as_str(), index))
        .collect();
    let mut receipts: Vec<Option<TaskReceipt>> = std::iter::repeat_with(|| None)
        .take(contract.tasks.len())
        .collect();

    for level in levels {
        let mut runnable = Vec::new();
        for &index in level {
            let task = &contract.tasks[index];
            if task.after.iter().any(|name| {
                !matches!(
                    receipts[names[name.as_str()]].as_ref().unwrap().outcome,
                    Outcome::Passed | Outcome::Cached
                )
            }) {
                progress(json, "blocked", &task.name);
                receipts[index] = Some(empty_task_receipt(
                    task,
                    runtime,
                    Outcome::Blocked,
                    "dependency failed",
                    "dependency_failed",
                    false,
                ));
            } else {
                let after: BTreeMap<&str, &str> = task
                    .after
                    .iter()
                    .map(|name| {
                        let key = receipts[names[name.as_str()]]
                            .as_ref()
                            .and_then(|record| record.cache_key.as_deref())
                            .unwrap_or("");
                        (name.as_str(), key)
                    })
                    .collect();
                match cache_key(task, runtime, &after) {
                    Ok(key) => {
                        if let Some(mut record) = cache.restore(task, &key, runtime) {
                            progress(json, "cached", &task.name);
                            record.cache_key = Some(key);
                            receipts[index] = Some(record);
                        } else {
                            runnable.push((index, key));
                        }
                    }
                    Err(error) => {
                        eprintln!("errored: {}", task.name);
                        receipts[index] = Some(empty_task_receipt(
                            task,
                            runtime,
                            Outcome::Errored,
                            &error,
                            "input_error",
                            false,
                        ));
                    }
                }
            }
        }

        let results = thread::scope(|scope| {
            runnable
                .into_iter()
                .map(|(index, key)| {
                    let task = &contract.tasks[index];
                    (
                        index,
                        key,
                        scope.spawn(move || {
                            run_task(task, runtime, json, MAX_CAPTURE_BYTES, Isolation::Shared)
                        }),
                    )
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|(index, key, handle)| {
                    let mut record = handle.join().unwrap_or_else(|_| {
                        empty_task_receipt(
                            &contract.tasks[index],
                            runtime,
                            Outcome::Errored,
                            "task thread panicked",
                            "runtime_error",
                            false,
                        )
                    });
                    record.cache_key = Some(key);
                    (index, record)
                })
                .collect::<Vec<_>>()
        });

        for (index, result) in results {
            let task = &contract.tasks[index];
            match result.outcome {
                Outcome::Passed => {
                    progress(json, "passed", &task.name);
                }
                Outcome::Failed => {
                    eprintln!("failed: {}", task.name);
                }
                Outcome::Errored => {
                    eprintln!("errored: {}", task.name);
                }
                Outcome::Cached | Outcome::Blocked => unreachable!(),
            }
            receipts[index] = Some(result);
        }
    }

    receipts.into_iter().map(Option::unwrap).collect()
}

fn progress(json: bool, status: &str, task: &str) {
    if json {
        eprintln!("{status}: {task}");
    } else {
        println!("{status}: {task}");
    }
}

fn cacheable(record: &TaskReceipt) -> bool {
    record.outcome == Outcome::Passed && record.importable
}

#[cfg(unix)]
unsafe extern "C" {
    fn setpgid(pid: i32, pgid: i32) -> i32;
    fn killpg(pgid: i32, signal: i32) -> i32;
}

/// How a task's shell relates to the caller's process group.
#[derive(Clone, Copy, PartialEq)]
#[cfg_attr(not(unix), allow(dead_code))] // only typed production, Unix-only, isolates
enum Isolation {
    /// Share the caller's group: Ctrl-C reaches the task, as a user expects.
    Shared,
    /// Own group: after the shell exits, the whole group is killed, so
    /// termination of every descendant is established, not assumed.
    OwnGroup,
}

fn run_task(
    task: &Task,
    runtime: &ShellRuntime,
    json: bool,
    capture_limit: usize,
    isolation: Isolation,
) -> TaskReceipt {
    progress(json, "running", &task.name);
    // Instant for the duration, wall clock for the claim: Instant is monotonic
    // and cannot be read as a time of day, SystemTime can jump but is what a
    // freshness policy compares against.
    let started_at_ms = unix_time_ms();
    let started = Instant::now();
    let mut command = ProcessCommand::new(&runtime.path);
    #[cfg(unix)]
    if isolation == Isolation::OwnGroup {
        use std::os::unix::process::CommandExt;
        // SAFETY: setpgid is async-signal-safe and allocates nothing.
        unsafe {
            command.pre_exec(|| {
                if setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    let _ = isolation;
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
        .envs(&task.env)
        .envs(
            task.inherit
                .iter()
                .filter_map(|name| std::env::var_os(name).map(|value| (name, value))),
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(workdir) = &task.workdir {
        command.current_dir(workdir);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return empty_task_receipt(
                task,
                runtime,
                Outcome::Errored,
                &error.to_string(),
                "spawn_error",
                false,
            );
        }
    };
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    #[cfg(unix)]
    let group = child.id() as i32;
    let (status, stdout, stderr) = thread::scope(|scope| {
        let stdout_writer: Box<dyn Write + Send> = if json {
            Box::new(io::stderr())
        } else {
            Box::new(io::stdout())
        };
        let stdout = scope.spawn(move || relay(stdout, stdout_writer, capture_limit));
        let stderr = scope.spawn(move || relay(stderr, io::stderr(), capture_limit));
        let status = child.wait().map_err(|error| error.to_string());
        let stdout = stdout.join().unwrap_or_else(|_| CapturedOutput {
            bytes: Vec::new(),
            digest: sha256(&[]),
            bytes_dropped: 0,
            error: Some("stdout reader panicked".into()),
        });
        let stderr = stderr.join().unwrap_or_else(|_| CapturedOutput {
            bytes: Vec::new(),
            digest: sha256(&[]),
            bytes_dropped: 0,
            error: Some("stderr reader panicked".into()),
        });
        (status, stdout, stderr)
    });
    #[cfg(unix)]
    if isolation == Isolation::OwnGroup {
        // The leader has exited; anything left in its group is a straggler.
        // SAFETY: killpg on a group id we created; ESRCH when already empty.
        unsafe {
            killpg(group, 9);
        }
    }

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
    let exit_code = match status {
        Ok(status) => {
            if !status.success() {
                outcome = Outcome::Failed;
                class = Some("command_failed");
                error = Some(status.to_string());
            }
            status.code()
        }
        Err(wait_error) => {
            outcome = Outcome::Errored;
            importable = false;
            class = Some("runtime_error");
            retryable = true;
            error = Some(wait_error);
            None
        }
    };

    if let Some(capture_error) = capture_error {
        importable = false;
        if outcome == Outcome::Passed {
            outcome = Outcome::Errored;
            class = Some("capture_error");
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
            class = Some("missing_output");
            error = Some(output_error);
        }
    }

    TaskReceipt {
        name: task.name.clone(),
        command: task.run.clone(),
        runtime_fingerprint: runtime.fingerprint.clone(),
        started_at_ms: Some(started_at_ms),
        exit_code,
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
        inherited_digests: inherited_digests(task),
        outcome,
        importable,
        class,
        retryable,
        source: "task",
        error,
        provenance: None,
        cache_key: None,
        inputs: None,
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
        inherited_digests: inherited_digests(task),
        outcome,
        importable: outcome != Outcome::Errored,
        class: Some(class),
        retryable,
        source: "task",
        error: Some(error.into()),
        provenance: None,
        cache_key: None,
        inputs: None,
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
}

impl InputRecord {
    fn digest(&self) -> Result<String, String> {
        let bytes = serde_json::to_vec(&("sykli-input.v2", &self.sha256, self.mode.unwrap_or(0)))
            .map_err(|error| error.to_string())?;
        Ok(sha256(&bytes))
    }
}

fn input_record(path: &Path) -> Result<InputRecord, String> {
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
    })
}

/// Every task's declared inputs as the run reads them: each input, as
/// declared, with its record, or None when it cannot be read (missing).
type InputRecords = BTreeMap<String, BTreeMap<String, Option<InputRecord>>>;

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
) -> Result<String, String> {
    #[derive(Serialize)]
    struct Key<'a> {
        task: &'a Task,
        inputs: BTreeMap<&'a str, String>,
        runtime: &'a str,
        after: &'a BTreeMap<&'a str, &'a str>,
        inherited: BTreeMap<&'a str, String>,
    }

    let root = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
    let mut inputs = BTreeMap::new();
    for input in &task.inputs {
        let path = root.join(input);
        // ponytail: declared inputs are files; add glob/tree hashing when a contract needs it.
        if !path.is_file() {
            return Err(format!("declared input {input:?} is missing or not a file"));
        }
        inputs.insert(input.as_str(), input_digest(&path)?);
    }
    let bytes = serde_json::to_vec(&Key {
        task,
        inputs,
        runtime: &runtime.fingerprint,
        after,
        inherited: inherited_digests(task)
            .iter()
            .map(|(name, digest)| (name.as_str(), digest.clone()))
            .collect(),
    })
    .map_err(|error| error.to_string())?;
    Ok(sha256(&bytes))
}

/// Digests of the inherited variables' current values, never the values.
/// An unset variable digests as `absent`, which is not any value's digest.
fn inherited_digests(task: &Task) -> BTreeMap<String, String> {
    task.inherit
        .iter()
        .map(|name| {
            let digest = match std::env::var_os(name) {
                Some(value) => sha256(value.as_encoded_bytes()),
                None => "absent".to_string(),
            };
            (name.clone(), digest)
        })
        .collect()
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
                            if let Ok((_, file)) = facts.file_behind(path) {
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
    fn inspect(
        &self,
        task: &Task,
        key: &str,
        runtime: &ShellRuntime,
    ) -> Result<CacheEntry, CacheMiss> {
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
        if !self.provenance_valid(task, &entry, runtime) {
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

    fn provenance_valid(&self, task: &Task, entry: &CacheEntry, runtime: &ShellRuntime) -> bool {
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
                    && record["runtime_fingerprint"] == runtime.fingerprint
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

fn restore_file(source: &Path, target: &Path, suffix: &str) -> Result<(), String> {
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
        let entry = self.inspect(task, key, runtime).ok()?;

        let root = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
        for output in &entry.outputs {
            let artifact = directory.join("outputs").join(&output.artifact);
            restore_file(&artifact, &root.join(&output.path), &output.artifact).ok()?;
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
            inherited_digests: inherited_digests(task),
            outcome: Outcome::Cached,
            importable: true,
            class: None,
            retryable: false,
            source: "cache",
            error: None,
            provenance: Some(entry.receipt),
            cache_key: None,
            inputs: None,
        })
    }

    fn store(&self, task: &Task, record: &TaskReceipt, receipt: &str) -> Result<(), String> {
        let Some(key) = &record.cache_key else {
            return Ok(());
        };
        fs::create_dir_all(&self.cache).map_err(|error| error.to_string())?;
        let temporary = self
            .cache
            .join(format!(".{key}.tmp-{}", std::process::id()));
        if temporary.exists() {
            fs::remove_dir_all(&temporary).map_err(|error| error.to_string())?;
        }
        fs::create_dir_all(temporary.join("outputs")).map_err(|error| error.to_string())?;

        let root = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
        let mut outputs = Vec::new();
        for (index, output) in task.outputs.iter().enumerate() {
            let digest = record
                .output_digests
                .get(output)
                .ok_or_else(|| format!("missing digest for output {output:?}"))?;
            let artifact = index.to_string();
            fs::copy(root.join(output), temporary.join("outputs").join(&artifact))
                .map_err(|error| error.to_string())?;
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
        fs::write(temporary.join("entry.json"), entry).map_err(|error| error.to_string())?;

        let target = self.cache.join(key);
        // ponytail: replacement is process-racy; add per-key locks if concurrent runs need them.
        if target.exists() {
            fs::remove_dir_all(&target).map_err(|error| error.to_string())?;
        }
        fs::rename(temporary, &target).map_err(|error| error.to_string())?;
        self.evict(&target);
        Ok(())
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

fn subject(contract: &Contract) -> Result<Subject, String> {
    let repository = repo::repository().map_err(|_| {
        "not inside a git repository; sykli binds every receipt to a git tree".to_string()
    })?;
    let declared = input_paths(contract)?;
    let paths = declared
        .iter()
        .map(|(_, _, path)| path.clone())
        .collect::<Vec<_>>();
    let (tree_oid, head_tree_oid, facts) = snapshot(repository, &paths)?;
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
fn snapshot(
    repository: &repo::Repository,
    paths: &[String],
) -> Result<(String, String, PathFacts), String> {
    let root = &repository.root;
    let state = TemporaryGitState::new(&object_directory_of(repository)?)?;
    let mut session = repository.session(vec![
        (
            "GIT_OBJECT_DIRECTORY",
            state.objects.clone().into_os_string(),
        ),
        ("GIT_ALTERNATE_OBJECT_DIRECTORIES", state.alternates.clone()),
    ]);
    let head = session.ask(&["HEAD^{tree}".into(), "HEAD:.sykli".into()])?;
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
    Ok((tree, head_tree, facts))
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
    let subject = subject(&contract)?;
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
}

impl Judged {
    /// A contract read from its file, with the lock beside it.
    fn from_file(path: &Path, outside: bool) -> Result<Self, String> {
        let (contract, _, hash) = load_unlocked(path)?;
        let (lock, digest) = read_lock_digest(path)?;
        Ok(Judged {
            contract,
            hash,
            lock,
            outside: outside.then(|| (path.to_path_buf(), digest)),
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
    let mut seen = HashSet::new();
    let unique = names
        .into_iter()
        .filter(|name| seen.insert(name.clone()))
        .collect::<Vec<_>>();
    let answers = session.ask(&unique)?;
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
/// level, by the names alone; None when that leaves the repository (an
/// absolute target, or `..` past the top).
fn link_target(link: &str, target: &str) -> Option<String> {
    if target.is_empty() || target.starts_with('/') {
        return None;
    }
    let mut parts = link.split('/').collect::<Vec<_>>();
    parts.pop();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// What one tree has at every declared input's path, and the target each
/// symbolic link among them names.
#[derive(Default)]
struct PathFacts {
    found: HashMap<String, Found>,
    links: HashMap<String, String>,
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
            let Some(next) = link_target(&at, target) else {
                return Err(format!(
                    "{at} is a symbolic link to {target}, outside the repository"
                ));
            };
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
    /// What a checkout here gives for a blob at a path, by path and blob:
    /// one `git cat-file --filters` each, only when the bytes the run read
    /// are not the stored ones.
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
                    stored.insert(path.clone(), sha256(&object.bytes));
                    if entry.starts_with("120000 ") {
                        facts.links.insert(
                            path.clone(),
                            String::from_utf8_lossy(&object.bytes).into_owned(),
                        );
                    }
                }
            }
            facts.found.insert(path.clone(), found);
        }
        tasks.entry(task).or_default().insert(input, path);
    }
    let mut outputs = Vec::new();
    for task in &contract.tasks {
        let root = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
        for output in &task.outputs {
            if let Ok(path) = declared_path(repository, &root.join(output)) {
                outputs.push((path, task.name.clone()));
            }
        }
    }
    Ok(InputsAt {
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
    /// the run generated it, from inputs judged in their own right.
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

    /// The SHA-256 of what a checkout here gives for blob `id` at `path`:
    /// `git cat-file --filters`, which applies the line-ending conversion
    /// and smudge filter this repository's configuration and attributes
    /// give that path, as a checkout would, read once per path and blob.
    fn checked_out(&self, path: &str, id: &str) -> Result<String, String> {
        let key = (path.to_string(), id.to_string());
        if let Some(digest) = self.checkouts.borrow().get(&key) {
            return Ok(digest.clone());
        }
        let repository = repo::repository()?;
        let output = repo::git_command()
            .arg("-C")
            .arg(&repository.root)
            .args(["cat-file", "--filters", &format!("--path={path}"), id])
            .stdin(Stdio::null())
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "git cat-file --filters cannot check out {path}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let digest = sha256(&output.stdout);
        self.checkouts.borrow_mut().insert(key, digest.clone());
        Ok(digest)
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
            "; the bytes of {converted} as a checkout here converts them, by this repository's \
             configuration and attributes"
        ));
    }
    if !generated.is_empty() {
        let generated = generated.into_iter().collect::<Vec<_>>().join(", ");
        actual.push_str(&format!(
            "; generated by the run, not in the commit, and judged through the tasks that \
             produce them: {generated}"
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
     bit, read from the checkout, or with --commit from the commit, never from the receipt (an \
     input a checkout converts, as a checkout here converts it; an input a task outputs, through \
     that task's own inputs)\n\
     not established: where the receipt came from, who produced it, that its tasks ran, or \
     anything they read that is not a declared input. Receipts are unsigned: an outcome rewritten \
     to passed, or a receipt recomputed in full for a commit with consistent digests, verifies; \
     and a converted input is bound only as this checkout's configuration converts it";

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
            Some(bytes) => pinned(
                parse_lock(&bytes, &format!("sykli.lock in commit {short}"))?,
                &path,
            ),
            None => None,
        };
        Ok(Judged {
            contract,
            hash,
            lock,
            outside: None,
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
    // that contract declares.
    let mut session = repository.session(Vec::new());
    let mut answers = ask_in(&mut session, names)?;
    let mut at = commit_from(rev, &answers)?;
    let judged = match inside {
        None => Judged::from_file(contract_path, true)?,
        Some(inside) => inside?.judged(&answers, &at.id)?,
    };
    answers.extend(ask_in(&mut session, input_names(rev, &judged.contract)?)?);
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
    record_checks(&mut checks, &receipt, &judged.contract);
    Ok(checks)
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

fn write_receipt(receipt: &Receipt) -> Result<PathBuf, String> {
    let bytes = serde_json::to_vec_pretty(receipt).map_err(|error| error.to_string())?;
    let hash = sha256(&bytes);
    let directory = Path::new(&receipt.subject.repository).join(".sykli/receipts");
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let path = directory.join(format!("rcpt_{hash}.json"));
    if !path.exists() {
        let temporary = directory.join(format!(".{hash}.tmp-{}", std::process::id()));
        fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
        if let Err(error) = fs::rename(&temporary, &path) {
            if path.exists() {
                let _ = fs::remove_file(temporary);
            } else {
                return Err(error.to_string());
            }
        }
    }
    Ok(path)
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
        let receipts = execute(
            &failure,
            &validate(&failure).unwrap(),
            &shell_runtime().unwrap(),
            &cache,
            false,
        );
        assert!(receipts[0].outcome == Outcome::Failed);
        assert!(receipts[1].outcome == Outcome::Blocked);
        assert!(receipts[2].outcome == Outcome::Failed);
        assert!(receipts[2].class == Some("missing_output"));

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
            true,
            4,
            Isolation::Shared,
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
            }],
        };
        let runtime = shell_runtime().unwrap();
        let cache = LocalCache::new(&root);
        let first = execute(&cached_contract, &[vec![0]], &runtime, &cache, false);
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
            },
            tasks: first,
            outcome: Outcome::Passed,
        };
        let receipt_path = write_receipt(&receipt).unwrap();
        cache
            .store(
                &cached_contract.tasks[0],
                &receipt.tasks[0],
                receipt_path.file_name().unwrap().to_str().unwrap(),
            )
            .unwrap();
        fs::write(root.join("result"), "stale").unwrap();
        let second = execute(&cached_contract, &[vec![0]], &runtime, &cache, false);
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
        let working_tree_oid = |_: &Path| snapshot(&repository, &[]).map(|(tree, _, _)| tree);

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
