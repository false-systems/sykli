//! Contract parsing, validation, planning, and execution for Sykli.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub const CONTRACT_SCHEMA: &str = "sykli-contract.v1";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub schema: String,
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub name: String,
    pub run: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidContract {
    pub contract: Contract,
    pub contract_hash: String,
    pub levels: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub schema: String,
    pub contract_hash: String,
    pub levels: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Receipt {
    pub schema: String,
    pub contract_hash: String,
    pub started_at_ms: u128,
    pub finished_at_ms: u128,
    pub levels: Vec<Vec<String>>,
    pub tasks: Vec<TaskReceipt>,
    pub outcome: RunOutcome,
}

#[derive(Debug, Clone, Serialize)]
pub struct StoredReceipt {
    pub receipt_path: PathBuf,
    pub receipt_hash: String,
    pub receipt: Receipt,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Passed,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskReceipt {
    pub name: String,
    pub command: String,
    pub outcome: TaskOutcome,
    pub exit_code: Option<i32>,
    pub started_at_ms: u128,
    pub finished_at_ms: u128,
    pub duration_ms: u128,
    pub stdout: String,
    pub stderr: String,
    pub stdout_sha256: String,
    pub stderr_sha256: String,
    pub missing_outputs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcome {
    Passed,
    Failed,
    Blocked,
    Errored,
}

#[derive(Debug)]
pub enum Error {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    Validation(String),
    Serialize(serde_json::Error),
    ThreadPanic(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { path, source } => write!(f, "{}: {}", path.display(), source),
            Error::Json { path, source } => write!(f, "{}: {}", path.display(), source),
            Error::Validation(message) => f.write_str(message),
            Error::Serialize(source) => write!(f, "failed to serialize contract: {source}"),
            Error::ThreadPanic(task) => write!(f, "task thread panicked: {task}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub fn load_contract(path: &Path) -> Result<Contract> {
    let bytes = fs::read(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| Error::Json {
        path: path.to_path_buf(),
        source,
    })
}

pub fn validate_contract(contract: Contract) -> Result<ValidContract> {
    if contract.schema != CONTRACT_SCHEMA {
        return Err(Error::Validation(format!(
            "unsupported contract schema {:?}; expected {CONTRACT_SCHEMA:?}",
            contract.schema
        )));
    }

    if contract.tasks.is_empty() {
        return Err(Error::Validation(
            "contract must declare at least one task".into(),
        ));
    }

    let mut names = BTreeSet::new();
    for task in &contract.tasks {
        validate_task_shape(task)?;
        if !names.insert(task.name.clone()) {
            return Err(Error::Validation(format!(
                "duplicate task name {:?}",
                task.name
            )));
        }
    }

    for task in &contract.tasks {
        let mut deps = BTreeSet::new();
        for dep in &task.after {
            if dep == &task.name {
                return Err(Error::Validation(format!(
                    "task {:?} depends on itself",
                    task.name
                )));
            }
            if !names.contains(dep) {
                return Err(Error::Validation(format!(
                    "task {:?} depends on unknown task {:?}",
                    task.name, dep
                )));
            }
            if !deps.insert(dep) {
                return Err(Error::Validation(format!(
                    "task {:?} declares dependency {:?} more than once",
                    task.name, dep
                )));
            }
        }
    }

    let levels = plan_levels(&contract)?;
    let contract_hash = contract_hash(&contract)?;
    Ok(ValidContract {
        contract,
        contract_hash,
        levels,
    })
}

pub fn plan(valid: &ValidContract) -> Plan {
    Plan {
        schema: "sykli-plan.v1".into(),
        contract_hash: valid.contract_hash.clone(),
        levels: valid.levels.clone(),
    }
}

pub fn run(valid: &ValidContract, root: &Path) -> Result<Receipt> {
    let run_started_at_ms = unix_time_ms();
    let mut completed = BTreeMap::<String, TaskOutcome>::new();
    let mut task_runs = Vec::new();
    let by_name: BTreeMap<_, _> = valid
        .contract
        .tasks
        .iter()
        .map(|task| (task.name.clone(), task.clone()))
        .collect();

    for level in &valid.levels {
        let mut handles = Vec::new();
        for name in level {
            let task = by_name
                .get(name)
                .expect("validated plan names existing tasks")
                .clone();
            if task
                .after
                .iter()
                .any(|dep| completed.get(dep) != Some(&TaskOutcome::Passed))
            {
                task_runs.push(blocked_task_run(&task));
                completed.insert(task.name.clone(), TaskOutcome::Blocked);
            } else {
                let root = root.to_path_buf();
                handles.push((
                    task.name.clone(),
                    thread::spawn(move || execute_task(&task, &root)),
                ));
            }
        }

        for (name, handle) in handles {
            let task_run = handle.join().map_err(|_| Error::ThreadPanic(name))?;
            completed.insert(task_run.name.clone(), task_run.outcome.clone());
            task_runs.push(task_run);
        }
    }

    let outcome = if task_runs
        .iter()
        .all(|task| task.outcome == TaskOutcome::Passed)
    {
        RunOutcome::Passed
    } else {
        RunOutcome::Failed
    };

    Ok(Receipt {
        schema: "sykli-receipt.v1".into(),
        contract_hash: valid.contract_hash.clone(),
        started_at_ms: run_started_at_ms,
        finished_at_ms: unix_time_ms(),
        levels: valid.levels.clone(),
        tasks: task_runs,
        outcome,
    })
}

pub fn write_receipt(receipt: Receipt, receipt_dir: &Path) -> Result<StoredReceipt> {
    fs::create_dir_all(receipt_dir).map_err(|source| Error::Io {
        path: receipt_dir.to_path_buf(),
        source,
    })?;
    let bytes = serde_json::to_vec_pretty(&receipt).map_err(Error::Serialize)?;
    let receipt_hash = sha256_hex(&bytes);
    let receipt_path = receipt_dir.join(format!("rcpt_{receipt_hash}.json"));
    fs::write(&receipt_path, &bytes).map_err(|source| Error::Io {
        path: receipt_path.clone(),
        source,
    })?;
    Ok(StoredReceipt {
        receipt_path,
        receipt_hash,
        receipt,
    })
}

pub fn load_valid_contract(path: &Path) -> Result<ValidContract> {
    validate_contract(load_contract(path)?)
}

fn validate_task_shape(task: &Task) -> Result<()> {
    if task.name.trim().is_empty() {
        return Err(Error::Validation("task name must not be empty".into()));
    }
    if task.run.trim().is_empty() {
        return Err(Error::Validation(format!(
            "task {:?} run command must not be empty",
            task.name
        )));
    }
    if let Some(runtime) = &task.runtime {
        if runtime != "shell" {
            return Err(Error::Validation(format!(
                "task {:?} has unsupported runtime {:?}; expected \"shell\"",
                task.name, runtime
            )));
        }
    }
    if let Some(workdir) = &task.workdir {
        validate_relative_path(&task.name, "workdir", workdir)?;
    }
    for input in &task.inputs {
        validate_relative_path(&task.name, "input", input)?;
    }
    for output in &task.outputs {
        validate_relative_path(&task.name, "output", output)?;
    }
    for key in task.env.keys() {
        if key.is_empty() || key.contains('=') {
            return Err(Error::Validation(format!(
                "task {:?} has invalid env key {:?}",
                task.name, key
            )));
        }
    }
    Ok(())
}

fn validate_relative_path(task: &str, field: &str, path: &Path) -> Result<()> {
    if path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(Error::Validation(format!(
            "task {task:?} {field} path {:?} must stay inside the contract directory",
            path.display().to_string()
        )));
    }
    Ok(())
}

fn contract_hash(contract: &Contract) -> Result<String> {
    let bytes = serde_json::to_vec(contract).map_err(Error::Serialize)?;
    Ok(sha256_hex(&bytes))
}

fn plan_levels(contract: &Contract) -> Result<Vec<Vec<String>>> {
    let mut dependents = BTreeMap::<String, Vec<String>>::new();
    let mut indegree = BTreeMap::<String, usize>::new();

    for task in &contract.tasks {
        indegree.insert(task.name.clone(), task.after.len());
        for dep in &task.after {
            dependents
                .entry(dep.clone())
                .or_default()
                .push(task.name.clone());
        }
    }

    let mut ready = indegree
        .iter()
        .filter_map(|(name, count)| (*count == 0).then_some(name.clone()))
        .collect::<VecDeque<_>>();
    let mut levels = Vec::new();
    let mut planned = 0usize;

    while !ready.is_empty() {
        let mut level = Vec::new();
        for _ in 0..ready.len() {
            let name = ready.pop_front().expect("ready length fixed for level");
            planned += 1;
            level.push(name.clone());
            if let Some(children) = dependents.get(&name) {
                for child in children {
                    let count = indegree
                        .get_mut(child)
                        .expect("validated dependency child present");
                    *count -= 1;
                    if *count == 0 {
                        ready.push_back(child.clone());
                    }
                }
            }
        }
        levels.push(level);
    }

    if planned != contract.tasks.len() {
        let cycle_tasks = indegree
            .into_iter()
            .filter_map(|(name, count)| (count > 0).then_some(name))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::Validation(format!(
            "contract dependency graph contains a cycle involving: {cycle_tasks}"
        )));
    }

    Ok(levels)
}

fn execute_task(task: &Task, root: &Path) -> TaskReceipt {
    let started_at_ms = unix_time_ms();
    let started = Instant::now();
    let mut command = Command::new("sh");
    command.arg("-c").arg(&task.run);
    command.current_dir(task_workdir(task, root));
    command.envs(&task.env);

    match command.output() {
        Ok(output) => {
            let finished_at_ms = unix_time_ms();
            let missing_outputs = missing_outputs(task, root);
            let exit_code = output.status.code();
            let command_passed = output.status.success();
            let outputs_present = missing_outputs.is_empty();
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            TaskReceipt {
                name: task.name.clone(),
                command: task.run.clone(),
                outcome: if command_passed && outputs_present {
                    TaskOutcome::Passed
                } else {
                    TaskOutcome::Failed
                },
                exit_code,
                started_at_ms,
                finished_at_ms,
                duration_ms: started.elapsed().as_millis(),
                stdout,
                stderr,
                stdout_sha256: sha256_hex(&output.stdout),
                stderr_sha256: sha256_hex(&output.stderr),
                missing_outputs,
            }
        }
        Err(source) => {
            let stderr = source.to_string();
            TaskReceipt {
                name: task.name.clone(),
                command: task.run.clone(),
                outcome: TaskOutcome::Errored,
                exit_code: None,
                started_at_ms,
                finished_at_ms: unix_time_ms(),
                duration_ms: started.elapsed().as_millis(),
                stdout: String::new(),
                stderr: stderr.clone(),
                stdout_sha256: sha256_hex(b""),
                stderr_sha256: sha256_hex(stderr.as_bytes()),
                missing_outputs: Vec::new(),
            }
        }
    }
}

fn blocked_task_run(task: &Task) -> TaskReceipt {
    let timestamp = unix_time_ms();
    TaskReceipt {
        name: task.name.clone(),
        command: task.run.clone(),
        outcome: TaskOutcome::Blocked,
        exit_code: None,
        started_at_ms: timestamp,
        finished_at_ms: timestamp,
        duration_ms: 0,
        stdout: String::new(),
        stderr: String::new(),
        stdout_sha256: sha256_hex(b""),
        stderr_sha256: sha256_hex(b""),
        missing_outputs: Vec::new(),
    }
}

fn task_workdir(task: &Task, root: &Path) -> PathBuf {
    task.workdir
        .as_ref()
        .map(|workdir| root.join(workdir))
        .unwrap_or_else(|| root.to_path_buf())
}

fn missing_outputs(task: &Task, root: &Path) -> Vec<String> {
    let base = task_workdir(task, root);
    task.outputs
        .iter()
        .filter(|path| !base.join(path).exists())
        .map(|path| path.display().to_string())
        .collect()
}

fn unix_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after Unix epoch")
        .as_millis()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract(tasks: Vec<Task>) -> Contract {
        Contract {
            schema: CONTRACT_SCHEMA.into(),
            tasks,
        }
    }

    fn task(name: &str, after: &[&str]) -> Task {
        Task {
            name: name.into(),
            run: "true".into(),
            workdir: None,
            env: BTreeMap::new(),
            after: after.iter().map(|dep| (*dep).into()).collect(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            runtime: None,
        }
    }

    #[test]
    fn validates_and_plans_dependency_levels() {
        let valid = validate_contract(contract(vec![
            task("build", &[]),
            task("lint", &[]),
            task("test", &["build"]),
            task("gate", &["lint", "test"]),
        ]))
        .expect("valid contract");

        assert_eq!(
            valid.levels,
            vec![
                vec!["build".to_string(), "lint".to_string()],
                vec!["test".to_string()],
                vec!["gate".to_string()]
            ]
        );
        assert_eq!(valid.contract_hash.len(), 64);
    }

    #[test]
    fn rejects_unknown_dependencies() {
        let err = validate_contract(contract(vec![task("test", &["build"])]))
            .expect_err("unknown dependency fails");
        assert!(err.to_string().contains("unknown task"));
    }

    #[test]
    fn rejects_cycles() {
        let err = validate_contract(contract(vec![task("a", &["b"]), task("b", &["a"])]))
            .expect_err("cycle fails");
        assert!(err.to_string().contains("cycle"));
    }
}
