//! Opt-in local production. The store and executor are trusted, not authenticated.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode, Stdio};

mod contract;
mod discovery;
mod store;
use contract::*;
use store::*;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Context {
    profile: String,
    os: String,
    architecture: String,
    runtime: String,
    tool_images: BTreeMap<String, Option<String>>,
    tool_versions: BTreeMap<String, Option<String>>,
    toolchain_resolution: String,
    undeclared_inputs_excluded: bool,
}

fn resolve_context(target: &Target) -> Result<Context, String> {
    if !matches!(std::env::consts::OS, "linux" | "macos") {
        return Err("unsupported local host: Linux/macOS required".into());
    }
    let runtime = super::shell_runtime()?;
    let paths = std::env::var_os("PATH").unwrap_or_default();
    let mut images = BTreeMap::new();
    let mut versions = BTreeMap::new();
    for tool in &target.profile.tools {
        let image = std::env::split_paths(&paths)
            .map(|p| p.join(tool))
            .find(|p| p.is_file())
            .and_then(|p| super::sha256_file(&p).ok());
        images.insert(tool.clone(), image);
        // Only known version probes; arbitrary declared executables are not run by plan.
        let version = if matches!(tool.as_str(), "rustc" | "cargo" | "cc" | "clang" | "gcc") {
            ProcessCommand::new(tool)
                .arg("--version")
                .env_clear()
                .envs(runtime.environment.iter().cloned())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| super::sha256(&o.stdout))
        } else {
            None
        };
        versions.insert(tool.clone(), version);
    }
    Ok(Context {
        profile: identity("sykli-profile.v1", &target.profile)?,
        os: std::env::consts::OS.into(),
        architecture: std::env::consts::ARCH.into(),
        runtime: runtime.fingerprint,
        tool_images: images,
        tool_versions: versions,
        toolchain_resolution:
            "unknown: tool images do not identify SDKs, wrappers or external dependencies".into(),
        undeclared_inputs_excluded: false,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema: String,
    contract: ProductionContract,
    contract_id: String,
    target: String,
    inputs: BTreeMap<String, Artifact>,
    context: Context,
}
impl Request {
    fn id(&self) -> Result<String, String> {
        identity("sykli-production-request.v1", self)
    }
    fn target(&self) -> &Target {
        &self.contract.targets[&self.target]
    }
    fn load(store: &Store, id: &str) -> Result<Self, String> {
        let request: Self =
            decode(&fs::read(store.production(id)?.join("request.json")).map_err(err)?)?;
        request.contract.validate()?;
        if request.schema != "sykli-production-request.v1"
            || request.id()? != id
            || request.contract.id()? != request.contract_id
            || !request.contract.targets.contains_key(&request.target)
        {
            return Err("production request identity/reference mismatch".into());
        }
        if request.inputs.len() != request.target().inputs.len() {
            return Err("input port mismatch".into());
        }
        for (port, source) in &request.target().inputs {
            let artifact = request.inputs.get(port).ok_or("missing request input")?;
            digest(&artifact.content)?;
            if artifact.ty != source.ty {
                return Err("request input type mismatch".into());
            }
        }
        if request.context.profile != identity("sykli-profile.v1", &request.target().profile)?
            || request.context.undeclared_inputs_excluded
        {
            return Err("invalid local execution context".into());
        }
        Ok(request)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum ResultFact {
    Produced {
        outputs: BTreeMap<String, Artifact>,
    },
    Checked {
        subject: Artifact,
        assertion: String,
        outcome: String,
    },
    ExecutionFailed {
        code: String,
    },
    Interrupted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum Fact {
    Started {
        attempt: String,
        operation: String,
        inputs: BTreeMap<String, Artifact>,
        recipe: String,
        context: String,
        supersedes: Option<String>,
        executor: String,
    },
    Finished {
        attempt: String,
        result: ResultFact,
        observation: Value,
    },
    ContactLost {
        attempt: String,
        reason: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: String,
    production: String,
    sequence: u64,
    previous: Option<String>,
    recorded_at: u64,
    fact: Fact,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    id: String,
    record: Record,
}

#[derive(Clone)]
struct Attempt {
    id: String,
    operation: String,
    inputs: BTreeMap<String, Artifact>,
    result: Option<ResultFact>,
}

struct History {
    records: Vec<Envelope>,
    attempts: BTreeMap<String, Attempt>,
    latest: BTreeMap<String, String>,
}

fn bound(
    binding: &Binding,
    request: &Request,
    accepted: &BTreeMap<String, BTreeMap<String, Artifact>>,
) -> Option<Artifact> {
    match binding {
        Binding::TargetInput { port } => request.inputs.get(port).cloned(),
        Binding::OperationOutput { operation, port } => accepted.get(operation)?.get(port).cloned(),
    }
}

fn inputs_for(
    op: &Operation,
    request: &Request,
    accepted: &BTreeMap<String, BTreeMap<String, Artifact>>,
) -> Option<BTreeMap<String, Artifact>> {
    op.inputs
        .iter()
        .map(|(port, input)| Some((port.clone(), bound(&input.from, request, accepted)?)))
        .collect()
}

impl History {
    fn accepted(
        &self,
        request: &Request,
    ) -> Result<BTreeMap<String, BTreeMap<String, Artifact>>, String> {
        let mut accepted = BTreeMap::new();
        for (name, _) in request.target().selected()? {
            let op = &request.target().operations[&name];
            if let Some(attempt) = self.latest.get(&name).and_then(|id| self.attempts.get(id)) {
                if inputs_for(op, request, &accepted).as_ref() == Some(&attempt.inputs) {
                    if let Some(ResultFact::Produced { outputs }) = &attempt.result {
                        accepted.insert(name, outputs.clone());
                    }
                }
            }
        }
        Ok(accepted)
    }

    fn load(store: &Store, request: &Request) -> Result<Self, String> {
        let production = request.id()?;
        let directory = store.production(&production)?.join("records");
        let mut paths = Vec::new();
        if directory.exists() {
            for entry in fs::read_dir(&directory).map_err(err)? {
                let path = entry.map_err(err)?.path();
                if path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.starts_with(".tmp-"))
                {
                    continue;
                }
                paths.push(path);
            }
        }
        paths.sort();
        let mut history = Self {
            records: vec![],
            attempts: BTreeMap::new(),
            latest: BTreeMap::new(),
        };
        for path in paths {
            let envelope: Envelope = decode(&fs::read(&path).map_err(err)?)?;
            let record = &envelope.record;
            if record.schema != "sykli-production-record.v1"
                || record.production != production
                || record.sequence != history.records.len() as u64 + 1
                || record.previous != history.records.last().map(|r| r.id.clone())
                || envelope.id != identity("sykli-production-record.v1", record)?
                || path.file_name().and_then(|s| s.to_str())
                    != Some(format!("{:020}.json", record.sequence).as_str())
            {
                return Err("record sequence, digest or production reference mismatch".into());
            }
            history.apply(request, &record.fact)?;
            history.records.push(envelope);
        }
        Ok(history)
    }

    fn apply(&mut self, request: &Request, fact: &Fact) -> Result<(), String> {
        match fact {
            Fact::Started {
                attempt,
                operation,
                inputs,
                recipe,
                context,
                supersedes,
                executor,
            } => {
                digest(attempt)?;
                let op = request
                    .target()
                    .operations
                    .get(operation)
                    .ok_or("unknown attempt operation")?;
                if !request
                    .target()
                    .selected()?
                    .iter()
                    .any(|(n, _)| n == operation)
                    || self.attempts.contains_key(attempt)
                    || recipe != &identity("sykli-recipe.v1", op)?
                    || context != &identity("sykli-context.v1", &request.context)?
                    || executor != "local-shell.v1"
                    || supersedes.as_ref() != self.latest.get(operation)
                    || self.attempts.values().any(|a| a.result.is_none())
                    || inputs_for(op, request, &self.accepted(request)?).as_ref() != Some(inputs)
                {
                    return Err("invalid attempt lineage, invocation or input binding".into());
                }
                self.attempts.insert(
                    attempt.clone(),
                    Attempt {
                        id: attempt.clone(),
                        operation: operation.clone(),
                        inputs: inputs.clone(),
                        result: None,
                    },
                );
                self.latest.insert(operation.clone(), attempt.clone());
            }
            Fact::Finished {
                attempt,
                result,
                observation,
            } => {
                let start = self
                    .attempts
                    .get_mut(attempt)
                    .ok_or("terminal record without start")?;
                if let Some(previous) = &start.result {
                    if canonical(previous)? == canonical(result)? {
                        return Err(
                            "duplicate terminal record; replay the identical envelope instead"
                                .into(),
                        );
                    }
                    return Err("conflicting terminal reports: attempt is indeterminate".into());
                }
                let op = &request.target().operations[&start.operation];
                match result {
                    ResultFact::Produced { outputs } => {
                        if op.kind != Kind::Transform || outputs.len() != op.outputs.len() {
                            return Err("invalid transformation result".into());
                        }
                        for (port, output) in &op.outputs {
                            let artifact = outputs.get(port).ok_or("missing result output")?;
                            digest(&artifact.content)?;
                            if artifact.ty != output.ty {
                                return Err("result output type mismatch".into());
                            }
                        }
                    }
                    ResultFact::Checked {
                        subject,
                        assertion,
                        outcome,
                    } if op.kind != Kind::Check
                        || op.assertion.as_ref() != Some(assertion)
                        || op.subject_input.as_ref().and_then(|s| start.inputs.get(s))
                            != Some(subject)
                        || !matches!(outcome.as_str(), "passed" | "failed" | "unknown") =>
                    {
                        return Err("check subject/assertion mismatch".into());
                    }
                    _ => {}
                }
                let successful = matches!(result, ResultFact::Produced { .. })
                    || matches!(result, ResultFact::Checked { outcome, .. } if outcome == "passed");
                if successful
                    && (observation["execution"]["exit_code"] != 0
                        || observation["execution"]["importable"] != true
                        || observation["execution"]["command"] != op.run
                        || observation["execution"]["name"] != start.operation
                        || observation["execution"]["runtime_fingerprint"]
                            != request.context.runtime
                        || observation["execution"]["outcome"] != "passed"
                        || observation["undeclared_inputs_excluded"] != false
                        || observation["input_binding"] != "materialized-snapshot")
                {
                    return Err(
                        "successful result lacks successful bound execution observation".into(),
                    );
                }
                start.result = Some(result.clone());
            }
            Fact::ContactLost { attempt, .. } => {
                if self
                    .attempts
                    .get(attempt)
                    .is_none_or(|a| a.result.is_some())
                {
                    return Err("contact-lost must refer to an unresolved attempt".into());
                }
            }
        }
        Ok(())
    }

    fn append(&mut self, store: &Store, request: &Request, fact: Fact) -> Result<(), String> {
        self.apply(request, &fact)?;
        let record = Record {
            schema: "sykli-production-record.v1".into(),
            production: request.id()?,
            sequence: self.records.len() as u64 + 1,
            previous: self.records.last().map(|r| r.id.clone()),
            recorded_at: now(),
            fact,
        };
        let envelope = Envelope {
            id: identity("sykli-production-record.v1", &record)?,
            record,
        };
        let path = store
            .production(&request.id()?)?
            .join("records")
            .join(format!("{:020}.json", envelope.record.sequence));
        publish(&path, &canonical(&envelope)?)?;
        self.records.push(envelope);
        Ok(())
    }
}

fn view(
    store: &Store,
    request: &Request,
    history: &History,
    controlling: bool,
) -> Result<Value, String> {
    let mut accepted = BTreeMap::new();
    let mut checks = BTreeMap::new();
    let mut work = BTreeMap::new();
    for (name, reasons) in request.target().selected()? {
        let op = &request.target().operations[&name];
        let inputs = inputs_for(op, request, &accepted);
        let attempt = history
            .latest
            .get(&name)
            .and_then(|id| history.attempts.get(id));
        let state = if let Some(attempt) = attempt.filter(|a| inputs.as_ref() == Some(&a.inputs)) {
            match &attempt.result {
                Some(ResultFact::Produced { outputs }) => {
                    accepted.insert(name.clone(), outputs.clone());
                    json!({"kind":"satisfied","attempt":attempt.id})
                }
                Some(ResultFact::Checked { outcome, .. }) if outcome == "passed" => {
                    checks.insert(name.clone(), attempt.id.clone());
                    json!({"kind":"satisfied","attempt":attempt.id})
                }
                None => {
                    json!({"kind":if controlling {"running"} else {"indeterminate"},"attempt":attempt.id})
                }
                Some(ResultFact::Interrupted) => {
                    json!({"kind":"indeterminate","attempt":attempt.id})
                }
                Some(ResultFact::Checked { outcome, .. }) if outcome == "unknown" => {
                    json!({"kind":"indeterminate","attempt":attempt.id})
                }
                _ => json!({"kind":"failed","attempt":attempt.id}),
            }
        } else if let Some(inputs) = inputs {
            let mut missing: Vec<_> = inputs.values().filter_map(|a| store.available(a).err().map(|reason| json!({"kind":"artifact-unavailable","artifact":a,"reason":reason}))).collect();
            for (tool, image) in &request.context.tool_images {
                if image.is_none() {
                    missing.push(json!({"kind":"missing-tool","tool":tool}));
                }
            }
            if missing.is_empty() {
                json!({"kind":"ready"})
            } else {
                json!({"kind":"blocked","reasons":missing})
            }
        } else {
            json!({"kind":"blocked","reasons":[{"kind":"dependency-unsatisfied","operation":name}]})
        };
        work.insert(name, json!({"state":state,"required_because":reasons}));
    }
    let mut products = BTreeMap::new();
    let mut delivery = BTreeMap::new();
    for (port, binding) in &request.target().products {
        if let Some(artifact) = bound(binding, request, &accepted) {
            let availability = match store.available(&artifact) {
                Ok(()) => {
                    json!({"kind":"available","locations":[store.blob_path(&artifact.content)?]})
                }
                Err(reason) => json!({"kind":"unavailable","reason":reason}),
            };
            delivery.insert(
                port.clone(),
                json!({"artifact":artifact,"availability":availability}),
            );
            products.insert(port.clone(), artifact);
        }
    }
    let outstanding: Vec<_> = work
        .iter()
        .filter(|(_, s)| s["state"]["kind"] != "satisfied")
        .map(|(n, _)| n.clone())
        .collect();
    let complete = products.len() == request.target().products.len()
        && request
            .target()
            .required_checks
            .iter()
            .all(|c| checks.contains_key(c))
        && outstanding.is_empty();
    let assessment = if complete {
        json!({"kind":"complete","products":products,"satisfied_checks":checks})
    } else {
        json!({"kind":"incomplete","outstanding":outstanding})
    };
    let delivery_success = complete
        && delivery
            .values()
            .all(|d| d["availability"]["kind"] == "available");
    Ok(json!({
        "schema":"sykli-production-view.v1", "production":request.id()?, "contract":request.contract_id,
        "target":request.target, "inputs":request.inputs, "context":request.context,
        "through_sequence":history.records.len(), "evaluator_version":"local.v1", "policy":"all-selected.v1",
        "evaluation_inputs":{"executor_lease_held":controlling},
        "evaluated_at":history.records.last().map(|r| r.record.recorded_at),
        "work":work,"assessment":assessment,"delivery":delivery,"delivery_success":delivery_success,
        "records":history.records,"trust":"trusted-local-executor-and-store; no authenticity claim"
    }))
}

fn request(store: &Store, path: &Path, target: &str) -> Result<Request, String> {
    let contract = ProductionContract::load(path)?;
    let definition = contract
        .targets
        .get(target)
        .ok_or_else(|| format!("unknown target {target}"))?;
    let root = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut inputs = BTreeMap::new();
    for (port, source) in &definition.inputs {
        inputs.insert(port.clone(), store.capture(root, source)?);
    }
    Ok(Request {
        schema: "sykli-production-request.v1".into(),
        contract_id: contract.id()?,
        context: resolve_context(definition)?,
        contract,
        target: target.into(),
        inputs,
    })
}

pub fn targets(path: &Path) -> Result<Value, String> {
    let contract = ProductionContract::load(path)?;
    Ok(json!({"schema":"sykli-targets.v1","contract":contract.id()?,"targets":contract.targets}))
}

pub fn init(
    path: &Path,
    force: bool,
    package: Option<&str>,
    binary: Option<&str>,
    smoke: Option<&str>,
) -> Result<Value, String> {
    if path
        .parent()
        .is_some_and(|p| !p.as_os_str().is_empty() && p != Path::new("."))
    {
        return Err(
            "typed init writes in the current directory; source paths resolve beside the contract"
                .into(),
        );
    }
    if path.exists() && !force {
        return Err("contract exists; pass --force to overwrite".into());
    }
    let cargo = if Path::new("Cargo.toml").is_file() {
        if smoke.is_none_or(|s| s.trim().is_empty()) {
            return Err(
                "Cargo production needs --smoke 'COMMAND', checking $SYKLI_INPUT_executable".into(),
            );
        }
        Some(discovery::cargo(path, package, binary)?)
    } else {
        if package.is_some() || binary.is_some() {
            return Err("--package and --bin require a Cargo workspace".into());
        }
        if !Path::new("main.rs").is_file() {
            return Err("typed init requires Cargo.toml or standalone main.rs".into());
        }
        None
    };
    let format = match std::env::consts::OS {
        "linux" => "elf",
        "macos" => "macho",
        _ => return Err("unsupported executable host".into()),
    };
    let source = json!({"kind":"source-tree"});
    let executable =
        json!({"kind":"executable","format":format,"architecture":std::env::consts::ARCH});
    let source_input =
        json!({"source":{"expects":source,"from":{"kind":"target-input","port":"source"}}});
    let mut value = json!({"schema":"sykli-production-contract.v1","targets":{"app":{
        "inputs":{"source":{"type":source,"paths":["main.rs"]}},
        "profile":{"kind":"local-shell.v1","tools":["rustc"]},
        "operations":{
            "build":{"kind":"transform","inputs":source_input,"run":"rustc \"$SYKLI_INPUT_source/main.rs\" -o \"$SYKLI_OUTPUT/app\"","reuse":"never",
                "outputs":{"executable":{"type":executable,"collect":"app","validator":"builtin.v1"}}},
            "unit_tests":{"kind":"check","inputs":source_input,"run":"rustc --test \"$SYKLI_INPUT_source/main.rs\" -o \"$SYKLI_OUTPUT/tests\" && \"$SYKLI_OUTPUT/tests\"","reuse":"never","subject_input":"source","assertion":"declared Rust unit tests pass"},
            "smoke_test":{"kind":"check","inputs":{"executable":{"expects":executable,"from":{"kind":"operation-output","operation":"build","port":"executable"}}},
                "run":"\"$SYKLI_INPUT_executable\"","reuse":"never","subject_input":"executable","assertion":"executable exits successfully without arguments"}
        },
        "products":{"app":{"kind":"operation-output","operation":"build","port":"executable"}},"required_checks":["unit_tests","smoke_test"]
    }}});
    if let Some(cargo) = &cargo {
        let target = &mut value["targets"]["app"];
        target["inputs"]["source"]["paths"] = json!(cargo.paths);
        target["profile"]["tools"] = json!(["cargo", "rustc"]);
        target["operations"]["build"]["run"] = cargo.build.clone().into();
        target["operations"]["unit_tests"]["run"] = cargo.test.clone().into();
        target["operations"]["unit_tests"]["assertion"] =
            "selected Cargo package's binary and library unit tests pass".into();
    }
    if let Some(command) = smoke {
        if command.trim().is_empty() {
            return Err("smoke command must not be empty".into());
        }
        value["targets"]["app"]["operations"]["smoke_test"]["run"] = command.into();
        value["targets"]["app"]["operations"]["smoke_test"]["assertion"] =
            "declared smoke command succeeds for the collected executable".into();
    }
    let contract: ProductionContract = serde_json::from_value(value.clone()).map_err(err)?;
    contract.validate()?;
    // Explicit --force authorizes replacing the authoring file, never production records.
    fs::write(path, serde_json::to_vec_pretty(&value).map_err(err)?).map_err(err)?;
    Ok(
        json!({"schema":"sykli-production-init.v1","contract":path,"target":"app","cargo":cargo.map(|c|json!({"package":c.package,"binary":c.binary})),"review":"source paths, recipes and smoke assertion; production pins the reviewed contract"}),
    )
}

pub fn plan(path: &Path, target: &str) -> Result<Value, String> {
    // Planning hashes the explicitly selected bytes without publishing production state.
    let contract = ProductionContract::load(path)?;
    let definition = contract
        .targets
        .get(target)
        .ok_or_else(|| format!("unknown target {target}"))?;
    let root = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let context = resolve_context(definition)?;
    let mut blockers = Vec::new();
    let mut inputs = BTreeMap::new();
    for (port, source) in &definition.inputs {
        match snapshot(root, source, |bytes| Ok(super::sha256(bytes))) {
            Ok(artifact) => {
                inputs.insert(port.clone(), artifact);
            }
            Err(reason) => {
                blockers.push(json!({"kind":"missing-input","port":port,"reason":reason}))
            }
        }
    }
    for (tool, image) in &context.tool_images {
        if image.is_none() {
            blockers.push(json!({"kind":"missing-tool","tool":tool}));
        }
    }
    let production = if inputs.len() == definition.inputs.len() {
        Some(
            Request {
                schema: "sykli-production-request.v1".into(),
                contract: contract.clone(),
                contract_id: contract.id()?,
                target: target.into(),
                inputs: inputs.clone(),
                context: context.clone(),
            }
            .id()?,
        )
    } else {
        None
    };
    Ok(
        json!({"schema":"sykli-production-plan.v1","contract":contract.id()?,"target":target,"production":production,"resolved_inputs":inputs,
        "selected":definition.selected()?.into_iter().map(|(operation,reasons)|json!({"operation":operation,"required_because":reasons})).collect::<Vec<_>>(),
        "inputs":definition.inputs,"products":definition.products,"context":context,"blockers":blockers}),
    )
}

pub fn inspect(store_path: &Path, id: &str) -> Result<Value, String> {
    let store = Store::new(store_path)?;
    let request = Request::load(&store, id)?;
    let lease = Lease::acquire(&store.production(id)?);
    let history = History::load(&store, &request)?;
    view(&store, &request, &history, lease.is_err())
}

pub fn produce(
    store_path: &Path,
    path: &Path,
    target: &str,
    stop_after: Option<&str>,
) -> Result<Value, String> {
    let store = Store::new(store_path)?;
    let request = request(&store, path, target)?;
    let directory = store.production(&request.id()?)?;
    fs::create_dir_all(&directory).map_err(err)?;
    publish(&directory.join("request.json"), &canonical(&request)?)?;
    advance(&store, &request, None, stop_after)
}

pub fn resume(
    store_path: &Path,
    id: &str,
    retry: Option<&str>,
    stop_after: Option<&str>,
) -> Result<Value, String> {
    let store = Store::new(store_path)?;
    let request = Request::load(&store, id)?;
    advance(&store, &request, retry, stop_after)
}

fn advance(
    store: &Store,
    request: &Request,
    retry: Option<&str>,
    stop_after: Option<&str>,
) -> Result<Value, String> {
    let id = request.id()?;
    let directory = store.production(&id)?;
    let lease = Lease::acquire(&directory)?;
    let mut history = History::load(store, request)?;
    let selected = request.target().selected()?;
    for option in [retry, stop_after].into_iter().flatten() {
        if !selected.iter().any(|(n, _)| n == option) {
            return Err(format!("operation {option} is not selected"));
        }
    }
    if identity("sykli-context.v1", &resolve_context(request.target())?)?
        != identity("sykli-context.v1", &request.context)?
    {
        return Err("execution context changed; create a new production with produce".into());
    }
    if request.context.tool_images.values().any(Option::is_none) {
        return view(store, request, &history, false);
    }
    let unresolved = history
        .attempts
        .values()
        .find(|a| a.result.is_none())
        .map(|a| a.id.clone());
    if let Some(attempt) = unresolved {
        if !history.records.iter().any(
            |r| matches!(&r.record.fact, Fact::ContactLost { attempt: a, .. } if a == &attempt),
        ) {
            history.append(store, request, Fact::ContactLost { attempt, reason:"executor lease released without a terminal observation; descendants and outcome unknown; retry refused".into() })?;
        }
        return view(store, request, &history, false);
    }
    // Older local executors recorded shell signals as terminal interruptions.
    // Those records do not establish child termination either.
    if history
        .latest
        .values()
        .any(|id| matches!(history.attempts[id].result, Some(ResultFact::Interrupted)))
    {
        return view(store, request, &history, false);
    }
    if let Some(retry) = retry {
        if !history.latest.contains_key(retry) {
            return Err("retry requires an earlier terminal attempt".into());
        }
    }
    for (name, _) in selected {
        let current = view(store, request, &history, false)?;
        let state = current["work"][&name]["state"]["kind"]
            .as_str()
            .unwrap_or("blocked");
        if state != "ready" && retry != Some(name.as_str()) {
            continue;
        }
        let op = &request.target().operations[&name];
        let Some(inputs) = inputs_for(op, request, &history.accepted(request)?) else {
            continue;
        };
        if inputs.values().any(|a| store.available(a).is_err()) {
            continue;
        }
        let attempt = identity(
            "sykli-attempt.v1",
            &json!({"production":id,"sequence":history.records.len()+1,"time":now(),"pid":std::process::id()}),
        )?;
        history.append(
            store,
            request,
            Fact::Started {
                attempt: attempt.clone(),
                operation: name.clone(),
                inputs,
                recipe: identity("sykli-recipe.v1", op)?,
                context: identity("sykli-context.v1", &request.context)?,
                supersedes: history.latest.get(&name).cloned(),
                executor: "local-shell.v1".into(),
            },
        )?;
        let mut child = ProcessCommand::new(std::env::current_exe().map_err(err)?);
        let fd = lease.inherit(&mut child);
        child
            .args(["__production_attempt", "--store"])
            .arg(&store.0)
            .args([
                "--production",
                &id,
                "--attempt",
                &attempt,
                "--lease-fd",
                &fd.to_string(),
            ]);
        fs::create_dir_all(directory.join("logs")).map_err(err)?;
        let log =
            fs::File::create(directory.join("logs").join(format!("{attempt}.log"))).map_err(err)?;
        child
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(err)?)
            .stderr(log);
        // A started-but-unspawned attempt remains unknown after a crash; never fabricate success.
        match child.spawn() {
            Ok(mut child) => {
                let _ = child.wait();
            }
            Err(error) => {
                history.append(
                    store,
                    request,
                    Fact::Finished {
                        attempt,
                        result: ResultFact::ExecutionFailed {
                            code: "spawn-failed".into(),
                        },
                        observation: json!({"error":error.to_string()}),
                    },
                )?;
            }
        }
        history = History::load(store, request)?;
        if history.attempts.values().any(|a| a.result.is_none())
            || stop_after == Some(name.as_str())
        {
            break;
        }
    }
    view(store, request, &history, false)
}

pub fn executor(
    store_path: &Path,
    production: &str,
    attempt_id: &str,
    fd: i32,
) -> Result<(), String> {
    let store = Store::new(store_path)?;
    let directory = store.production(production)?;
    let _lease = Lease::received(&directory, fd)?;
    let request = Request::load(&store, production)?;
    let mut history = History::load(&store, &request)?;
    let attempt = history
        .attempts
        .get(attempt_id)
        .ok_or("unknown executor attempt")?
        .clone();
    if attempt.result.is_some() {
        return Err("executor attempt already finished".into());
    }
    let outcome = execute(&store, &directory, &request, &attempt);
    let (result, observation) = match outcome {
        Ok(Some(result)) => result,
        Ok(None) => {
            return history.append(
                &store,
                &request,
                Fact::ContactLost {
                    attempt: attempt.id,
                    reason: "executor could not establish command termination".into(),
                },
            );
        }
        Err(error) => (
            ResultFact::ExecutionFailed {
                code: "preparation-or-collection-failed".into(),
            },
            json!({"error":error}),
        ),
    };
    history.append(
        &store,
        &request,
        Fact::Finished {
            attempt: attempt.id,
            result,
            observation,
        },
    )
}

fn execute(
    store: &Store,
    directory: &Path,
    request: &Request,
    attempt: &Attempt,
) -> Result<Option<(ResultFact, Value)>, String> {
    if identity("sykli-context.v1", &resolve_context(request.target())?)?
        != identity("sykli-context.v1", &request.context)?
    {
        return Err("resolved context changed before execution".into());
    }
    let root = directory.join("attempts").join(&attempt.id);
    fs::create_dir_all(root.parent().unwrap()).map_err(err)?;
    fs::create_dir(&root).map_err(|e| format!("fresh attempt directory required: {e}"))?;
    for sub in ["inputs", "outputs", "work"] {
        fs::create_dir(root.join(sub)).map_err(err)?;
    }
    let mut env = BTreeMap::new();
    for (port, artifact) in &attempt.inputs {
        let destination = root.join("inputs").join(port);
        store.materialize(artifact, &destination)?;
        env.insert(
            format!("SYKLI_INPUT_{port}"),
            destination
                .to_str()
                .ok_or("non-UTF8 input location")?
                .into(),
        );
    }
    env.insert(
        "SYKLI_OUTPUT".into(),
        root.join("outputs")
            .to_str()
            .ok_or("non-UTF8 output location")?
            .into(),
    );
    let op = &request.target().operations[&attempt.operation];
    let task = super::Task {
        name: attempt.operation.clone(),
        run: op.run.clone(),
        workdir: Some(root.join("work")),
        env,
        after: vec![],
        inputs: vec![],
        outputs: vec![],
        runtime: None,
    };
    let execution = super::run_task(
        &task,
        &super::shell_runtime()?,
        true,
        super::MAX_CAPTURE_BYTES,
    );
    // Waiting for the shell does not establish that its foreground children stopped.
    if execution.class == Some("runtime_error")
        || (execution.exit_code.is_none() && execution.class == Some("command_failed"))
    {
        return Ok(None);
    }
    let mut observation = json!({"execution":execution,"input_binding":"materialized-snapshot","undeclared_inputs_excluded":false});
    let result = if !execution.importable || execution.outcome == super::Outcome::Errored {
        ResultFact::ExecutionFailed {
            code: "execution-unobserved-or-capture-incomplete".into(),
        }
    } else if op.kind == Kind::Check {
        ResultFact::Checked {
            subject: attempt.inputs[op.subject_input.as_ref().unwrap()].clone(),
            assertion: op.assertion.clone().unwrap(),
            outcome: if execution.outcome == super::Outcome::Passed {
                "passed"
            } else {
                "failed"
            }
            .into(),
        }
    } else if execution.outcome != super::Outcome::Passed {
        ResultFact::ExecutionFailed {
            code: "command-failed".into(),
        }
    } else {
        let outputs = op
            .outputs
            .iter()
            .map(|(port, output)| Ok((port.clone(), store.collect(&root.join("outputs"), output)?)))
            .collect::<Result<BTreeMap<_, _>, String>>();
        match outputs {
            Ok(outputs) => ResultFact::Produced { outputs },
            Err(error) => {
                observation["collection_error"] = error.into();
                ResultFact::ExecutionFailed {
                    code: "output-validation-failed".into(),
                }
            }
        }
    };
    Ok(Some((result, observation)))
}

fn blocker_summary(reason: &Value) -> String {
    if let Some(detail) = reason["reason"].as_str() {
        return detail.into();
    }
    let kind = reason["kind"].as_str().unwrap_or("blocked");
    let subject = reason["tool"]
        .as_str()
        .or(reason["port"].as_str())
        .or(reason["operation"].as_str())
        .unwrap_or("");
    format!("{kind}: {subject}")
}

// Human presentation uses the same evaluated data as the JSON interface.
fn human_summary(value: &Value) -> Option<String> {
    let mut lines = Vec::new();
    match value["schema"].as_str()? {
        "sykli-targets.v1" => {
            lines.push("Available targets".into());
            for (name, target) in value["targets"].as_object()? {
                let products = target["products"]
                    .as_object()?
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>();
                let tools = target["profile"]["tools"]
                    .as_array()?
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>();
                lines.push(format!("  {name}: produces {}", products.join(", ")));
                lines.push(format!("    Required tools: {}", tools.join(", ")));
            }
        }
        "sykli-production-plan.v1" => {
            lines.push(format!("Plan: {}", value["target"].as_str()?));
            for operation in value["selected"].as_array()? {
                let reasons = operation["required_because"]
                    .as_array()?
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>();
                lines.push(format!(
                    "  {} ({})",
                    operation["operation"].as_str()?,
                    reasons.join(", ")
                ));
            }
            for blocker in value["blockers"].as_array()? {
                lines.push(format!("  Blocked: {}", blocker_summary(blocker)));
            }
        }
        "sykli-production-view.v1" => {
            let state = if value["delivery_success"] == true {
                "complete, artifact available"
            } else if value["assessment"]["kind"] == "complete" {
                "checks complete, delivery unavailable"
            } else {
                "incomplete"
            };
            lines.push(format!("{}: {state}", value["target"].as_str()?));
            for (name, work) in value["work"].as_object()? {
                let state = &work["state"];
                let kind = state["kind"].as_str()?;
                lines.push(format!("  {name}: {kind}"));
                if let Some(reasons) = state["reasons"].as_array() {
                    for reason in reasons {
                        lines.push(format!("    {}", blocker_summary(reason)));
                    }
                }
                if kind == "failed" {
                    if let Some(fact) = value["records"]
                        .as_array()?
                        .iter()
                        .rev()
                        .map(|record| &record["record"]["fact"])
                        .find(|fact| {
                            fact["kind"] == "finished" && fact["attempt"] == state["attempt"]
                        })
                    {
                        let observation = &fact["observation"];
                        let reason = observation["collection_error"]
                            .as_str()
                            .or(observation["error"].as_str())
                            .or(observation["execution"]["error"].as_str())
                            .or(fact["result"]["code"].as_str());
                        if let Some(reason) = reason {
                            lines.push(format!("    {reason}"));
                        }
                        for stream in ["stdout", "stderr"] {
                            if let Some(capture) = observation["execution"][stream]
                                .as_str()
                                .filter(|capture| !capture.is_empty())
                            {
                                lines.push(format!("    {stream} (last 20 lines):"));
                                let tail = capture.lines().rev().take(20).collect::<Vec<_>>();
                                for line in tail.into_iter().rev() {
                                    lines.push(format!(
                                        "      {}",
                                        line.chars().take(240).collect::<String>()
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            for (name, product) in value["delivery"].as_object()? {
                let availability = &product["availability"];
                lines.push(format!(
                    "\nArtifact {name}: {}",
                    availability["kind"].as_str()?
                ));
                lines.push(format!(
                    "  SHA-256: {}",
                    product["artifact"]["content"].as_str()?
                ));
                if let Some(locations) = availability["locations"].as_array() {
                    for location in locations {
                        lines.push(format!("  Path: {}", location.as_str()?));
                    }
                }
                if let Some(reason) = availability["reason"].as_str() {
                    lines.push(format!("  {reason}"));
                }
            }
        }
        _ => return None,
    }
    if let Some(production) = value["production"].as_str() {
        lines.push(format!("\nProduction: {production}"));
    }
    let inputs = value.get("resolved_inputs").or_else(|| value.get("inputs"));
    if let Some(inputs) = inputs.and_then(Value::as_object) {
        for (port, artifact) in inputs {
            if let Some(content) = artifact["content"].as_str() {
                lines.push(format!("Input {port}: {content}"));
            }
        }
    }
    lines.push("\nUse --json for full structured details.".into());
    Some(lines.join("\n"))
}

pub fn report(
    result: Result<Value, String>,
    json_output: bool,
    delivery_required: bool,
) -> ExitCode {
    match result {
        Ok(value) => {
            let success = !delivery_required || value["delivery_success"] == true;
            if json_output {
                println!("{value}");
            } else {
                println!(
                    "{}",
                    human_summary(&value)
                        .unwrap_or_else(|| serde_json::to_string_pretty(&value).unwrap())
                );
            }
            ExitCode::from(if success { 0 } else { 1 })
        }
        Err(error) => {
            if json_output {
                println!(
                    "{}",
                    json!({"schema":"sykli-production-error.v1","error":error})
                );
            } else {
                eprintln!("error: {error}");
            }
            ExitCode::from(2)
        }
    }
}
