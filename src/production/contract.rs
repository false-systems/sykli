use super::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ArtifactType {
    SourceTree,
    File {
        media_type: String,
    },
    Directory,
    Executable {
        format: String,
        architecture: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub content: String,
    #[serde(rename = "type")]
    pub ty: ArtifactType,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Binding {
    TargetInput { port: String },
    OperationOutput { operation: String, port: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub expects: ArtifactType,
    pub from: Binding,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    #[serde(rename = "type")]
    pub ty: ArtifactType,
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    #[serde(rename = "type")]
    pub ty: ArtifactType,
    pub collect: String,
    pub validator: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Transform,
    Check,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub kind: Kind,
    pub inputs: BTreeMap<String, Input>,
    pub run: String,
    pub reuse: String,
    #[serde(default)]
    pub outputs: BTreeMap<String, Output>,
    #[serde(default)]
    pub subject_input: Option<String>,
    #[serde(default)]
    pub assertion: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub kind: String,
    pub tools: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub inputs: BTreeMap<String, Source>,
    pub profile: Profile,
    pub operations: BTreeMap<String, Operation>,
    pub products: BTreeMap<String, Binding>,
    pub required_checks: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionContract {
    pub schema: String,
    pub targets: BTreeMap<String, Target>,
}

pub use crate::canonical::{canonical, decode, identity};

pub fn name(value: &str) -> Result<(), String> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
    {
        return Err(format!(
            "invalid identifier {value:?}; use letters, digits or underscore"
        ));
    }
    Ok(())
}

// Target/product labels are not used as environment variable names or paths.
fn label(value: &str) -> Result<(), String> {
    if !value.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
    {
        return Err(format!(
            "invalid target/product name {value:?}; start with a letter, digit or underscore and use letters, digits, underscores, hyphens or dots"
        ));
    }
    Ok(())
}

pub fn relative(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.contains('\\')
        || value
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
        || Path::new(value).is_absolute()
    {
        return Err(format!("not a canonical relative path: {value:?}"));
    }
    Ok(())
}

pub fn source_path(path: &str) -> Result<(), String> {
    relative(path)?;
    if matches!(path, ".cargo/config" | ".cargo/config.toml") {
        return Ok(());
    }
    if path
        .split('/')
        .any(|p| p.starts_with('.') || matches!(p, "node_modules" | "target" | "vendor"))
    {
        return Err(format!("unsupported hidden/generated source path {path}"));
    }
    Ok(())
}

fn supported(ty: &ArtifactType) -> Result<(), String> {
    match ty {
        ArtifactType::Executable {
            format,
            architecture,
        } if !matches!(format.as_str(), "elf" | "macho")
            || !matches!(architecture.as_str(), "x86_64" | "aarch64") =>
        {
            Err(format!("unsupported executable type {ty:?}"))
        }
        ArtifactType::File { media_type } if media_type != "application/octet-stream" => {
            Err("only application/octet-stream file validation is supported".into())
        }
        _ => Ok(()),
    }
}

impl ProductionContract {
    pub fn load(path: &Path) -> Result<Self, String> {
        let result: Self = decode(&fs::read(path).map_err(err)?)?;
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != "sykli-production-contract.v1" || self.targets.is_empty() {
            return Err("expected sykli-production-contract.v1 with at least one target".into());
        }
        for (id, target) in &self.targets {
            label(id)?;
            target.validate()?;
        }
        Ok(())
    }

    pub fn id(&self) -> Result<String, String> {
        identity("sykli-production-contract.v1", self)
    }
}

impl Target {
    pub fn binding_type(&self, binding: &Binding) -> Result<&ArtifactType, String> {
        match binding {
            Binding::TargetInput { port } => self.inputs.get(port).map(|i| &i.ty),
            Binding::OperationOutput { operation, port } => self
                .operations
                .get(operation)
                .and_then(|o| o.outputs.get(port))
                .map(|o| &o.ty),
        }
        .ok_or_else(|| format!("unresolved binding {binding:?}"))
    }

    pub fn graph(&self) -> super::super::Contract {
        super::super::Contract {
            schema: "sykli-contract.v1".into(),
            tasks: self
                .operations
                .iter()
                .map(|(id, op)| super::super::Task {
                    name: id.clone(),
                    run: op.run.clone(),
                    workdir: None,
                    env: BTreeMap::new(),
                    after: op
                        .inputs
                        .values()
                        .filter_map(|i| match &i.from {
                            Binding::OperationOutput { operation, .. } => Some(operation.clone()),
                            _ => None,
                        })
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect(),
                    inputs: vec![],
                    outputs: vec![],
                    runtime: None,
                    inherit: vec![],
                })
                .collect(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.profile.kind != "local-shell.v1" {
            return Err(format!(
                "unsupported-execution-profile: {}",
                self.profile.kind
            ));
        }
        let mut tools = BTreeSet::new();
        for tool in &self.profile.tools {
            name(tool)?;
            if !tools.insert(tool) {
                return Err(format!("duplicate tool {tool}"));
            }
        }
        for (id, source) in &self.inputs {
            name(id)?;
            if source.ty != ArtifactType::SourceTree || source.paths.is_empty() {
                return Err(
                    "target inputs currently require source-tree with explicit file paths".into(),
                );
            }
            let mut paths = BTreeSet::new();
            for path in &source.paths {
                source_path(path)?;
                if !paths.insert(path) {
                    return Err(format!("duplicate source path {path}"));
                }
            }
        }
        for (id, op) in &self.operations {
            name(id)?;
            if op.reuse != "never" {
                return Err("typed cross-production reuse is unsupported; use never".into());
            }
            for (port, input) in &op.inputs {
                name(port)?;
                supported(&input.expects)?;
                if &input.expects != self.binding_type(&input.from)? {
                    return Err(format!("type mismatch at {id}.{port}"));
                }
            }
            for (port, output) in &op.outputs {
                name(port)?;
                supported(&output.ty)?;
                relative(&output.collect)?;
                if output.validator != "builtin.v1" || output.ty == ArtifactType::SourceTree {
                    return Err(format!("unsupported output validator at {id}.{port}"));
                }
            }
            match op.kind {
                Kind::Transform
                    if op.outputs.is_empty()
                        || op.subject_input.is_some()
                        || op.assertion.is_some() =>
                {
                    return Err(format!(
                        "transform {id} requires outputs and no check fields"
                    ));
                }
                Kind::Check
                    if !op.outputs.is_empty()
                        || !op
                            .subject_input
                            .as_ref()
                            .is_some_and(|s| op.inputs.contains_key(s))
                        || op.assertion.as_ref().is_none_or(|s| s.trim().is_empty()) =>
                {
                    return Err(format!("invalid check subject/assertion at {id}"));
                }
                _ => {}
            }
        }
        if self.products.is_empty() {
            return Err("target has no product".into());
        }
        for (port, binding) in &self.products {
            label(port)?;
            self.binding_type(binding)?;
        }
        let mut checks = BTreeSet::new();
        for check in &self.required_checks {
            if !checks.insert(check)
                || !self
                    .operations
                    .get(check)
                    .is_some_and(|o| o.kind == Kind::Check)
            {
                return Err(format!("invalid required check {check}"));
            }
        }
        super::super::validate(&self.graph())?;
        Ok(())
    }

    pub fn selected(&self) -> Result<Vec<(String, BTreeSet<String>)>, String> {
        let mut reasons: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (port, binding) in &self.products {
            if let Binding::OperationOutput { operation, .. } = binding {
                reasons
                    .entry(operation.clone())
                    .or_default()
                    .insert(format!("product:{port}"));
            }
        }
        for check in &self.required_checks {
            reasons
                .entry(check.clone())
                .or_default()
                .insert("required-check".into());
        }
        let graph = self.graph();
        let levels = super::super::validate(&graph)?;
        for &i in levels.iter().rev().flatten() {
            let op = &graph.tasks[i];
            if reasons.contains_key(&op.name) {
                for dependency in &op.after {
                    reasons
                        .entry(dependency.clone())
                        .or_default()
                        .insert(format!("input-for:{}", op.name));
                }
            }
        }
        Ok(levels
            .iter()
            .flatten()
            .filter_map(|&i| {
                let name = &graph.tasks[i].name;
                reasons.remove(name).map(|r| (name.clone(), r))
            })
            .collect())
    }
}
