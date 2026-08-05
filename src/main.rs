//! Sykli executes declared graphs and proves what ran.
//!
//! A receipt claims exactly what ran — never what it meant.

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode, Stdio};
use std::thread;
use std::time::Instant;
use sykli::{Contract, Task};

#[derive(Parser)]
#[command(
    name = "sykli",
    version,
    about = "Executes declared graphs and proves what ran"
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
    },
    /// Execute an emitted contract
    Run {
        /// Path to sykli.rs or a sykli-contract.v1 JSON file
        #[arg(default_value = "sykli.rs")]
        contract: PathBuf,
    },
    /// Select tasks affected by changed files
    Plan {
        /// Path to sykli.rs or a sykli-contract.v1 JSON file
        #[arg(default_value = "sykli.rs")]
        contract: PathBuf,
        /// Changed file path; repeat for multiple files
        #[arg(long, required = true)]
        changed: Vec<PathBuf>,
    },
    /// Pin the emitted contract in sykli.lock
    Lock {
        /// Path to sykli.rs or a sykli-contract.v1 JSON file
        #[arg(default_value = "sykli.rs")]
        contract: PathBuf,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LockedContract {
    schema: String,
    contract_hash: String,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Validate { contract } => match load(&contract) {
            Ok(_) => {
                println!("valid: {}", contract.display());
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("invalid {}: {error}", contract.display());
                ExitCode::FAILURE
            }
        },
        Command::Run { contract } => match load(&contract)
            .and_then(|(contract, levels, hash)| run(&contract, &levels, hash))
        {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Plan { contract, changed } => match load(&contract)
            .and_then(|(contract, levels, _)| affected(&contract, &levels, &changed))
        {
            Ok(tasks) => {
                for task in tasks {
                    println!("{task}");
                }
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("invalid {}: {error}", contract.display());
                ExitCode::FAILURE
            }
        },
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
    }
}

fn load(path: &Path) -> Result<(Contract, Vec<Vec<usize>>, String), String> {
    let loaded = load_unlocked(path)?;
    let lock_path = lock_path(path);
    if lock_path.is_file() {
        let lock: LockedContract =
            serde_json::from_slice(&fs::read(&lock_path).map_err(|error| error.to_string())?)
                .map_err(|error| format!("invalid {}: {error}", lock_path.display()))?;
        if lock.schema != "sykli-lock.v1" {
            return Err(format!("unsupported lock schema {:?}", lock.schema));
        }
        if lock.contract_hash != loaded.2 {
            return Err(format!(
                "contract differs from {}; run `sykli lock` to accept it",
                lock_path.display()
            ));
        }
    }
    Ok(loaded)
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
    tree_oid: String,
    dirty: bool,
}

#[derive(Serialize)]
struct TaskReceipt {
    name: String,
    command: String,
    runtime_fingerprint: String,
    exit_code: Option<i32>,
    duration_ms: u64,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
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

fn run(contract: &Contract, levels: &[Vec<usize>], contract_hash: String) -> Result<bool, String> {
    let subject = subject()?;
    let runtime = shell_runtime()?;
    let cache = LocalCache::new(Path::new(&subject.repository));
    let tasks = execute(contract, levels, &runtime, &cache);
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
        if record.outcome == Outcome::Passed {
            if let Err(error) = cache.store(task, record, &receipt_name) {
                eprintln!("cache write failed for {}: {error}", task.name);
            }
        }
    }
    println!("receipt: {}", path.display());
    Ok(matches!(outcome, Outcome::Passed | Outcome::Cached))
}

fn execute(
    contract: &Contract,
    levels: &[Vec<usize>],
    runtime: &ShellRuntime,
    cache: &dyn Cache,
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
                println!("blocked: {}", task.name);
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
                            println!("cached: {}", task.name);
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
                    (index, key, scope.spawn(move || run_task(task, runtime)))
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
                    println!("passed: {}", task.name);
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

fn run_task(task: &Task, runtime: &ShellRuntime) -> TaskReceipt {
    println!("running: {}", task.name);
    let started = Instant::now();
    let mut command = ProcessCommand::new(&runtime.path);
    command
        .arg("-c")
        .arg(&task.run)
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
    let (status, (stdout, stdout_error), (stderr, stderr_error)) = thread::scope(|scope| {
        let stdout = scope.spawn(move || relay(stdout, io::stdout()));
        let stderr = scope.spawn(move || relay(stderr, io::stderr()));
        let status = child.wait().map_err(|error| error.to_string());
        let stdout = stdout
            .join()
            .unwrap_or_else(|_| (Vec::new(), Some("stdout reader panicked".into())));
        let stderr = stderr
            .join()
            .unwrap_or_else(|_| (Vec::new(), Some("stderr reader panicked".into())));
        (status, stdout, stderr)
    });

    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut output_digests = BTreeMap::new();
    let mut outcome = Outcome::Passed;
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
        exit_code,
        duration_ms,
        stdout_digest: sha256(&stdout),
        stderr_digest: sha256(&stderr),
        stdout,
        stderr,
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

fn relay(mut reader: impl Read, mut writer: impl Write) -> (Vec<u8>, Option<String>) {
    let mut captured = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return (captured, None),
            Ok(count) => {
                captured.extend_from_slice(&buffer[..count]);
                let _ = writer.write_all(&buffer[..count]);
                let _ = writer.flush();
            }
            Err(error) => return (captured, Some(error.to_string())),
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
        stdout: Vec::new(),
        stderr: Vec::new(),
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
    if target.exists() {
        if let Err(error) = fs::remove_file(target) {
            let _ = fs::remove_file(&temporary);
            return Err(error.to_string());
        }
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
            stdout: Vec::new(),
            stderr: Vec::new(),
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
    let fingerprint = format!("shell:{}:sha256:{}", path.display(), sha256_file(&path)?);
    Ok(ShellRuntime { path, fingerprint })
}

fn subject() -> Result<Subject, String> {
    Ok(Subject {
        repository: git(&["rev-parse", "--show-toplevel"])?,
        tree_oid: git(&["rev-parse", "HEAD^{tree}"])?,
        dirty: !git(&["status", "--porcelain"])?.is_empty(),
    })
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
        );
        assert!(receipts[0].outcome == Outcome::Failed);
        assert!(receipts[1].outcome == Outcome::Blocked);
        assert!(receipts[2].outcome == Outcome::Failed);
        assert!(receipts[2].class == Some("missing_output"));

        let (captured, error) = relay(&b"captured"[..], io::sink());
        assert_eq!(captured, b"captured");
        assert!(error.is_none());

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
        let first = execute(&cached_contract, &[vec![0]], &runtime, &cache);
        let receipt = Receipt {
            schema: "sykli-receipt.v1",
            contract_hash: "test".into(),
            subject: Subject {
                repository: root.to_string_lossy().into(),
                tree_oid: "test".into(),
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
        fs::remove_file(root.join("result")).unwrap();
        let second = execute(&cached_contract, &[vec![0]], &runtime, &cache);
        assert!(second[0].outcome == Outcome::Cached);
        assert_eq!(fs::read(root.join("result")).unwrap(), b"cache");
        fs::remove_dir_all(root).unwrap();
    }
}
