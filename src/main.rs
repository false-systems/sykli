//! Sykli is the content-addressed evaluator for declared work graphs.
//!
//! A receipt claims exactly what ran — never what it meant.

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode, Stdio};
use std::thread;
use std::time::Instant;
use sykli::{Contract, Task};

mod init;

#[derive(Parser)]
#[command(
    name = "sykli",
    version,
    about = "Content-addressed evaluator for declared work graphs"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate an emitted contract without executing it
    Validate {
        /// Path to sykli.rs or a sykli-contract.v1 JSON file
        #[arg(default_value = "sykli.rs")]
        contract: PathBuf,
        /// Print the verdict as JSON (`sykli-validate.v1`); exit 1 when invalid
        #[arg(long)]
        json: bool,
    },
    /// Execute an emitted contract
    Run {
        /// Path to sykli.rs or a sykli-contract.v1 JSON file
        #[arg(default_value = "sykli.rs")]
        contract: PathBuf,
        /// Print the run receipt as JSON; progress and task output go to stderr
        #[arg(long)]
        json: bool,
    },
    /// Select tasks affected by changed files
    Plan {
        /// Path to sykli.rs or a sykli-contract.v1 JSON file
        #[arg(default_value = "sykli.rs")]
        contract: PathBuf,
        /// Changed file path; repeat for multiple files
        #[arg(long)]
        changed: Vec<PathBuf>,
        /// Print the plan as JSON
        #[arg(long)]
        json: bool,
    },
    /// Detect the repository's ecosystems and write a declared graph
    #[command(after_help = "Exit codes:\n  \
        0  wrote the contract\n  \
        1  nothing to declare — no Cargo.toml, package.json, or go.mod\n  \
        2  the contract exists; pass --force to overwrite it")]
    Init {
        /// Where to write the sykli-contract.v1 JSON
        #[arg(default_value = "sykli.json")]
        path: PathBuf,
        /// Overwrite an existing contract
        #[arg(long)]
        force: bool,
        /// Do not pin the written contract in sykli.lock
        #[arg(long = "no-lock")]
        no_lock: bool,
    },
    /// Pin the emitted contract in sykli.lock
    Lock {
        /// Path to sykli.rs or a sykli-contract.v1 JSON file
        #[arg(default_value = "sykli.rs")]
        contract: PathBuf,
    },
    /// Check that a receipt is consistent with the current tree and contract
    #[command(after_help = "Exit codes (first failing stage decides):\n  \
        0  verified\n  \
        1  outcome or evidence failed — the work is bad or incomplete\n  \
        2  cannot verify — not a receipt, unreadable input, git or contract error\n  \
        3  tree or input mismatch — receipt is stale; re-run sykli\n  \
        4  contract mismatch — contract drifted from the receipt; re-lock")]
    Verify {
        /// Path to a sykli-receipt.v1 JSON file
        receipt: PathBuf,
        /// Path to sykli.rs or a sykli-contract.v1 JSON file
        #[arg(long, default_value = "sykli.rs")]
        contract: PathBuf,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LockedContract {
    schema: String,
    contract_hash: String,
}

#[derive(Serialize)]
struct PlanOutput {
    schema: &'static str,
    contract_hash: String,
    tasks: Vec<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
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
        Command::Run { contract, json } => match load(&contract)
            .and_then(|(contract, levels, hash)| run(&contract, &levels, hash, json))
        {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Plan {
            contract,
            changed,
            json,
        } => match load(&contract).and_then(|(contract, levels, hash)| {
            affected(&contract, &levels, &changed).map(|tasks| (hash, tasks))
        }) {
            Ok((contract_hash, tasks)) => {
                if json {
                    serde_json::to_writer(
                        io::stdout().lock(),
                        &PlanOutput {
                            schema: "sykli-plan.v1",
                            contract_hash,
                            tasks,
                        },
                    )
                    .expect("write Sykli plan");
                    println!();
                } else {
                    for task in tasks {
                        println!("{task}");
                    }
                }
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("invalid {}: {error}", contract.display());
                ExitCode::FAILURE
            }
        },
        Command::Init {
            path,
            force,
            no_lock,
        } => init::run(&path, force, !no_lock, &write_lock),
        Command::Lock { contract } => match write_lock(&contract) {
            Ok(path) => {
                println!("locked: {}", path.display());
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Verify { receipt, contract } => match verify(&receipt, &contract) {
            Ok(checks) => report(&checks),
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(NOT_VERIFIABLE)
            }
        },
    }
}

fn load(path: &Path) -> Result<(Contract, Vec<Vec<usize>>, String), String> {
    let loaded = load_unlocked(path)?;
    if let Some(lock) = read_lock(path)? {
        if lock.contract_hash != loaded.2 {
            return Err(format!(
                "contract differs from {}; run `sykli lock` to accept it",
                lock_path(path).display()
            ));
        }
    }
    Ok(loaded)
}

fn read_lock(path: &Path) -> Result<Option<LockedContract>, String> {
    let lock_path = lock_path(path);
    if !lock_path.is_file() {
        return Ok(None);
    }
    let lock: LockedContract =
        serde_json::from_slice(&fs::read(&lock_path).map_err(|error| error.to_string())?)
            .map_err(|error| format!("invalid {}: {error}", lock_path.display()))?;
    if lock.schema != "sykli-lock.v1" {
        return Err(format!("unsupported lock schema {:?}", lock.schema));
    }
    Ok(Some(lock))
}

fn load_unlocked(path: &Path) -> Result<(Contract, Vec<Vec<usize>>, String), String> {
    let bytes = if path
        .extension()
        .is_some_and(|extension| extension == "json")
    {
        fs::read(path).map_err(|error| error.to_string())?
    } else {
        emit_contract(path)?
    };
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
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
    let mut bytes = serde_json::to_vec_pretty(&LockedContract {
        schema: "sykli-lock.v1".into(),
        contract_hash,
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
        if names.insert(task.name.as_str(), index).is_some() {
            return Err(format!("duplicate task name {:?}", task.name));
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
    dirty: bool,
}

#[derive(Serialize)]
struct TaskReceipt {
    name: String,
    command: String,
    runtime_fingerprint: String,
    exit_code: Option<i32>,
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
    outcome: Outcome,
    importable: bool,
    class: Option<&'static str>,
    retryable: bool,
    source: &'static str,
    error: Option<String>,
    provenance: Option<String>,
    #[serde(skip)]
    cache_key: Option<String>,
}

#[derive(Serialize)]
struct Receipt {
    schema: &'static str,
    contract_hash: String,
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
    // ponytail: unbounded until family receipts define a real size/age eviction budget.
    cache: PathBuf,
    receipts: PathBuf,
}

fn run(
    contract: &Contract,
    levels: &[Vec<usize>],
    contract_hash: String,
    json: bool,
) -> Result<bool, String> {
    let subject = subject(contract)?;
    let runtime = shell_runtime()?;
    let cache = LocalCache::new(Path::new(&subject.repository));
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
    let receipt = Receipt {
        schema: "sykli-receipt.v1",
        contract_hash,
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
    if json {
        serde_json::to_writer(io::stdout().lock(), &receipt).map_err(|error| error.to_string())?;
        println!();
    } else {
        println!("receipt: {}", path.display());
    }
    Ok(matches!(outcome, Outcome::Passed | Outcome::Cached))
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
                ));
            } else {
                match cache_key(task, runtime) {
                    Ok(key) => {
                        if let Some(record) = cache.restore(task, &key, runtime) {
                            progress(json, "cached", &task.name);
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
                        scope.spawn(move || run_task(task, runtime, json, MAX_CAPTURE_BYTES)),
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

fn run_task(task: &Task, runtime: &ShellRuntime, json: bool, capture_limit: usize) -> TaskReceipt {
    progress(json, "running", &task.name);
    let started = Instant::now();
    let mut command = ProcessCommand::new(&runtime.path);
    command
        .arg("-c")
        .arg(&task.run)
        .env_clear()
        .envs(
            runtime
                .environment
                .iter()
                .map(|(name, value)| (name, value)),
        )
        .envs(&task.env)
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
                "runtime_error",
            );
        }
    };
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
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
    let mut importable = stdout_bytes_dropped == 0 && stderr_bytes_dropped == 0;
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
        outcome,
        importable,
        class,
        retryable,
        source: "task",
        error,
        provenance: None,
        cache_key: None,
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
) -> TaskReceipt {
    TaskReceipt {
        name: task.name.clone(),
        command: task.run.clone(),
        runtime_fingerprint: runtime.fingerprint.clone(),
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
        outcome,
        importable: outcome != Outcome::Errored,
        class: Some(class),
        retryable: outcome == Outcome::Errored,
        source: "task",
        error: Some(error.into()),
        provenance: None,
        cache_key: None,
    }
}

fn cache_key(task: &Task, runtime: &ShellRuntime) -> Result<String, String> {
    #[derive(Serialize)]
    struct Key<'a> {
        task: &'a Task,
        inputs: BTreeMap<&'a str, String>,
        runtime: &'a str,
    }

    let root = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
    let mut inputs = BTreeMap::new();
    for input in &task.inputs {
        let path = root.join(input);
        // ponytail: declared inputs are files; add glob/tree hashing when a contract needs it.
        if !path.is_file() {
            return Err(format!("declared input {input:?} is missing or not a file"));
        }
        inputs.insert(input.as_str(), sha256_file(&path)?);
    }
    let bytes = serde_json::to_vec(&Key {
        task,
        inputs,
        runtime: &runtime.fingerprint,
    })
    .map_err(|error| error.to_string())?;
    Ok(sha256(&bytes))
}

fn declared_inputs_digest(contract: &Contract) -> Result<String, String> {
    let inputs: BTreeMap<_, _> = contract
        .tasks
        .iter()
        .map(|task| {
            let root = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
            let files: BTreeMap<_, _> = task
                .inputs
                .iter()
                .map(|input| (input, sha256_file(&root.join(input)).ok()))
                .collect();
            (&task.name, files)
        })
        .collect();
    serde_json::to_vec(&inputs)
        .map(|bytes| sha256(&bytes))
        .map_err(|error| error.to_string())
}

impl LocalCache {
    fn new(repository: &Path) -> Self {
        Self {
            cache: repository.join(".sykli/cache"),
            receipts: repository.join(".sykli/receipts"),
        }
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
        let entry: CacheEntry =
            serde_json::from_slice(&fs::read(directory.join("entry.json")).ok()?).ok()?;
        if entry.outputs.len() != task.outputs.len()
            || !entry.outputs.iter().zip(&task.outputs).enumerate().all(
                |(index, (output, path))| {
                    output.path == *path && output.artifact == index.to_string()
                },
            )
            || !self.provenance_valid(task, &entry, runtime)
        {
            return None;
        }

        for output in &entry.outputs {
            let artifact = directory.join("outputs").join(&output.artifact);
            if !artifact.is_file() || sha256_file(&artifact).ok()? != output.digest {
                return None;
            }
        }

        let root = task.workdir.as_deref().unwrap_or_else(|| Path::new("."));
        for output in &entry.outputs {
            let artifact = directory.join("outputs").join(&output.artifact);
            restore_file(&artifact, &root.join(&output.path), &output.artifact).ok()?;
        }

        let output_digests = entry
            .outputs
            .iter()
            .map(|output| (output.path.clone(), output.digest.clone()))
            .collect();
        Some(TaskReceipt {
            name: task.name.clone(),
            command: task.run.clone(),
            runtime_fingerprint: runtime.fingerprint.clone(),
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
            outcome: Outcome::Cached,
            importable: true,
            class: None,
            retryable: false,
            source: "cache",
            error: None,
            provenance: Some(entry.receipt),
            cache_key: None,
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
        })
        .map_err(|error| error.to_string())?;
        fs::write(temporary.join("entry.json"), entry).map_err(|error| error.to_string())?;

        let target = self.cache.join(key);
        // ponytail: replacement is process-racy; add per-key locks if concurrent runs need them.
        if target.exists() {
            fs::remove_dir_all(&target).map_err(|error| error.to_string())?;
        }
        fs::rename(temporary, target).map_err(|error| error.to_string())
    }
}

fn shell_runtime() -> Result<ShellRuntime, String> {
    let output = ProcessCommand::new("sh")
        .args(["-c", "command -v sh"])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err("cannot resolve shell runtime".into());
    }
    let path = fs::canonicalize(String::from_utf8_lossy(&output.stdout).trim())
        .map_err(|error| error.to_string())?;
    let environment = inherited_environment(std::env::vars_os());
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

fn inherited_environment(
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    environment
        .into_iter()
        .filter(|(name, _)| matches!(name.to_str(), Some("PATH" | "HOME" | "TMPDIR")))
        .collect()
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
    let repository = git(&["rev-parse", "--show-toplevel"])?;
    let head_tree_oid = git(&["rev-parse", "HEAD^{tree}"])?;
    let tree_oid = working_tree_oid(Path::new(&repository))?;
    let inputs_digest = declared_inputs_digest(contract)?;
    let dirty = tree_oid != head_tree_oid;
    Ok(Subject {
        repository,
        tree_oid,
        inputs_digest,
        head_tree_oid,
        dirty,
    })
}

/// Content-address the working tree itself: stage every non-ignored file into
/// an ephemeral index and let git compute the tree OID. `.sykli` is always
/// excluded so receipts and cache entries never perturb the tree they witness.
fn working_tree_oid(repository: &Path) -> Result<String, String> {
    let state = TemporaryGitState::new(&git_object_directory(repository)?)?;
    git_with_temporary_state(repository, &state, &["add", "--all"])
        .and_then(|_| {
            git_with_temporary_state(
                repository,
                &state,
                &["rm", "--cached", "-r", "-q", "--ignore-unmatch", ".sykli"],
            )
        })
        .and_then(|_| git_with_temporary_state(repository, &state, &["write-tree"]))
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

fn git_object_directory(repository: &Path) -> Result<PathBuf, String> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repository)
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "objects",
        ])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().into());
    }
    let objects = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    if !objects.is_dir() {
        return Err(format!(
            "Git object directory {} is unavailable",
            objects.display()
        ));
    }
    Ok(objects)
}

fn git_with_temporary_state(
    repository: &Path,
    state: &TemporaryGitState,
    args: &[&str],
) -> Result<String, String> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .env("GIT_INDEX_FILE", &state.index)
        .env("GIT_OBJECT_DIRECTORY", &state.objects)
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &state.alternates)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().into())
}

fn git(args: &[&str]) -> Result<String, String> {
    let output = ProcessCommand::new("git")
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
    subject: SubjectSummary,
    tasks: Vec<TaskSummary>,
    outcome: String,
}

#[derive(Deserialize)]
struct SubjectSummary {
    tree_oid: String,
    inputs_digest: String,
}

#[derive(Deserialize)]
struct TaskSummary {
    name: String,
    outcome: String,
    importable: bool,
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
fn verify(receipt_path: &Path, contract_path: &Path) -> Result<Vec<Check>, String> {
    let receipt: ReceiptSummary =
        serde_json::from_slice(&fs::read(receipt_path).map_err(|error| error.to_string())?)
            .map_err(|error| format!("invalid {}: {error}", receipt_path.display()))?;
    let (contract, _, contract_hash) = load_unlocked(contract_path)?;
    let mut checks = vec![Check::equals(
        "schema",
        NOT_VERIFIABLE,
        "sykli-receipt.v1",
        receipt.schema,
    )];
    if checks.iter().any(|check| !check.ok) {
        return Ok(checks);
    }
    if let Some(lock) = read_lock(contract_path)? {
        checks.push(Check::equals(
            "contract lock",
            CONTRACT_MISMATCH,
            &contract_hash,
            lock.contract_hash,
        ));
    }
    checks.push(Check::equals(
        "contract",
        CONTRACT_MISMATCH,
        &contract_hash,
        receipt.contract_hash,
    ));
    if checks.iter().any(|check| !check.ok) {
        return Ok(checks);
    }
    let subject = subject(&contract)?;
    checks.extend([
        Check::equals(
            "tree",
            TREE_MISMATCH,
            subject.tree_oid,
            receipt.subject.tree_oid,
        ),
        Check::equals(
            "inputs",
            TREE_MISMATCH,
            subject.inputs_digest,
            receipt.subject.inputs_digest,
        ),
    ]);
    if checks.iter().any(|check| !check.ok) {
        return Ok(checks);
    }
    let tasks_ok = receipt.tasks.len() == contract.tasks.len()
        && receipt
            .tasks
            .iter()
            .zip(&contract.tasks)
            .all(|(record, task)| {
                record.name == task.name
                    && record.importable
                    && matches!(record.outcome.as_str(), "passed" | "cached")
            });
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
    Ok(checks)
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
            ExitCode::SUCCESS
        }
        Some(first) => ExitCode::from(first.failure_code),
    }
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
        let truncated = run_task(&truncates.tasks[0], &shell_runtime().unwrap(), true, 4);
        assert!(truncated.stdout_truncated);
        assert_eq!(truncated.stdout_bytes_dropped, 4);
        assert!(!truncated.importable);
        assert!(!cacheable(&truncated));

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
            }],
        };
        let runtime = shell_runtime().unwrap();
        let cache = LocalCache::new(&root);
        let first = execute(&cached_contract, &[vec![0]], &runtime, &cache, false);
        let receipt = Receipt {
            schema: "sykli-receipt.v1",
            contract_hash: "test".into(),
            subject: Subject {
                repository: root.to_string_lossy().into(),
                tree_oid: "test".into(),
                inputs_digest: "test".into(),
                head_tree_oid: "test".into(),
                dirty: false,
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
