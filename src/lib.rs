//! Rust emitter for `sykli-contract.v1` execution graphs.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub schema: String,
    pub tasks: Vec<Task>,
}

#[derive(Deserialize, Serialize)]
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
    pub inputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// Environment variables passed through from the invoking environment.
    /// Values reach the command; only their digests reach the receipt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inherit: Vec<String>,
}

pub struct Pipeline {
    contract: Contract,
}

impl Pipeline {
    pub fn new() -> Self {
        Self {
            contract: Contract {
                schema: "sykli-contract.v1".into(),
                tasks: Vec::new(),
            },
        }
    }

    pub fn task(&mut self, name: impl Into<String>) -> TaskBuilder<'_> {
        self.contract.tasks.push(Task {
            name: name.into(),
            run: String::new(),
            workdir: None,
            env: BTreeMap::new(),
            after: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            runtime: None,
            inherit: Vec::new(),
        });
        TaskBuilder {
            task: self.contract.tasks.last_mut().unwrap(),
        }
    }

    pub fn emit(self) {
        if !std::env::args().any(|argument| argument == "--emit") {
            eprintln!("this binary is a Sykli emitter; invoke it through `sykli run`");
            std::process::exit(2);
        }
        if serde_json::to_writer(std::io::stdout().lock(), &self.contract).is_err() {
            eprintln!("could not write the Sykli contract to stdout");
            std::process::exit(2);
        }
        println!();
    }
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}

pub struct TaskBuilder<'a> {
    task: &'a mut Task,
}

impl TaskBuilder<'_> {
    pub fn run(self, command: impl Into<String>) -> Self {
        self.task.run = command.into();
        self
    }

    pub fn workdir(self, path: impl Into<PathBuf>) -> Self {
        self.task.workdir = Some(path.into());
        self
    }

    pub fn env(self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.task.env.insert(name.into(), value.into());
        self
    }

    pub fn after(self, tasks: &[&str]) -> Self {
        self.task.after = tasks.iter().map(|task| (*task).into()).collect();
        self
    }

    pub fn inputs(self, paths: &[&str]) -> Self {
        self.task.inputs = paths.iter().map(|path| (*path).into()).collect();
        self
    }

    pub fn outputs(self, paths: &[&str]) -> Self {
        self.task.outputs = paths.iter().map(|path| (*path).into()).collect();
        self
    }

    pub fn runtime(self, runtime: impl Into<String>) -> Self {
        self.task.runtime = Some(runtime.into());
        self
    }

    /// Pass these environment variables through from the invoking environment.
    pub fn inherit(self, names: &[&str]) -> Self {
        self.task.inherit = names.iter().map(|name| (*name).into()).collect();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_v1_contract() {
        let mut pipeline = Pipeline::new();
        let _ = pipeline
            .task("test")
            .run("cargo test")
            .inputs(&["src/lib.rs"]);
        let value = serde_json::to_value(pipeline.contract).unwrap();
        assert_eq!(value["schema"], "sykli-contract.v1");
        assert_eq!(value["tasks"][0]["name"], "test");
        assert!(value["tasks"][0].get("outputs").is_none());
    }
}
