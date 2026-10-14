//! Delivery (Unix): a production's artifact applied to a declared target,
//! journaled as one operation, and judged only from what an observer of the
//! target reports, one dimension at a time.
//!
//! Sykli knows nothing about any kind of target. A `sykli-delivery-contract.v1`
//! names, for each target, the product it delivers and four commands the
//! caller owns: `identity` (what the target will report for the artifact),
//! `apply` (the side effect), `observe` (prints a `sykli-observation.v1` of
//! the target) and an optional `smoke`. Sykli runs them under the run rules
//! (a cleared environment, declared `env`, `inherit` whose values only reach
//! the command), journals what happened, and never takes a command's word
//! for more than it says: an apply that exits 0 is acknowledged, not
//! deployed.
//!
//! A delivery lives in `<git-common-dir>/sykli/deliveries/<id>/`, beside the
//! clone's runs, so every worktree of the clone sees every delivery to a
//! target:
//!
//! * `records/<sequence>.json`: the journal (`sykli-delivery-record.v1`),
//!   hash chained, each record synced before it is acknowledged.
//! * `outputs/<sha256>.json`: each apply's output record, cited by its end.
//! * `lease`: the coordinator's; `attempt-leases/<attempt>`: held by an
//!   apply's shell and everything it starts, so an apply that outlives a
//!   killed coordinator is seen as running.
//! * `scratch/`: the artifact materialized for an apply, removed after it.
//!
//! The order of facts is the point (invariants A, B, H):
//! `requested` is durable before anything is dispatched; `dispatched` is
//! durable before the apply's shell is spawned; then exactly one end:
//! `acknowledged` (exit 0), `failed` (it reported a failure, or could not
//! start), `interrupted` (sykli stopped it) or `lost` (a later coordinator
//! found it unfinished). Observations (`observed`) are recorded with their
//! time and the digest of the observer's output; any number, any time.
//! When an apply's end is unknown, recovery never runs it again: `resume`
//! observes the target and records `reconciled`, effect `observed` only when
//! the observation shows this delivery's artifact as the target's desired
//! state, `unknown` otherwise. Only `resume --reapply` applies again, as a
//! new attempt that supersedes the last; nothing is rewritten.
//!
//! Every coordinator holds a generation, as a run's does: a fact from a
//! coordinator a newer generation took over is refused.
//!
//! Retention: at most `SYKLI_DELIVERY_BUDGET` deliveries (default 100; 0
//! keeps every one) per clone; when one is requested beyond that, the oldest
//! settled ones are removed (`prunable`): never one whose coordinator or apply
//! is alive, whose apply's effect is unknown, or the newest delivery to any
//! target. A delivery holds at most `MAX_RECORDS` records; beyond that
//! nothing more is observed into it.
use super::{Artifact, Store, label, relative};
use crate::canonical::{Durability, NONCE, canonical, decode, digest, identity, now, publish_with};
use crate::journal::{self, Envelope, Lease};
use crate::{Journaled, Launched, Progress, ShellRuntime, TaskReceipt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use sykli::Task;

const CONTRACT: &str = "sykli-delivery-contract.v1";
const RECORD: &str = "sykli-delivery-record.v1";
const OBSERVATION: &str = "sykli-observation.v1";
const STATUS: &str = "sykli-delivery-status.v1";
const EXECUTOR: &str = "local-shell.v1";
/// What a delivery's status rests on.
const TRUST: &str = "trusted-local-coordinator-and-journal; each observation is what the \
     declared observer reported, never independently verified; no authenticity claim";
const DEFAULT_BUDGET: usize = 100;
/// Records one delivery's journal may hold; observing stops there.
const MAX_RECORDS: usize = 2000;
/// Each stream of an apply kept in its output record.
const APPLY_CAPTURE: usize = 64 * 1024;
/// The most an observer may print; a longer report is refused.
const OBSERVATION_LIMIT: usize = 64 * 1024;
/// The start of a command's stderr kept in a record, for a person.
const KEPT_STDERR: usize = 4 * 1024;
const MAX_SETTLE_SECONDS: u64 = 3600;
/// How far ahead of now an observation may be dated and still be taken as
/// now. Observations are dated on this machine's clock, so more than this
/// means the clock stepped back: such an observation is never fresh.
const CLOCK_SKEW_MS: u64 = 5_000;
const OBSERVE_INTERVAL: Duration = Duration::from_secs(2);
const CONTROLLER_STATES: [&str; 5] = ["pending", "progressing", "complete", "failed", "unknown"];

/// The labels a delivery's overall line can carry; nothing else is said.
pub(crate) const VERIFIED_HEALTHY: &str = "VERIFIED HEALTHY";
const NOT_VERIFIED_HEALTHY: &str = "NOT VERIFIED HEALTHY";
const DEGRADED: &str = "DEGRADED";
const UNKNOWN: &str = "UNKNOWN";
const STALE: &str = "STALE";

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

// ---------------------------------------------------------------------------
// The contract.

/// `sykli-delivery-contract.v1`: how the products of one production contract
/// reach their targets. Kept apart from the production contract, so changing
/// how an artifact is delivered never changes the production that built it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Contract {
    schema: String,
    /// The `sykli-production-contract.v1` whose products are delivered, as a
    /// path relative to this contract.
    production: String,
    targets: BTreeMap<String, Target>,
}

/// One target: the product it receives and the caller's commands.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    artifact: Product,
    identity: Identity,
    apply: Step,
    observe: Step,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    smoke: Option<Step>,
    /// How old an observation may be and still support a present-tense
    /// statement about the target.
    freshness_seconds: u64,
    /// How long `deliver` (and `resume`) keep observing after the apply until
    /// the target settles; 0 observes once.
    #[serde(default, skip_serializing_if = "is_zero")]
    settle_seconds: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Product {
    /// The production target that makes it.
    target: String,
    /// Its product port.
    product: String,
}

/// How the identity the target reports for the artifact is known before it
/// is delivered: the artifact's own SHA-256, or what a command prints given
/// the artifact (an image manifest digest inside an image archive, say).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum Identity {
    Content,
    Command {
        run: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        inherit: Vec<String>,
    },
}

/// A command the caller owns, run as a run's task is: `sh -c`, a cleared
/// environment plus PATH, HOME and TMPDIR, the declared `env`, and the
/// `inherit`ed variables, whose values reach only this command and whose
/// digests only reach the journal.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Step {
    run: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    inherit: Vec<String>,
}

impl Contract {
    fn load(path: &Path) -> Result<Self, String> {
        let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let contract: Self = decode(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        contract.validate()?;
        Ok(contract)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema != CONTRACT || self.targets.is_empty() {
            return Err(format!("expected {CONTRACT} with at least one target"));
        }
        relative(&self.production)?;
        for (name, target) in &self.targets {
            label(name)?;
            target.validate(name)?;
        }
        Ok(())
    }
}

impl Target {
    fn validate(&self, name: &str) -> Result<(), String> {
        label(&self.artifact.target)?;
        label(&self.artifact.product)?;
        self.apply.validate(name, "apply")?;
        self.observe.validate(name, "observe")?;
        if let Some(smoke) = &self.smoke {
            smoke.validate(name, "smoke")?;
        }
        if let Some(step) = self.identity_step() {
            step.validate(name, "identity")?;
        }
        if self.freshness_seconds == 0 {
            return Err(format!(
                "target {name}: freshness_seconds must be positive; an observation is never \
                 fresh forever"
            ));
        }
        if self.settle_seconds > MAX_SETTLE_SECONDS {
            return Err(format!(
                "target {name}: settle_seconds is at most {MAX_SETTLE_SECONDS}"
            ));
        }
        Ok(())
    }

    fn identity_step(&self) -> Option<Step> {
        match &self.identity {
            Identity::Content => None,
            Identity::Command { run, env, inherit } => Some(Step {
                run: run.clone(),
                env: env.clone(),
                inherit: inherit.clone(),
            }),
        }
    }
}

impl Step {
    fn validate(&self, target: &str, which: &str) -> Result<(), String> {
        let at = format!("target {target}, {which}");
        if self.run.trim().is_empty() {
            return Err(format!("{at}: an empty command"));
        }
        for (name, value) in &self.env {
            variable(name).map_err(|e| format!("{at}: {e}"))?;
            if value.contains("${") {
                return Err(format!(
                    "{at}: env {name} names a placeholder; a delivery's env values are literal"
                ));
            }
        }
        let mut inherited = BTreeSet::new();
        for name in &self.inherit {
            variable(name).map_err(|e| format!("{at}: {e}"))?;
            if !inherited.insert(name) || self.env.contains_key(name) {
                return Err(format!("{at}: {name} is declared twice"));
            }
        }
        Ok(())
    }
}

/// An environment variable a contract may set or pass: never one of the
/// `SYKLI_` variables sykli gives every delivery command.
fn variable(name: &str) -> Result<(), String> {
    let valid = name
        .bytes()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_');
    if !valid {
        return Err(format!("{name:?} is not an environment variable name"));
    }
    if name.starts_with("SYKLI_") {
        return Err(format!(
            "{name} is reserved: sykli sets the SYKLI_ variables"
        ));
    }
    Ok(())
}

/// An identity as sykli compares them: `algorithm:value`, no whitespace.
fn identity_text(value: &str) -> Result<(), String> {
    let valid = value.len() <= 512
        && value.split_once(':').is_some_and(|(algorithm, rest)| {
            !algorithm.is_empty()
                && !rest.is_empty()
                && algorithm
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
        && !value
            .bytes()
            .any(|c| c.is_ascii_whitespace() || c.is_ascii_control());
    if !valid {
        return Err(format!(
            "{value:?} is not an artifact identity (`algorithm:value`, e.g. `sha256:…`)"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// What an observer reports.

/// `sykli-observation.v1`, printed by a target's `observe` command: what the
/// target says it is asked to run (desired), what it runs (observed), and
/// how healthy that is. Integers only; `detail` is the observer's own and is
/// kept, never judged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    schema: String,
    /// The observer reached the target's controller.
    reachable: bool,
    /// The target object exists (required when reachable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    present: Option<bool>,
    /// The target object's identity; a recreated object has another, and
    /// its revisions start again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    uid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    desired: Option<Desired>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed: Option<Running>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    readiness: Option<Readiness>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    checks: Vec<HealthCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    controller: Option<Controller>,
    /// Changes whenever anything about the target changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<Value>,
}

/// What the target's controller is asked to run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Desired {
    #[serde(default)]
    revision: Option<String>,
    /// The artifact identity the desired state names.
    #[serde(default)]
    artifact: Option<String>,
    /// The operation the desired state names (an apply passes
    /// `$SYKLI_DELIVERY` to the target), if the target keeps one.
    #[serde(default)]
    operation: Option<String>,
    /// Who last wrote the desired artifact, as the target says.
    #[serde(default)]
    writer: Option<String>,
}

/// What the target runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Running {
    #[serde(default)]
    revision: Option<String>,
    /// The identities of what actually runs, by immutable digest.
    #[serde(default)]
    artifacts: Vec<String>,
    /// The controller has acted on the desired state and finished doing so.
    converged: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Readiness {
    ready: u64,
    wanted: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HealthCheck {
    name: String,
    passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Controller {
    /// pending, progressing, complete, failed or unknown.
    state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

impl Observation {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let observation: Self = decode(bytes)?;
        observation.validate()?;
        Ok(observation)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema != OBSERVATION {
            return Err(format!("schema {:?}, expected {OBSERVATION}", self.schema));
        }
        if !self.reachable {
            if self.present.is_some() || self.desired.is_some() || self.observed.is_some() {
                return Err("an unreachable target reports no state".into());
            }
            return Ok(());
        }
        let Some(present) = self.present else {
            return Err("a reachable target says whether it is present".into());
        };
        if present
            && (self.desired.is_none()
                || self.observed.is_none()
                || self.readiness.is_none()
                || self.controller.is_none())
        {
            return Err(
                "a present target reports desired, observed, readiness and controller".into(),
            );
        }
        if let Some(desired) = &self.desired {
            if let Some(artifact) = &desired.artifact {
                identity_text(artifact)?;
            }
        }
        if let Some(running) = &self.observed {
            for artifact in &running.artifacts {
                identity_text(artifact)?;
            }
        }
        if let Some(controller) = &self.controller {
            if !CONTROLLER_STATES.contains(&controller.state.as_str()) {
                return Err(format!(
                    "controller state {:?} is not one of {}",
                    controller.state,
                    CONTROLLER_STATES.join(", ")
                ));
            }
        }
        let mut names = BTreeSet::new();
        for check in &self.checks {
            if check.name.trim().is_empty() || !names.insert(&check.name) {
                return Err("every check has a name of its own".into());
            }
        }
        Ok(())
    }

    /// Whether this observation shows `delivery`'s effect: the target's
    /// desired state names its artifact and names this delivery as its
    /// operation. The artifact alone shows nothing of who put it there.
    fn shows(&self, delivery: &str, intended: &str) -> bool {
        self.desires(intended)
            && self
                .desired
                .as_ref()
                .is_some_and(|desired| desired.operation.as_deref() == Some(delivery))
    }

    /// Whether the target's desired state names `intended`, whoever asked.
    fn desires(&self, intended: &str) -> bool {
        self.reachable
            && self.present == Some(true)
            && self
                .desired
                .as_ref()
                .is_some_and(|desired| desired.artifact.as_deref() == Some(intended))
    }
}

// ---------------------------------------------------------------------------
// The request and the journal.

/// The product a delivery delivers.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Delivered {
    /// The production that made it, in `store`.
    production: String,
    store: String,
    target: String,
    product: String,
    artifact: Artifact,
}

/// How the intended identity was established.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum Identified {
    Content,
    Command { stdout_digest: String },
}

/// What a delivery was asked to do; its identity is the delivery id.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    /// The contract as it was read; later commands run from this, not the file.
    contract: Contract,
    contract_path: String,
    contract_digest: String,
    target: String,
    /// Where the commands run: the contract's directory.
    workdir: String,
    common_dir: String,
    artifact: Delivered,
    /// The identity the target should report for the artifact.
    intended: String,
    identified_by: Identified,
    /// The earlier delivery whose artifact this one delivers again.
    rollback_of: Option<String>,
    /// The newest delivery to this target in this clone when this one was
    /// requested.
    previous: Option<String>,
    /// Its place in the target's sequence (`next_in_sequence`): requests and
    /// dispatches to the target in the order they happened in this clone,
    /// whatever the clock did.
    target_sequence: u64,
    /// Earlier deliveries to this target whose last apply's effect was
    /// unknown, and could not be reconciled because no observation reached
    /// the target, that this one was asked to apply over
    /// (`deliver --reapply-over`).
    reapply_over: Vec<Override>,
    runtime: String,
    requested_at_ms: u64,
    executor: String,
    detached: bool,
    nonce: String,
}

impl Request {
    fn definition(&self) -> Result<&Target, String> {
        self.contract
            .targets
            .get(&self.target)
            .ok_or_else(|| format!("the recorded contract has no target {}", self.target))
    }
}

/// What `deliver --reapply-over` applied over: an earlier delivery's apply
/// whose effect is unknown, as it stood when this one was requested.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Override {
    delivery: String,
    /// Its last attempt; none when its journal could not be read at all.
    attempt: Option<String>,
    /// Why its effect stayed unknown.
    reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
struct RequestFact(Request);

impl PartialEq for RequestFact {
    fn eq(&self, other: &Self) -> bool {
        canonical(&self.0).ok() == canonical(&other.0).ok()
    }
}

/// One command as it ran: how it ended, the digests of what it printed, and
/// the start of its stderr.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Execution {
    exit_code: Option<i32>,
    signal: Option<i32>,
    started_at_ms: u64,
    ended_at_ms: u64,
    stdout_digest: String,
    stderr_digest: String,
    stderr: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    inherited_digests: BTreeMap<String, String>,
}

impl Execution {
    /// `began` is the time taken before the command was spawned: it cannot
    /// have read anything earlier, so it is the time an observation speaks
    /// from, and the order observations are judged in.
    fn of(record: &TaskReceipt, began: u64) -> Self {
        let mut stderr = record.stderr.clone();
        if stderr.len() > KEPT_STDERR {
            let mut end = KEPT_STDERR;
            while !stderr.is_char_boundary(end) {
                end -= 1;
            }
            stderr.truncate(end);
        }
        let started_at_ms = record.started_at_ms.map_or(began, |at| at.min(began));
        Execution {
            exit_code: record.exit_code,
            signal: record.signal,
            started_at_ms,
            ended_at_ms: record
                .started_at_ms
                .unwrap_or(began)
                .saturating_add(record.duration_ms),
            stdout_digest: record.stdout_digest.clone(),
            stderr_digest: record.stderr_digest.clone(),
            stderr,
            inherited_digests: record.inherited_digests.clone(),
        }
    }

    fn passed(&self) -> bool {
        self.exit_code == Some(0) && self.signal.is_none()
    }

    fn ended(&self) -> String {
        match (self.exit_code, self.signal) {
            (_, Some(signal)) => format!("was ended by signal {signal}"),
            (Some(code), None) => format!("exited {code}"),
            (None, None) => "could not be started".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum Fact {
    Requested {
        request: Box<RequestFact>,
    },
    Resumed {
        generation: u64,
    },
    /// Durable before the apply's shell is spawned.
    Dispatched {
        attempt: String,
        generation: u64,
        supersedes: Option<String>,
        /// Asked for with `resume --reapply`: the only way to apply again.
        reapply: bool,
        /// Its place in the target's sequence: which apply to the target
        /// came after which, never by the clock.
        target_sequence: u64,
    },
    Started {
        attempt: String,
        generation: u64,
        process_group: i32,
        started_at_ms: u64,
    },
    /// The apply exited 0: the target accepted the change. Nothing more.
    Acknowledged {
        attempt: String,
        generation: u64,
        output: String,
    },
    /// The apply reported a failure (exit code), was ended by a signal sykli
    /// did not send, or could not be started (neither).
    Failed {
        attempt: String,
        generation: u64,
        exit_code: Option<i32>,
        signal: Option<i32>,
        output: String,
    },
    /// sykli's own stop reached the apply.
    Interrupted {
        attempt: String,
        generation: u64,
        signal: i32,
        exit_code: Option<i32>,
        ended_by: Option<i32>,
    },
    /// A later coordinator found the attempt without an end.
    Lost {
        attempt: String,
        generation: u64,
        reason: String,
    },
    /// What the observer reported, when (`observer.started_at_ms`), and the
    /// digest of exactly what it printed. `generation` is the coordinator's;
    /// none for `sykli observe`, which observes beside any coordinator.
    Observed {
        generation: Option<u64>,
        observer: Box<Execution>,
        observation: Option<Box<Observation>>,
        /// Why nothing the observer printed was accepted.
        error: Option<String>,
        smoke: Option<Box<Execution>>,
    },
    /// An apply whose end is unknown, judged from an observation of the
    /// target (`observation` is that record's sequence).
    Reconciled {
        attempt: String,
        generation: u64,
        effect: String,
        observation: u64,
        reason: String,
    },
}

impl Fact {
    fn observed(
        generation: Option<u64>,
        observer: Execution,
        observation: Option<Observation>,
        error: Option<String>,
        smoke: Option<Execution>,
    ) -> Self {
        Fact::Observed {
            generation,
            observer: Box::new(observer),
            observation: observation.map(Box::new),
            error,
            smoke: smoke.map(Box::new),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: String,
    delivery: String,
    sequence: u64,
    previous: Option<String>,
    recorded_at: u64,
    fact: Fact,
}

impl journal::Record for Record {
    type Fact = Fact;
    const SCHEMA: &'static str = RECORD;
    const OWNER: &'static str = "delivery";
    fn new(
        delivery: String,
        sequence: u64,
        previous: Option<String>,
        recorded_at: u64,
        fact: Fact,
    ) -> Self {
        Record {
            schema: RECORD.into(),
            delivery,
            sequence,
            previous,
            recorded_at,
            fact,
        }
    }
    fn schema(&self) -> &str {
        &self.schema
    }
    fn owner(&self) -> &str {
        &self.delivery
    }
    fn sequence(&self) -> u64 {
        self.sequence
    }
    fn previous(&self) -> Option<&str> {
        self.previous.as_deref()
    }
}

/// How an apply attempt ended.
#[derive(Clone, Debug)]
enum End {
    Acknowledged {
        at: u64,
    },
    Failed {
        at: u64,
        exit_code: Option<i32>,
        signal: Option<i32>,
    },
    Interrupted {
        at: u64,
        signal: i32,
    },
    Lost {
        at: u64,
        reason: String,
    },
}

/// What an attempt did to the target, as far as is known.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Effect {
    /// It has not ended.
    Pending,
    /// The apply exited 0.
    Applied,
    /// The apply reported failure, or never started; the target may hold
    /// part of what it sent.
    Refused,
    /// It ended without saying (lost, interrupted, killed), and no
    /// observation has shown its effect.
    Unknown,
    /// Its end was unknown, and an observation showed its effect.
    Observed,
}

#[derive(Clone, Debug)]
struct Reconciliation {
    effect: String,
    observation: u64,
    /// The cited observation reached the target: its observer exited 0 with
    /// a valid report of a reachable target. Without that, nothing about
    /// the target was learned.
    reached: bool,
    reason: String,
    at: u64,
}

#[derive(Clone, Debug)]
struct AttemptState {
    id: String,
    generation: u64,
    supersedes: Option<String>,
    reapply: bool,
    dispatched_at: u64,
    dispatched_sequence: u64,
    /// Its place in the target's sequence.
    target_sequence: u64,
    started: Option<(i32, u64)>,
    end: Option<End>,
    reconciled: Vec<Reconciliation>,
}

impl AttemptState {
    /// From when an observation can speak about what this attempt did: its
    /// recorded end, or its dispatch while it has none. An observer that
    /// began earlier may have read the state before it, wherever its record
    /// landed in the journal.
    fn observable_from(&self) -> u64 {
        match &self.end {
            Some(
                End::Acknowledged { at }
                | End::Failed { at, .. }
                | End::Interrupted { at, .. }
                | End::Lost { at, .. },
            ) => *at,
            None => self.dispatched_at,
        }
    }

    fn effect(&self) -> Effect {
        match &self.end {
            None => Effect::Pending,
            Some(End::Acknowledged { .. }) => Effect::Applied,
            Some(End::Failed {
                exit_code: Some(_),
                signal: None,
                ..
            })
            | Some(End::Failed {
                exit_code: None,
                signal: None,
                ..
            }) => Effect::Refused,
            Some(_) => match self.reconciled.last() {
                Some(last) if last.effect == "observed" => Effect::Observed,
                _ => Effect::Unknown,
            },
        }
    }
}

#[derive(Clone, Debug)]
struct Seen {
    sequence: u64,
    generation: Option<u64>,
    observer: Execution,
    observation: Option<Observation>,
    error: Option<String>,
    smoke: Option<Execution>,
}

impl Seen {
    fn usable(&self) -> Option<&Observation> {
        self.observation.as_ref().filter(|o| o.reachable)
    }

    fn at(&self) -> u64 {
        self.observer.started_at_ms
    }
}

/// A delivery's journal, validated: what is known, and nothing else.
struct History {
    id: String,
    records: Vec<Envelope<Record>>,
    request: Option<Request>,
    generation: u64,
    attempts: BTreeMap<String, AttemptState>,
    lineage: Vec<String>,
    seen: Vec<Seen>,
}

impl History {
    fn new(id: &str) -> Self {
        History {
            id: id.into(),
            records: Vec::new(),
            request: None,
            generation: 0,
            attempts: BTreeMap::new(),
            lineage: Vec::new(),
            seen: Vec::new(),
        }
    }

    fn load(directory: &Path, id: &str) -> Result<Self, String> {
        let mut history = History::new(id);
        history.records = journal::load(&directory.join("records"), id, |record: &Record| {
            history.apply(record)
        })?;
        if history.request.is_none() {
            return Err(format!("delivery {id} has no request record"));
        }
        Ok(history)
    }

    fn request(&self) -> &Request {
        self.request
            .as_ref()
            .expect("a loaded delivery has its request")
    }

    fn last(&self) -> Option<&AttemptState> {
        self.lineage.last().map(|id| &self.attempts[id])
    }

    fn current(&self, generation: u64) -> Result<(), String> {
        if generation != self.generation {
            return Err(format!(
                "stale record: generation {generation} is fenced off by generation {}",
                self.generation
            ));
        }
        Ok(())
    }

    fn apply(&mut self, record: &Record) -> Result<(), String> {
        let at = record.recorded_at;
        if let Fact::Requested { request } = &record.fact {
            if record.sequence != 1 || self.request.is_some() {
                return Err("a delivery is requested once, in its first record".into());
            }
            if identity("sykli-delivery.v1", &request.0)? != self.id {
                return Err("the delivery's request does not match its id".into());
            }
            request.0.contract.validate()?;
            request.0.definition()?;
            identity_text(&request.0.intended)?;
            if request.0.executor != EXECUTOR {
                return Err("the delivery's request names another executor".into());
            }
            self.request = Some(request.0.clone());
            self.generation = 1;
            return Ok(());
        }
        if self.request.is_none() {
            return Err("a record before the delivery's request".into());
        }
        match &record.fact {
            Fact::Requested { .. } => unreachable!(),
            Fact::Resumed { generation } => {
                if *generation != self.generation + 1 {
                    return Err("a resumed generation must follow the last".into());
                }
                self.generation = *generation;
            }
            Fact::Dispatched {
                attempt,
                generation,
                supersedes,
                reapply,
                target_sequence,
            } => {
                self.current(*generation)?;
                digest(attempt)?;
                if self.attempts.contains_key(attempt) || supersedes.as_ref() != self.lineage.last()
                {
                    return Err("invalid attempt identity or lineage".into());
                }
                let after = self
                    .last()
                    .map_or(self.request().target_sequence, |last| last.target_sequence);
                if *target_sequence <= after {
                    return Err(
                        "an apply's place in the target's sequence comes after its request's \
                         and its last apply's"
                            .into(),
                    );
                }
                // Invariant H: one apply, unless the last one's effect is
                // known not to be there (refused), or is still unknown after a
                // reconciliation whose observation reached the target, and
                // the caller asked.
                if let Some(last) = self.last() {
                    let reached = last.reconciled.last().is_some_and(|r| r.reached);
                    let allowed = *reapply
                        && match last.effect() {
                            Effect::Refused => true,
                            Effect::Unknown => reached,
                            _ => false,
                        };
                    if !allowed {
                        return Err(format!(
                            "attempt {} is not superseded: an apply runs again only on \
                             --reapply, after one that failed or whose unknown effect was \
                             reconciled from an observation that reached the target",
                            last.id
                        ));
                    }
                } else if *reapply {
                    return Err("a first attempt is not a re-apply".into());
                }
                self.attempts.insert(
                    attempt.clone(),
                    AttemptState {
                        id: attempt.clone(),
                        generation: *generation,
                        supersedes: supersedes.clone(),
                        reapply: *reapply,
                        dispatched_at: at,
                        dispatched_sequence: record.sequence,
                        target_sequence: *target_sequence,
                        started: None,
                        end: None,
                        reconciled: Vec::new(),
                    },
                );
                self.lineage.push(attempt.clone());
            }
            Fact::Started {
                attempt,
                generation,
                process_group,
                started_at_ms,
            } => {
                self.current(*generation)?;
                let state = self
                    .attempts
                    .get_mut(attempt)
                    .ok_or("a start without a dispatch")?;
                if state.generation != *generation || state.started.is_some() || state.end.is_some()
                {
                    return Err("an attempt starts once, before it ends".into());
                }
                state.started = Some((*process_group, *started_at_ms));
            }
            Fact::Acknowledged {
                attempt,
                generation,
                output,
            }
            | Fact::Failed {
                attempt,
                generation,
                output,
                ..
            } => {
                let current = self.generation;
                let state = self
                    .attempts
                    .get_mut(attempt)
                    .ok_or("an end without a dispatch")?;
                if state.generation != *generation || *generation != current {
                    return Err(format!(
                        "stale completion: attempt {attempt} of generation {} is fenced off by \
                         generation {current}",
                        state.generation
                    ));
                }
                if state.end.is_some() {
                    return Err(format!("conflicting ends: attempt {attempt} already ended"));
                }
                digest(output)?;
                let unstarted_failure = matches!(
                    record.fact,
                    Fact::Failed {
                        exit_code: None,
                        signal: None,
                        ..
                    }
                );
                if state.started.is_none() && !unstarted_failure {
                    return Err("an apply that never started has no exit".into());
                }
                // A started apply ends with an exit or a signal; without one,
                // how it ended is unknown, which is never a refusal.
                if state.started.is_some() && unstarted_failure {
                    return Err(
                        "an apply that started has no recorded exit: how it ended is unknown"
                            .into(),
                    );
                }
                state.end = Some(match &record.fact {
                    Fact::Acknowledged { .. } => End::Acknowledged { at },
                    Fact::Failed {
                        exit_code, signal, ..
                    } => {
                        if *exit_code == Some(0) && signal.is_none() {
                            return Err("an apply that exited 0 is acknowledged".into());
                        }
                        End::Failed {
                            at,
                            exit_code: *exit_code,
                            signal: *signal,
                        }
                    }
                    _ => unreachable!(),
                });
            }
            Fact::Interrupted {
                attempt,
                generation,
                signal,
                ..
            } => {
                self.current(*generation)?;
                let state = self
                    .attempts
                    .get_mut(attempt)
                    .ok_or("an interruption without a dispatch")?;
                if state.end.is_some() || state.generation != *generation {
                    return Err(
                        "only an unfinished attempt of this generation is interrupted".into(),
                    );
                }
                state.end = Some(End::Interrupted {
                    at,
                    signal: *signal,
                });
            }
            Fact::Lost {
                attempt,
                generation,
                reason,
            } => {
                self.current(*generation)?;
                let state = self
                    .attempts
                    .get_mut(attempt)
                    .ok_or("a lost attempt without a dispatch")?;
                if state.end.is_some() || state.generation >= *generation {
                    return Err(
                        "only an unfinished attempt of an earlier generation is lost".into(),
                    );
                }
                state.end = Some(End::Lost {
                    at,
                    reason: reason.clone(),
                });
            }
            Fact::Observed {
                generation,
                observer,
                observation,
                error,
                smoke,
            } => {
                if let Some(generation) = generation {
                    self.current(*generation)?;
                }
                digest(&observer.stdout_digest)?;
                match (observation, error) {
                    (Some(observation), None) => observation.validate()?,
                    (None, Some(_)) => {}
                    _ => return Err("an observation, or why there is none".into()),
                }
                self.seen.push(Seen {
                    sequence: record.sequence,
                    generation: *generation,
                    observer: (**observer).clone(),
                    observation: observation.as_deref().cloned(),
                    error: error.clone(),
                    smoke: smoke.as_deref().cloned(),
                });
            }
            Fact::Reconciled {
                attempt,
                generation,
                effect,
                observation,
                reason,
            } => {
                self.current(*generation)?;
                let request = self.request.as_ref().unwrap();
                let (delivery, intended) = (self.id.clone(), request.intended.clone());
                let state = self
                    .attempts
                    .get(attempt)
                    .ok_or("a reconciliation without a dispatch")?;
                if state.effect() != Effect::Unknown {
                    return Err(format!(
                        "attempt {attempt}'s effect is not unknown; there is nothing to reconcile"
                    ));
                }
                let seen = self
                    .seen
                    .iter()
                    .find(|seen| seen.sequence == *observation)
                    .filter(|seen| {
                        seen.sequence > state.dispatched_sequence
                            && seen.at() >= state.observable_from()
                    })
                    .ok_or("a reconciliation cites no observation made after its dispatch")?;
                // Recovery never creates a fact: an effect is observed only
                // when the cited observation shows it.
                let shows = seen
                    .observation
                    .as_ref()
                    .is_some_and(|o| o.shows(&delivery, &intended));
                match effect.as_str() {
                    "observed" if shows => {}
                    "unknown" => {}
                    _ => {
                        return Err(format!(
                            "effect {effect:?} does not follow from observation {observation}"
                        ));
                    }
                }
                let reached = seen.usable().is_some();
                let state = self.attempts.get_mut(attempt).unwrap();
                state.reconciled.push(Reconciliation {
                    effect: effect.clone(),
                    observation: *observation,
                    reached,
                    reason: reason.clone(),
                    at,
                });
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Where deliveries live.

fn deliveries_directory(common: &Path) -> PathBuf {
    common.join("sykli").join("deliveries")
}

fn delivery_directory(common: &Path, id: &str) -> Result<PathBuf, String> {
    digest(id).map_err(|_| format!("{id:?} is not a delivery id"))?;
    Ok(deliveries_directory(common).join(id))
}

fn common_dir() -> Result<PathBuf, String> {
    let repository = crate::repo::repository().map_err(
        |_| "not inside a git repository; deliveries are kept in its common git directory",
    )?;
    crate::absolute(&repository.common_dir)
}

fn unknown(id: &str) -> String {
    format!(
        "unknown delivery {id}: this clone keeps no record of it (a clone keeps at most \
         SYKLI_DELIVERY_BUDGET deliveries, default {DEFAULT_BUDGET}, and removes the oldest)"
    )
}

/// A delivery this clone keeps, as its first record says.
struct Listed {
    id: String,
    path: PathBuf,
    request: Request,
}

/// Every delivery this clone keeps whose first record reads.
fn list(common: &Path) -> Vec<Listed> {
    let Ok(entries) = fs::read_dir(deliveries_directory(common)) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if digest(&id).is_err() {
            continue;
        }
        let first = journal::record_path(&entry.path().join("records"), 1);
        let request = fs::read(&first)
            .map_err(err)
            .and_then(|bytes| decode::<Envelope<Record>>(&bytes))
            .and_then(|envelope| match envelope.record.fact {
                Fact::Requested { request } => Ok(request.0),
                _ => Err("its first record is not its request".into()),
            });
        if let Ok(request) = request {
            found.push(Listed {
                id,
                path: entry.path(),
                request,
            });
        }
    }
    found.sort_by(|a, b| {
        (a.request.requested_at_ms, &a.id).cmp(&(b.request.requested_at_ms, &b.id))
    });
    found
}

/// The lock one `deliver` to `target` holds from its busy check until its
/// delivery is recorded and held: `deliveries/.targets/<digest of the
/// name>/journal-lock`, taken exclusively (a second deliver waits).
fn target_lock(common: &Path, target: &str) -> Result<Lease, String> {
    Lease::journal(&target_directory(common, target)?)
}

/// `deliveries/.targets/<digest of the name>/`: the target's lock and its
/// sequence.
fn target_directory(common: &Path, target: &str) -> Result<PathBuf, String> {
    let mut directory = common.to_path_buf();
    let name = crate::sha256(target.as_bytes());
    for part in ["sykli", "deliveries", ".targets", &name[..32]] {
        directory.push(part);
        crate::hook::ensure_directory(&directory, |path| fs::create_dir(path))?;
    }
    Ok(directory)
}

/// The next place in the target's sequence (`.targets/<digest>/sequence/`),
/// durable before it is returned, taken under the sequence's own lock, held
/// only for this: every request and every dispatch to the target in this
/// clone gets a place after every one before it, the order in which they
/// happened, which a clock stepping back cannot invert. (A request takes its
/// place holding the target's lock too; a dispatch does not need it, since
/// only a delivery that holds its target, the one live there, dispatches,
/// and a coordinator stopped mid-dispatch must never hold up a `deliver`
/// that would otherwise be told `target-busy`.) Without its file (removed by
/// hand), it starts after every place the clone's deliveries recorded.
fn next_in_sequence(common: &Path, target: &str) -> Result<u64, String> {
    let directory = target_directory(common, target)?.join("sequence");
    crate::hook::ensure_directory(&directory, |path| fs::create_dir(path))?;
    let _sequence = Lease::journal(&directory)?;
    let path = directory.join("last");
    let last = match fs::read_to_string(&path) {
        Ok(text) => text
            .trim()
            .parse::<u64>()
            .map_err(|_| format!("{}: not a sequence number", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => list(common)
            .iter()
            .filter(|d| d.request.target == target)
            .map(|d| {
                let applied = Lease::journal_read(&d.path)
                    .and_then(|_journal| History::load(&d.path, &d.id))
                    .ok()
                    .and_then(|h| h.last().map(|a| a.target_sequence));
                applied.unwrap_or(0).max(d.request.target_sequence)
            })
            .max()
            .unwrap_or(0),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let next = last + 1;
    let staged = path.with_file_name(format!(
        ".last-{}-{}",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    let written = fs::File::create(&staged)
        .and_then(|mut file| {
            use std::io::Write;
            file.write_all(next.to_string().as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&staged, &path))
        .map_err(|e| format!("{}: {e}", path.display()));
    if written.is_err() {
        let _ = fs::remove_file(&staged);
    }
    written?;
    crate::canonical::sync_directory(path.parent().unwrap(), Durability::Fsync)?;
    Ok(next)
}

/// Why a delivery still holds its target, if it does.
enum Hold {
    /// Its coordinator is alive, or its last apply still runs without one.
    Busy(&'static str),
    /// Its last apply was dispatched and how it ended is unknown, and no
    /// observation of the target has reconciled it (invariant H: nothing is
    /// applied to the target over it until one has).
    Unreconciled { attempt: String, how: String },
    /// It was reconciled, but no observation reached the target, so its
    /// effect is as unknown as before. Only `--reapply-over` passes it.
    Unreached { attempt: String, reason: String },
    /// Its journal does not read: nothing is known of its applies.
    Unreadable(String),
}

/// Whether a delivery holds its target: its coordinator or apply is alive,
/// or its last apply's effect is unknown and was never reconciled with an
/// observation that reached the target.
fn hold(delivery: &Listed) -> Option<Hold> {
    if Lease::observe(&delivery.path.join("lease")).unwrap_or(true) {
        return Some(Hold::Busy("is still running"));
    }
    let history = Lease::journal_read(&delivery.path)
        .and_then(|_journal| History::load(&delivery.path, &delivery.id));
    let history = match history {
        Ok(history) => history,
        Err(error) => return Some(Hold::Unreadable(error)),
    };
    let last = history.last()?;
    if last.end.is_none() {
        if attempt_alive(&delivery.path, last).unwrap_or(true) {
            return Some(Hold::Busy(
                "has an apply still running without its coordinator; once it has ended, `sykli \
                 resume --delivery` reconciles it",
            ));
        }
        return Some(Hold::Unreconciled {
            attempt: last.id.clone(),
            how: "its coordinator ended without recording how the apply ended".into(),
        });
    }
    if last.effect() != Effect::Unknown {
        return None;
    }
    match last.reconciled.last() {
        Some(reconciled) if reconciled.reached => None,
        Some(reconciled) => Some(Hold::Unreached {
            attempt: last.id.clone(),
            reason: reconciled.reason.clone(),
        }),
        None => Some(Hold::Unreconciled {
            attempt: last.id.clone(),
            how: match &last.end {
                Some(End::Interrupted { signal, .. }) => {
                    format!("sykli stopped the apply on signal {signal}")
                }
                Some(End::Failed { signal, .. }) => format!(
                    "the apply was ended by signal {} that sykli did not send",
                    signal.unwrap_or(0)
                ),
                Some(End::Lost { reason, .. }) => format!("the apply was lost: {reason}"),
                _ => "how the apply ended is unknown".into(),
            },
        }),
    }
}

/// Whether anything may be applied to `target` now: no other delivery to it
/// (`except` aside) holds it. A delivery whose effect stayed unknown because
/// its reconciliation never reached the target is passed only when `over`
/// names it, or a later delivery to the target already applied over it; what
/// is passed is returned, for the request to record. Every name in `over`
/// must be such a delivery. Call it holding the target's lock.
fn clear_to_apply(
    listed: &[Listed],
    target: &str,
    except: Option<&str>,
    over: &[String],
) -> Result<Vec<Override>, String> {
    let to_target: Vec<&Listed> = listed
        .iter()
        .filter(|d| d.request.target == target && Some(d.id.as_str()) != except)
        .collect();
    let applied_over: BTreeSet<&str> = to_target
        .iter()
        .flat_map(|d| d.request.reapply_over.iter().map(|o| o.delivery.as_str()))
        .collect();
    let mut asked: BTreeSet<&str> = over.iter().map(String::as_str).collect();
    let mut passed = Vec::new();
    for other in to_target {
        let id = &other.id;
        let (attempt, reason) = match hold(other) {
            None => continue,
            Some(Hold::Busy(why)) => {
                return Err(format!(
                    "target-busy: delivery {id} to {target} {why}; see `sykli status --delivery \
                     {id}`"
                ));
            }
            Some(Hold::Unreconciled { attempt, how }) => {
                return Err(format!(
                    "target-unreconciled: delivery {id} to {target} dispatched apply attempt \
                     {attempt}, and {how}: whether it took effect is unknown, and nothing is \
                     applied to {target} until that is reconciled with the target. Run `sykli \
                     resume --delivery {id}` first: it observes the target and applies nothing"
                ));
            }
            Some(Hold::Unreached { attempt, reason }) => (Some(attempt), reason),
            Some(Hold::Unreadable(error)) => (None, format!("its journal does not read: {error}")),
        };
        if applied_over.contains(id.as_str()) {
            continue;
        }
        if !asked.remove(id.as_str()) {
            return Err(format!(
                "target-unreconciled: delivery {id} to {target} has an apply whose effect is \
                 unknown, and its reconciliation could not reach the target ({reason}). Run \
                 `sykli resume --delivery {id}` again once the target answers; or, to apply over \
                 that unknown effect anyway, deliver with `--reapply-over {id}`, which the new \
                 delivery records"
            ));
        }
        passed.push(Override {
            delivery: id.clone(),
            attempt,
            reason,
        });
    }
    if let Some(name) = asked.first() {
        return Err(format!(
            "--reapply-over {name}: no delivery to {target} with an unknown effect that its \
             reconciliation could not settle is named {name}; nothing is applied over"
        ));
    }
    Ok(passed)
}

/// The delivery in this clone that superseded this one: of the other
/// deliveries to the target, the one whose last apply was dispatched last,
/// if that was after this one's last apply, in the target's sequence (never
/// by the clock). Its id and when it was dispatched. None while this one has
/// applied nothing.
fn superseded_by(common: &Path, id: &str, history: &History) -> Option<(String, u64)> {
    let request = history.request();
    let mine = history.last()?.target_sequence;
    list(common)
        .into_iter()
        .filter(|other| other.request.target == request.target && other.id != id)
        .filter_map(|other| {
            let _journal = Lease::journal_read(&other.path).ok()?;
            let theirs = History::load(&other.path, &other.id).ok()?;
            let last = theirs.last()?;
            (last.target_sequence > mine)
                .then(|| (last.target_sequence, other.id.clone(), last.dispatched_at))
        })
        .max()
        .map(|(_, other, dispatched_at)| (other, dispatched_at))
}

// ---------------------------------------------------------------------------
// The coordinator.

/// A delivery's coordinator: the holder of its lease, the parent of its
/// applies, and (beside `sykli observe`) the writer of its journal.
struct Coordinator {
    directory: PathBuf,
    id: String,
    lease: Lease,
    history: Mutex<History>,
    generation: AtomicU64,
    fenced: AtomicBool,
}

/// One apply, dispatched: its id and its lease, which its shell inherits.
struct Attempt<'a> {
    coordinator: &'a Coordinator,
    id: String,
    generation: u64,
    lease: Lease,
}

impl Journaled for Attempt<'_> {
    fn inherit(&self, command: &mut ProcessCommand) {
        self.lease.inherit(command);
    }
    fn started(&self, group: i32, started_at_ms: u64) -> Result<(), String> {
        self.coordinator.append(Fact::Started {
            attempt: self.id.clone(),
            generation: self.generation,
            process_group: group,
            started_at_ms,
        })?;
        crate::checkpoint("after-apply-start")
    }
}

/// Whether an unfinished apply may still be running: its lease is held, or
/// the process group the journal recorded for it still exists.
fn attempt_alive(directory: &Path, attempt: &AttemptState) -> Result<bool, String> {
    Ok(
        Lease::observe(&directory.join("attempt-leases").join(&attempt.id))?
            || attempt
                .started
                .is_some_and(|(group, _)| journal::group_exists(group)),
    )
}

impl Coordinator {
    /// Record a new delivery, durably, and hold it: its lease is taken
    /// before its first record is published.
    fn create(request: Request) -> Result<Self, String> {
        let id = identity("sykli-delivery.v1", &request)?;
        let common = PathBuf::from(&request.common_dir);
        let mut parent = common.clone();
        for part in ["sykli", "deliveries"] {
            parent.push(part);
            crate::hook::ensure_directory(&parent, |path| fs::create_dir(path))?;
        }
        prune(&common, &id);
        let directory = delivery_directory(&common, &id)?;
        let staging = parent.join(format!(".creating-{id}-{}", std::process::id()));
        fs::create_dir(&staging).map_err(err)?;
        let staged = fs::create_dir(staging.join("records"))
            .map_err(err)
            .and_then(|()| Lease::acquire(&staging))
            .and_then(|lease| {
                fs::rename(&staging, &directory)
                    .map_err(|e| format!("{}: {e}", directory.display()))?;
                Ok(lease)
            });
        let lease = match staged {
            Ok(lease) => lease,
            Err(error) => {
                let _ = fs::remove_dir_all(&staging);
                return Err(error);
            }
        };
        for synced in [&directory, &parent, &common.join("sykli"), &common] {
            crate::canonical::sync_directory(synced, Durability::Fsync)?;
        }
        let coordinator = Coordinator {
            directory,
            id: id.clone(),
            lease,
            history: Mutex::new(History::new(&id)),
            generation: AtomicU64::new(1),
            fenced: AtomicBool::new(false),
        };
        coordinator.append(Fact::Requested {
            request: Box::new(RequestFact(request)),
        })?;
        Ok(coordinator)
    }

    /// The delivery a detached coordinator was started for, with the lease
    /// its client handed it.
    fn received(id: &str, fd: i32) -> Result<Self, String> {
        let directory = delivery_directory(&common_dir()?, id)?;
        let lease = Lease::received(&directory, fd)?;
        let history = History::load(&directory, id)?;
        if history.generation != 1 || !history.attempts.is_empty() {
            return Err("a detached coordinator starts only a fresh delivery".into());
        }
        Ok(Coordinator {
            directory,
            id: id.into(),
            lease,
            history: Mutex::new(history),
            generation: AtomicU64::new(1),
            fenced: AtomicBool::new(false),
        })
    }

    /// Take over a delivery whose coordinator is gone. Refused while it is
    /// alive, and while an apply's process still is (unless `assume_ended`
    /// says its process group is not the apply's any more). Nothing is
    /// written: `take_over` records the new generation.
    fn reopen(id: &str, assume_ended: bool) -> Result<(Self, Vec<(String, i32)>), String> {
        let common = common_dir()?;
        let directory = delivery_directory(&common, id)?;
        if !directory.join("records").is_dir() {
            return Err(unknown(id));
        }
        let lease = Lease::acquire(&directory).map_err(|error| {
            if error.starts_with("production-busy") {
                format!("delivery-busy: delivery {id}'s coordinator is still running")
            } else {
                error
            }
        })?;
        let history = History::load(&directory, id)?;
        let mut assumed = Vec::new();
        for attempt in history.attempts.values().filter(|a| a.end.is_none()) {
            if Lease::observe(&directory.join("attempt-leases").join(&attempt.id))? {
                return Err(format!(
                    "apply attempt {} of delivery {id} is still running without a coordinator; \
                     wait for it to end, then resume: its effect is then reconciled with the \
                     target",
                    attempt.id
                ));
            }
            let group = attempt.started.map(|(group, _)| group);
            if let Some(group) = group.filter(|group| journal::group_exists(*group)) {
                if !assume_ended {
                    return Err(format!(
                        "apply attempt {} may still be running without a coordinator: its \
                         process group {group} still exists. Wait for it to end, or stop it, \
                         then resume; if that group now belongs to an unrelated process, resume \
                         with --assume-ended",
                        attempt.id
                    ));
                }
                assumed.push((attempt.id.clone(), group));
            }
        }
        let generation = history.generation;
        Ok((
            Coordinator {
                directory,
                id: id.into(),
                lease,
                history: Mutex::new(history),
                generation: AtomicU64::new(generation),
                fenced: AtomicBool::new(false),
            },
            assumed,
        ))
    }

    /// Open the next generation and record every apply the last one left
    /// unfinished as lost: its effect is unknown, never assumed.
    fn take_over(&self, assumed: &[(String, i32)]) -> Result<(), String> {
        let generation = self.generation() + 1;
        self.append(Fact::Resumed { generation })?;
        self.generation.store(generation, Ordering::SeqCst);
        let unfinished = {
            let history = self.history.lock().unwrap();
            history
                .attempts
                .values()
                .filter(|a| a.end.is_none())
                .map(|a| a.id.clone())
                .collect::<Vec<_>>()
        };
        for attempt in unfinished {
            let reason = match assumed.iter().find(|(id, _)| *id == attempt) {
                Some((_, group)) => format!(
                    "its process group {group} still existed when `resume --assume-ended` took \
                     the delivery over; taken as ended on that word; whether the apply took \
                     effect is unknown"
                ),
                None => "its coordinator ended without recording how the apply ended; whether \
                         it took effect is unknown"
                    .into(),
            };
            self.append(Fact::Lost {
                attempt,
                generation,
                reason,
            })?;
        }
        Ok(())
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn request(&self) -> Request {
        self.history.lock().unwrap().request().clone()
    }

    fn last(&self) -> Option<AttemptState> {
        self.history.lock().unwrap().last().cloned()
    }

    fn full(&self) -> bool {
        self.history.lock().unwrap().records.len() >= MAX_RECORDS
    }

    /// Append a fact under the journal lock; its sequence. The journal is
    /// reread when a record beyond the last one known is there (an
    /// observation `sykli observe` made, or a newer coordinator's); a newer
    /// generation fences this coordinator off for good.
    fn append(&self, fact: Fact) -> Result<u64, String> {
        let mut history = self.history.lock().unwrap();
        let stale = || {
            format!(
                "stale coordinator: delivery {} was taken over by a newer generation; this one \
                 (generation {}) records nothing more",
                self.id,
                self.generation()
            )
        };
        if self.fenced.load(Ordering::SeqCst) {
            return Err(stale());
        }
        let _journal = Lease::journal(&self.directory)?;
        let records = self.directory.join("records");
        if journal::record_path(&records, history.records.len() as u64 + 1).exists() {
            *history = History::load(&self.directory, &self.id)?;
            if history.generation != self.generation() {
                self.fenced.store(true, Ordering::SeqCst);
                return Err(stale());
            }
        }
        let envelope = journal::seal(self.id.clone(), &history.records, fact)?;
        history.apply(&envelope.record)?;
        if let Err(error) = journal::write(&records, &envelope, Durability::Fsync) {
            match History::load(&self.directory, &self.id) {
                Ok(reloaded) => *history = reloaded,
                Err(_) => self.fenced.store(true, Ordering::SeqCst),
            }
            return Err(error);
        }
        let sequence = envelope.record.sequence;
        history.records.push(envelope);
        Ok(sequence)
    }

    /// Keep an apply's output record, named by its digest.
    fn persist(&self, record: &TaskReceipt) -> Result<String, String> {
        let bytes = serde_json::to_vec(record).map_err(err)?;
        let digest = crate::sha256(&bytes);
        publish_with(
            &self
                .directory
                .join("outputs")
                .join(format!("{digest}.json")),
            &bytes,
            Durability::Fsync,
        )?;
        Ok(digest)
    }

    /// Dispatch an apply: its lease is taken and its `dispatched` record is
    /// durable before anything is spawned.
    fn dispatch(&self, reapply: bool) -> Result<Attempt<'_>, String> {
        let generation = self.generation();
        let (supersedes, sequence) = {
            let history = self.history.lock().unwrap();
            (history.lineage.last().cloned(), history.records.len())
        };
        let id = identity(
            "sykli-delivery-attempt.v1",
            &json!({"delivery": self.id, "sequence": sequence, "time": now(),
                    "pid": std::process::id(), "nonce": NONCE.fetch_add(1, Ordering::Relaxed)}),
        )?;
        let lease = Lease::attempt(&self.directory, &id)?;
        // Its place in the target's sequence: applies to a target are ordered
        // as they happened, never by the clock.
        let request = self.request();
        let common = PathBuf::from(&request.common_dir);
        self.append(Fact::Dispatched {
            attempt: id.clone(),
            generation,
            supersedes,
            reapply,
            target_sequence: next_in_sequence(&common, &request.target)?,
        })?;
        Ok(Attempt {
            coordinator: self,
            id,
            generation,
            lease,
        })
    }

    /// Hand the delivery to a coordinator in its own session that outlives
    /// this process; it inherits the lease.
    fn detach(self) -> Result<String, String> {
        let request = self.request();
        let repository = crate::repo::repository()?;
        let executable = std::env::current_exe().map_err(err)?;
        let mut command = ProcessCommand::new(executable);
        let fd = self.lease.inherit(&mut command);
        // SAFETY: setsid is async-signal-safe and allocates nothing.
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(|| {
                if journal::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.directory.join("coordinator.log"))
            .map_err(err)?;
        command
            .arg("--repo-root")
            .arg(&repository.root)
            .arg("--git-common-dir")
            .arg(&request.common_dir)
            .args([
                "__delivery_coordinator",
                "--delivery",
                &self.id,
                "--lease-fd",
            ])
            .arg(fd.to_string())
            .current_dir(&request.workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log);
        journal::close_inherited_on_exec();
        command
            .spawn()
            .map(|_| self.id.clone())
            .map_err(|error| format!("the delivery's coordinator could not be started: {error}"))
    }

    /// One apply: dispatched, run in its own process group holding its
    /// lease, and ended. None, or the signal that stopped it.
    fn apply(
        &self,
        runtime: &ShellRuntime,
        progress: Progress,
        reapply: bool,
    ) -> Result<Option<i32>, String> {
        let request = self.request();
        let definition = request.definition()?.clone();
        // The artifact is in hand before anything is dispatched: an apply that
        // could not be given it never runs, and nothing is recorded.
        let scratch = self.directory.join("scratch").join(format!(
            "{}-{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        let artifact = materialize(&request.artifact, &scratch)?;
        let attempt = match self.dispatch(reapply) {
            Ok(attempt) => attempt,
            Err(error) => {
                let _ = fs::remove_dir_all(&scratch);
                return Err(error);
            }
        };
        crate::checkpoint("after-dispatch")?;
        let task = command(
            &self.id,
            &request,
            "apply",
            &definition.apply,
            &[
                ("SYKLI_ARTIFACT", artifact.to_string_lossy().into_owned()),
                ("SYKLI_DELIVERY_ATTEMPT", attempt.id.clone()),
            ],
        );
        let launched = crate::launch(&task, runtime, progress, APPLY_CAPTURE, Some(&attempt));
        let _ = fs::remove_dir_all(&scratch);
        match launched? {
            Launched::Stopped { exit_code, signal } => {
                let stop = crate::runs::signalled().unwrap_or(15);
                self.append(Fact::Interrupted {
                    attempt: attempt.id.clone(),
                    generation: attempt.generation,
                    signal: stop,
                    exit_code,
                    ended_by: signal,
                })?;
                Ok(Some(stop))
            }
            Launched::Ran { record, .. } => {
                crate::checkpoint("after-apply-exit")?;
                // Started, but neither an exit nor a signal came back (the
                // wait failed): how the apply ended is unknown. No end is
                // recorded, so a later coordinator finds it lost and
                // reconciles it with the target; it is never a refusal.
                if record.started_at_ms.is_some()
                    && record.exit_code.is_none()
                    && record.signal.is_none()
                {
                    return Err(format!(
                        "apply attempt {} started, but how it ended could not be established \
                         ({}); `sykli resume --delivery {}` reconciles it with the target",
                        attempt.id,
                        record.error.as_deref().unwrap_or("no exit status"),
                        self.id
                    ));
                }
                let output = self.persist(&record)?;
                if record.exit_code == Some(0) && record.signal.is_none() {
                    self.append(Fact::Acknowledged {
                        attempt: attempt.id.clone(),
                        generation: attempt.generation,
                        output,
                    })?;
                } else {
                    self.append(Fact::Failed {
                        attempt: attempt.id.clone(),
                        generation: attempt.generation,
                        exit_code: record.exit_code,
                        signal: record.signal,
                        output,
                    })?;
                }
                crate::checkpoint("after-ack")?;
                Ok(None)
            }
        }
    }

    /// Observe once and record it; with `smoke`, run the smoke command too.
    /// The record's sequence, or the signal that stopped it.
    fn observe(&self, runtime: &ShellRuntime, smoke: bool) -> Result<Result<u64, i32>, String> {
        let request = self.request();
        let (observer, observation, error) = match observe_target(&self.id, &request, runtime)? {
            Round::Stopped(signal) => return Ok(Err(signal)),
            Round::Done(done) => *done,
        };
        let smoke = if smoke {
            match smoke_target(&self.id, &request, runtime)? {
                Some(Err(signal)) => return Ok(Err(signal)),
                Some(Ok(execution)) => Some(execution),
                None => None,
            }
        } else {
            None
        };
        crate::checkpoint("before-observed")?;
        let sequence = self.append(Fact::observed(
            Some(self.generation()),
            observer,
            observation,
            error,
            smoke,
        ))?;
        Ok(Ok(sequence))
    }

    /// Observe until the target settles or `settle_seconds` pass (once at
    /// least); smoke with the last observation. None, or a stopping signal.
    fn settle(&self, runtime: &ShellRuntime, observe_for: u64) -> Result<Option<i32>, String> {
        let request = self.request();
        let deadline = Instant::now() + Duration::from_secs(observe_for);
        loop {
            if let Some(signal) = crate::runs::signalled() {
                return Ok(Some(signal));
            }
            if self.full() {
                return Ok(None);
            }
            let (observer, observation, error) = match observe_target(&self.id, &request, runtime)?
            {
                Round::Stopped(signal) => return Ok(Some(signal)),
                Round::Done(done) => *done,
            };
            let settled = Instant::now() >= deadline
                || observation
                    .as_ref()
                    .is_some_and(|o| settled(&self.id, &request, o));
            let smoke = if settled {
                match smoke_target(&self.id, &request, runtime)? {
                    Some(Err(signal)) => return Ok(Some(signal)),
                    Some(Ok(execution)) => Some(execution),
                    None => None,
                }
            } else {
                None
            };
            crate::checkpoint("before-observed")?;
            self.append(Fact::observed(
                Some(self.generation()),
                observer,
                observation,
                error,
                smoke,
            ))?;
            if settled {
                return Ok(None);
            }
            let next = Instant::now() + observe_interval();
            while Instant::now() < next.min(deadline) {
                if let Some(signal) = crate::runs::signalled() {
                    return Ok(Some(signal));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }

    /// Apply, then observe: what a fresh delivery does, and what a resume
    /// does once nothing unknown stands in the way.
    fn apply_and_observe(
        &self,
        runtime: &ShellRuntime,
        progress: Progress,
        reapply: bool,
    ) -> Result<Done, String> {
        if let Some(signal) = self.apply(runtime, progress, reapply)? {
            return Ok(Done::Interrupted(signal));
        }
        let settle = match self.last().map(|attempt| attempt.effect()) {
            Some(Effect::Applied) => self.request().definition()?.settle_seconds,
            _ => 0,
        };
        match self.settle(runtime, settle)? {
            Some(signal) => Ok(Done::Interrupted(signal)),
            None => Ok(Done::Judged),
        }
    }
}

fn observe_interval() -> Duration {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("SYKLI_TEST_OBSERVE_INTERVAL_MS")
        .ok()
        .and_then(|ms| ms.parse().ok())
    {
        return Duration::from_millis(ms);
    }
    OBSERVE_INTERVAL
}

/// What a coordinator's work came to.
pub(crate) enum Done {
    /// It ran to its end; the delivery's status says what that means.
    Judged,
    /// A signal stopped it.
    Interrupted(i32),
}

/// Whether there is no reason to keep observing: the target either shows
/// this delivery healthy and converged, or shows something waiting will not
/// change (another desired state, no target, a failure).
fn settled(id: &str, request: &Request, observation: &Observation) -> bool {
    if !observation.reachable {
        return false;
    }
    if observation.present != Some(true) || !observation.shows(id, &request.intended) {
        return true;
    }
    let failed = observation
        .controller
        .as_ref()
        .is_some_and(|c| c.state == "failed")
        || observation.checks.iter().any(|check| !check.passed);
    let complete = observation
        .controller
        .as_ref()
        .is_some_and(|c| c.state == "complete");
    let healthy = complete
        && observation.observed.as_ref().is_some_and(|running| {
            running.converged
                && !running.artifacts.is_empty()
                && running
                    .artifacts
                    .iter()
                    .all(|artifact| *artifact == request.intended)
        })
        && observation
            .readiness
            .as_ref()
            .is_some_and(|r| r.wanted > 0 && r.ready >= r.wanted);
    failed || healthy
}

// ---------------------------------------------------------------------------
// Running the caller's commands.

/// The artifact, materialized from its store into `scratch`.
fn materialize(delivered: &Delivered, scratch: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(scratch).map_err(err)?;
    let store = Store::new(Path::new(&delivered.store))?;
    let path = scratch.join("artifact");
    store
        .available(&delivered.artifact)
        .and_then(|()| store.materialize(&delivered.artifact, &path))
        .map_err(|error| {
            let _ = fs::remove_dir_all(scratch);
            format!(
                "the artifact of production {} ({}) is not available in {}: {error}",
                delivered.production, delivered.artifact.content, delivered.store
            )
        })?;
    Ok(path)
}

/// One of a target's commands as a task: in the contract's directory, with
/// the declared env and inherit, and the `SYKLI_` variables.
fn command(id: &str, request: &Request, name: &str, step: &Step, extra: &[(&str, String)]) -> Task {
    let mut env = step.env.clone();
    env.insert("SYKLI_DELIVERY".into(), id.into());
    env.insert("SYKLI_DELIVERY_TARGET".into(), request.target.clone());
    env.insert("SYKLI_INTENDED".into(), request.intended.clone());
    for (name, value) in extra {
        env.insert((*name).into(), value.clone());
    }
    Task {
        name: name.into(),
        run: step.run.clone(),
        workdir: Some(PathBuf::from(&request.workdir)),
        env,
        after: vec![],
        inputs: vec![],
        outputs: vec![],
        runtime: None,
        inherit: step.inherit.clone(),
    }
}

enum Round {
    Stopped(i32),
    Done(Box<(Execution, Option<Observation>, Option<String>)>),
}

/// Run the target's observer once: what it reported, or why nothing it
/// printed is accepted.
fn observe_target(id: &str, request: &Request, runtime: &ShellRuntime) -> Result<Round, String> {
    let definition = request.definition()?;
    let task = command(id, request, "observe", &definition.observe, &[]);
    // Taken before the observer exists: the time its observation speaks from.
    let started = now();
    match crate::launch(&task, runtime, Progress::Quiet, OBSERVATION_LIMIT, None)? {
        Launched::Stopped { .. } => Ok(Round::Stopped(crate::runs::signalled().unwrap_or(15))),
        Launched::Ran { record, .. } => {
            let observer = Execution::of(&record, started);
            let (observation, error) = if !observer.passed() {
                let said = observer.stderr.trim();
                (
                    None,
                    Some(format!(
                        "the observer {}{}",
                        observer.ended(),
                        if said.is_empty() {
                            String::new()
                        } else {
                            format!(": {said}")
                        }
                    )),
                )
            } else if record.stdout_truncated {
                (
                    None,
                    Some(format!(
                        "the observer printed more than {OBSERVATION_LIMIT} bytes"
                    )),
                )
            } else {
                match Observation::parse(record.stdout.as_bytes()) {
                    Ok(observation) => (Some(observation), None),
                    Err(error) => (
                        None,
                        Some(format!(
                            "the observer printed no valid {OBSERVATION}: {error}"
                        )),
                    ),
                }
            };
            Ok(Round::Done(Box::new((observer, observation, error))))
        }
    }
}

/// Run the target's smoke command, when it declares one.
fn smoke_target(
    id: &str,
    request: &Request,
    runtime: &ShellRuntime,
) -> Result<Option<Result<Execution, i32>>, String> {
    let Some(step) = &request.definition()?.smoke else {
        return Ok(None);
    };
    let task = command(id, request, "smoke", step, &[]);
    let started = now();
    Ok(Some(
        match crate::launch(&task, runtime, Progress::Quiet, OBSERVATION_LIMIT, None)? {
            Launched::Stopped { .. } => Err(crate::runs::signalled().unwrap_or(15)),
            Launched::Ran { record, .. } => Ok(Execution::of(&record, started)),
        },
    ))
}

/// The identity the target should report for the artifact, before anything
/// is recorded: the artifact's SHA-256, or what the identity command prints.
fn identify(
    definition: &Target,
    delivered: &Delivered,
    workdir: &Path,
    common: &Path,
    runtime: &ShellRuntime,
) -> Result<(String, Identified), String> {
    let Some(step) = definition.identity_step() else {
        return Ok((
            format!("sha256:{}", delivered.artifact.content),
            Identified::Content,
        ));
    };
    let scratch = deliveries_directory(common).join(format!(
        ".identify-{}-{}",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    // Held while the command runs, so no sweep of leftovers takes it.
    fs::create_dir_all(&scratch).map_err(err)?;
    let _held = Lease::acquire(&scratch)?;
    let artifact = materialize(delivered, &scratch)?;
    let mut env = step.env.clone();
    env.insert(
        "SYKLI_ARTIFACT".into(),
        artifact.to_string_lossy().into_owned(),
    );
    let task = Task {
        name: "identity".into(),
        run: step.run.clone(),
        workdir: Some(workdir.to_path_buf()),
        env,
        after: vec![],
        inputs: vec![],
        outputs: vec![],
        runtime: None,
        inherit: step.inherit.clone(),
    };
    let launched = crate::launch(&task, runtime, Progress::Quiet, 4096, None);
    let _ = fs::remove_dir_all(&scratch);
    match launched? {
        Launched::Stopped { .. } => Err("stopped by a signal before anything was recorded".into()),
        Launched::Ran { record, .. } => {
            if record.exit_code != Some(0) {
                return Err(format!(
                    "the identity command failed ({}): {}",
                    record.error.as_deref().unwrap_or("no exit code"),
                    record.stderr.trim()
                ));
            }
            let intended = record.stdout.trim().to_string();
            identity_text(&intended).map_err(|e| format!("the identity command printed {e}"))?;
            Ok((
                intended,
                Identified::Command {
                    stdout_digest: record.stdout_digest.clone(),
                },
            ))
        }
    }
}

/// The product a delivery takes: the named production's, or the one the
/// current sources make (as `produce` would request it). Complete, with its
/// product available, or refused.
fn resolve_product(
    contract: &Contract,
    contract_path: &Path,
    definition: &Target,
    store_path: &Path,
    production: Option<&str>,
) -> Result<Delivered, String> {
    let store = Store::new(store_path)?;
    let wanted = &definition.artifact;
    let production_contract = contract_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(&contract.production);
    let id = match production {
        Some(id) => id.to_string(),
        None => super::request(&store, &production_contract, &wanted.target)?.id()?,
    };
    let request = super::Request::load(&store, &id).map_err(|error| {
        if production.is_none() {
            format!(
                "the current sources of production target {} are not produced (production {id}); \
                 run `sykli produce {} --contract {}` first ({error})",
                wanted.target,
                wanted.target,
                production_contract.display()
            )
        } else {
            error
        }
    })?;
    if request.target != wanted.target {
        return Err(format!(
            "production {id} is of target {}, not {}",
            request.target, wanted.target
        ));
    }
    let history = super::History::load(&store, &request)?;
    let view = super::view(&store, &request, &history, false)?;
    if view["delivery_success"] != true {
        return Err(format!(
            "production {id} is not complete with its artifact available; only a complete \
             production is delivered (`sykli status {id}`)"
        ));
    }
    let product = &view["delivery"][&wanted.product];
    let artifact: Artifact = serde_json::from_value(product["artifact"].clone())
        .map_err(|_| format!("production {id} has no product {}", wanted.product))?;
    Ok(Delivered {
        production: id,
        store: store.0.to_string_lossy().into_owned(),
        target: wanted.target.clone(),
        product: wanted.product.clone(),
        artifact,
    })
}

// ---------------------------------------------------------------------------
// Commands.

/// `sykli deliver`: request a delivery and carry it out (or hand it to a
/// detached coordinator).
pub(crate) enum Started {
    Ran(String, Done),
    Detached(String),
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn deliver(
    contract_path: &Path,
    target: &str,
    store: &Path,
    production: Option<&str>,
    rollback_to: Option<&str>,
    reapply_over: &[String],
    detach: bool,
    json: bool,
) -> Result<Started, String> {
    crate::runs::handle_signals();
    let contract = Contract::load(contract_path)?;
    let definition = contract
        .targets
        .get(target)
        .ok_or_else(|| {
            format!(
                "unknown delivery target {target} in {}; it declares {}",
                contract_path.display(),
                contract
                    .targets
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?
        .clone();
    let contract_digest = crate::sha256_file(contract_path)?;
    let contract_path = crate::absolute(contract_path)?;
    let workdir = contract_path
        .parent()
        .ok_or("the contract has no directory")?
        .to_path_buf();
    let common = common_dir()?;
    // One deliver per target at a time, from this check until the new
    // delivery is recorded and its lease held: two cannot both pass it. No
    // delivery is applied over another whose effect is unknown and was never
    // reconciled with the target (invariant H, across deliveries).
    let target_lock = target_lock(&common, target)?;
    let listed = list(&common);
    let applied_over = clear_to_apply(&listed, target, None, reapply_over)?;
    let previous = listed
        .iter()
        .filter(|d| d.request.target == target)
        .max_by_key(|d| d.request.target_sequence)
        .map(|d| d.id.clone());
    let runtime = crate::shell_runtime()?;
    let (artifact, intended, identified_by, rollback_of) = match rollback_to {
        Some(earlier) => {
            let earlier = listed
                .iter()
                .find(|d| d.id == earlier)
                .ok_or_else(|| unknown(earlier))?;
            if earlier.request.target != target {
                return Err(format!(
                    "delivery {} went to {}, not {target}",
                    earlier.id, earlier.request.target
                ));
            }
            // Its artifact and intended identity stand only for the same
            // product, identified the same way.
            let same_identity = matches!(
                (&earlier.request.identified_by, &definition.identity),
                (Identified::Content, Identity::Content)
                    | (Identified::Command { .. }, Identity::Command { .. })
            );
            if earlier.request.artifact.target != definition.artifact.target
                || earlier.request.artifact.product != definition.artifact.product
                || !same_identity
            {
                return Err(format!(
                    "delivery {} delivered product {} of {}, identified by {}; this contract's \
                     target {target} takes product {} of {}, identified by {}: deliver that \
                     production with --production instead",
                    earlier.id,
                    earlier.request.artifact.product,
                    earlier.request.artifact.target,
                    match earlier.request.identified_by {
                        Identified::Content => "content",
                        Identified::Command { .. } => "a command",
                    },
                    definition.artifact.product,
                    definition.artifact.target,
                    match definition.identity {
                        Identity::Content => "content",
                        Identity::Command { .. } => "a command",
                    },
                ));
            }
            let store = Store::new(Path::new(&earlier.request.artifact.store))?;
            store
                .available(&earlier.request.artifact.artifact)
                .map_err(|error| {
                    format!(
                        "delivery {}'s artifact is no longer available in {}: {error}",
                        earlier.id, earlier.request.artifact.store
                    )
                })?;
            (
                earlier.request.artifact.clone(),
                earlier.request.intended.clone(),
                earlier.request.identified_by.clone(),
                Some(earlier.id.clone()),
            )
        }
        None => {
            let artifact =
                resolve_product(&contract, &contract_path, &definition, store, production)?;
            let (intended, identified_by) =
                identify(&definition, &artifact, &workdir, &common, &runtime)?;
            (artifact, intended, identified_by, None)
        }
    };
    // Last, so a refusal above takes no place in the target's sequence.
    let target_sequence = next_in_sequence(&common, target)?;
    let request = Request {
        contract,
        contract_path: contract_path.to_string_lossy().into_owned(),
        contract_digest,
        target: target.into(),
        workdir: workdir.to_string_lossy().into_owned(),
        common_dir: common.to_string_lossy().into_owned(),
        artifact,
        intended,
        identified_by,
        rollback_of,
        previous,
        target_sequence,
        reapply_over: applied_over,
        runtime: runtime.fingerprint.clone(),
        requested_at_ms: now(),
        executor: EXECUTOR.into(),
        detached: detach,
        nonce: format!(
            "{}-{}-{}",
            std::process::id(),
            now(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ),
    };
    let coordinator = Coordinator::create(request)?;
    drop(target_lock);
    let progress = Progress::of(json);
    // First, so a caller whose deliver dies from here on knows what to resume.
    if !detach {
        progress.line("delivery", coordinator.id());
    }
    for over in &coordinator.request().reapply_over {
        eprintln!(
            "warning: applying over delivery {}, whose effect is unknown ({}); recorded in this \
             delivery's request",
            over.delivery, over.reason
        );
    }
    crate::checkpoint("after-request")?;
    if detach {
        return coordinator.detach().map(Started::Detached);
    }
    let done = coordinator.apply_and_observe(&runtime, progress, false)?;
    Ok(Started::Ran(coordinator.id().into(), done))
}

/// A detached coordinator (`deliver --detach`).
pub(crate) fn coordinator_main(id: &str, fd: i32) -> Result<Done, String> {
    crate::runs::handle_signals();
    let coordinator = Coordinator::received(id, fd)?;
    let runtime = crate::shell_runtime()?;
    crate::checkpoint("coordinator-start")?;
    coordinator.apply_and_observe(&runtime, Progress::Quiet, false)
}

/// `sykli resume --delivery`: take over a delivery whose coordinator ended,
/// reconcile whatever it left unknown with the target, and apply again only
/// when asked (`reapply`) and only after that reconciliation.
pub(crate) fn resume(
    id: &str,
    reapply: bool,
    assume_ended: bool,
    json: bool,
) -> Result<Done, String> {
    crate::runs::handle_signals();
    let (coordinator, assumed) = Coordinator::reopen(id, assume_ended)?;
    if reapply {
        match coordinator
            .last()
            .map(|attempt| (attempt.effect(), attempt.id))
        {
            None => {
                return Err(format!(
                    "delivery {id} was never dispatched, so nothing was applied; resume it \
                     without --reapply to dispatch it"
                ));
            }
            Some((Effect::Applied | Effect::Observed, attempt)) => {
                return Err(format!(
                    "attempt {attempt} of delivery {id} took effect; nothing is applied again \
                     (deliver again for a new apply)"
                ));
            }
            _ => {}
        }
    }
    // Whatever this resume dispatches is an apply to the target: never one
    // over another delivery's unknown, unreconciled effect (invariant H
    // across deliveries). This delivery's lease, held from here on, keeps
    // the target busy for any deliver that comes after the check.
    if reapply || coordinator.last().is_none() {
        let request = coordinator.request();
        let common = PathBuf::from(&request.common_dir);
        let _target = target_lock(&common, &request.target)?;
        clear_to_apply(&list(&common), &request.target, Some(id), &[])?;
    }
    let runtime = crate::shell_runtime()?;
    coordinator.take_over(&assumed)?;
    crate::checkpoint("after-lost")?;
    let progress = Progress::of(json);
    progress.line("delivery", id);
    let request = coordinator.request();
    let Some(last) = coordinator.last() else {
        // Never dispatched: nothing was applied, so this is no repeat.
        return coordinator.apply_and_observe(&runtime, progress, false);
    };
    let settle = request.definition()?.settle_seconds;
    let stopped = match last.effect() {
        Effect::Pending => unreachable!("take_over ends every unfinished attempt"),
        Effect::Applied | Effect::Observed => coordinator.settle(&runtime, settle)?,
        Effect::Refused if reapply => {
            return coordinator.apply_and_observe(&runtime, progress, true);
        }
        Effect::Refused => coordinator.settle(&runtime, 0)?,
        Effect::Unknown => {
            // Invariant H: ask the target before anything is applied again.
            let sequence = match coordinator.observe(&runtime, false)? {
                Ok(sequence) => sequence,
                Err(signal) => return Ok(Done::Interrupted(signal)),
            };
            let observation = {
                let history = coordinator.history.lock().unwrap();
                history
                    .seen
                    .iter()
                    .find(|seen| seen.sequence == sequence)
                    .cloned()
                    .expect("just recorded")
            };
            let (effect, reason) = reconcile(id, &request, &observation);
            coordinator.append(Fact::Reconciled {
                attempt: last.id.clone(),
                generation: coordinator.generation(),
                effect: effect.into(),
                observation: sequence,
                reason,
            })?;
            crate::checkpoint("after-reconcile")?;
            if effect == "observed" {
                coordinator.settle(&runtime, settle)?
            } else if reapply && observation.usable().is_none() {
                // Nothing about the target was learned: the lost apply may
                // have taken effect, and repeating it would be blind.
                return Err(format!(
                    "--reapply refused: the reconciliation could not reach the target ({}); \
                     nothing was applied again. Run `sykli resume --delivery {id} --reapply` \
                     once the target answers (`sykli status --delivery {id}`)",
                    observation.error.clone().unwrap_or_else(|| {
                        format!(
                            "the target's controller was unreachable{}",
                            observation
                                .observation
                                .as_ref()
                                .and_then(|o| o.reason.as_ref())
                                .map(|r| format!(": {r}"))
                                .unwrap_or_default()
                        )
                    })
                ));
            } else if reapply {
                return coordinator.apply_and_observe(&runtime, progress, true);
            } else {
                None
            }
        }
    };
    Ok(match stopped {
        Some(signal) => Done::Interrupted(signal),
        None => Done::Judged,
    })
}

/// What an observation says about an apply whose end is unknown.
fn reconcile(id: &str, request: &Request, seen: &Seen) -> (&'static str, String) {
    let Some(observation) = &seen.observation else {
        return (
            "unknown",
            format!(
                "the target could not be observed: {}; nothing was applied again, and nothing \
                 is until an observation reaches the target (`sykli resume --delivery {id}` once \
                 it answers)",
                seen.error.as_deref().unwrap_or("no observation")
            ),
        );
    };
    if observation.shows(id, &request.intended) {
        let desired = observation.desired.as_ref().unwrap();
        return (
            "observed",
            format!(
                "acknowledgement lost, effect observed: the target's desired state is this \
                 delivery's artifact {}{}{}",
                request.intended,
                desired
                    .revision
                    .as_ref()
                    .map(|r| format!(", revision {r}"))
                    .unwrap_or_default(),
                observation
                    .uid
                    .as_ref()
                    .map(|u| format!(", object {u}"))
                    .unwrap_or_default(),
            ),
        );
    }
    let why = if !observation.reachable {
        format!(
            "the target's controller was unreachable{}",
            observation
                .reason
                .as_ref()
                .map(|r| format!(": {r}"))
                .unwrap_or_default()
        )
    } else if observation.present != Some(true) {
        "the target is missing".into()
    } else if observation.desires(&request.intended)
        && observation
            .desired
            .as_ref()
            .is_some_and(|d| d.operation.is_none())
    {
        format!(
            "artifact present, not attributed to this delivery: the target's desired state is \
             {}, but names no operation, so it may be any delivery's",
            request.intended
        )
    } else {
        let desired = observation.desired.as_ref();
        format!(
            "the target's desired state is artifact {} of operation {}, not this delivery's {}",
            desired
                .and_then(|d| d.artifact.as_deref())
                .unwrap_or("(none)"),
            desired
                .and_then(|d| d.operation.as_deref())
                .unwrap_or("(none)"),
            request.intended
        )
    };
    let next = if observation.reachable {
        format!("`sykli resume --delivery {id} --reapply` applies again")
    } else {
        format!(
            "the target was not reached; nothing is applied again until an observation reaches \
             it (`sykli resume --delivery {id}` once it answers)"
        )
    };
    (
        "unknown",
        format!("effect unknown: {why}; nothing was applied again ({next})"),
    )
}

/// `sykli observe --delivery`: observe the target now and record it,
/// beside any coordinator. Never applies anything.
pub(crate) fn observe(id: &str) -> Result<Result<Value, i32>, String> {
    crate::runs::handle_signals();
    let common = common_dir()?;
    let directory = delivery_directory(&common, id)?;
    if !directory.join("records").is_dir() {
        return Err(unknown(id));
    }
    let request = {
        let _journal = Lease::journal_read(&directory)?;
        let history = History::load(&directory, id)?;
        if history.records.len() >= MAX_RECORDS {
            return Err(format!(
                "delivery {id} holds {MAX_RECORDS} records, its bound; deliver again to observe \
                 the target into a new delivery"
            ));
        }
        history.request().clone()
    };
    let runtime = crate::shell_runtime()?;
    let (observer, observation, error) = match observe_target(id, &request, &runtime)? {
        Round::Stopped(signal) => return Ok(Err(signal)),
        Round::Done(done) => *done,
    };
    let smoke = match smoke_target(id, &request, &runtime)? {
        Some(Err(signal)) => return Ok(Err(signal)),
        Some(Ok(execution)) => Some(execution),
        None => None,
    };
    {
        let _journal = Lease::journal(&directory)?;
        let mut history = History::load(&directory, id)?;
        let envelope = journal::seal(
            id.into(),
            &history.records,
            Fact::observed(None, observer, observation, error, smoke),
        )?;
        history.apply(&envelope.record)?;
        journal::write(&directory.join("records"), &envelope, Durability::Fsync)?;
    }
    status(id).map(Ok)
}

// ---------------------------------------------------------------------------
// Status: the dimensions, judged from the journal and the clock.

/// What sykli knows of one delivery: `sykli-delivery-status.v1`. Read from
/// its journal, its leases and the clock; nothing is written, no command runs.
pub(crate) fn status(id: &str) -> Result<Value, String> {
    let common = common_dir()?;
    let directory = delivery_directory(&common, id)?;
    if !directory.join("records").is_dir() {
        return Err(unknown(id));
    }
    let history = {
        let _journal = Lease::journal_read(&directory)?;
        History::load(&directory, id)?
    };
    let live = Lease::observe(&directory.join("lease"))?;
    let superseded = superseded_by(&common, id, &history);
    let known: BTreeMap<String, String> = list(&common)
        .into_iter()
        .map(|d| (d.id, d.request.intended))
        .collect();
    judge(&directory, &history, live, now(), superseded, &known)
}

fn judge(
    directory: &Path,
    history: &History,
    live: bool,
    now_ms: u64,
    superseded: Option<(String, u64)>,
    known: &BTreeMap<String, String>,
) -> Result<Value, String> {
    let id = &history.id;
    let request = history.request();
    let definition = request.definition()?;
    let intended = &request.intended;
    let last = history.last();

    // A: the operation.
    let (state, effect, detail) = match last {
        None if live => (
            "requested",
            "none",
            "requested; its apply is not dispatched yet".to_string(),
        ),
        None => (
            "requested",
            "none",
            format!(
                "not dispatched: nothing was applied; `sykli resume --delivery {id}` dispatches it"
            ),
        ),
        Some(attempt) => match (&attempt.end, attempt.effect()) {
            (None, _) if live || attempt_alive(directory, attempt)? => (
                if attempt.started.is_some() {
                    "applying"
                } else {
                    "dispatched"
                },
                "pending",
                "its apply is running".into(),
            ),
            (None, _) => (
                "lost",
                "unknown",
                format!(
                    "dispatched, but its coordinator ended without recording how the apply \
                     ended: whether it took effect is unknown; `sykli resume --delivery {id}` \
                     reconciles it with the target"
                ),
            ),
            (Some(End::Acknowledged { .. }), _) => (
                "acknowledged",
                "applied",
                "the apply exited 0: the target accepted the change (an acknowledgement, not a \
                 deployment)"
                    .into(),
            ),
            (
                Some(End::Failed {
                    exit_code, signal, ..
                }),
                Effect::Refused,
            ) => (
                "apply-failed",
                "refused",
                match (exit_code, signal) {
                    (Some(code), _) => {
                        format!("the apply exited {code}; the target may hold part of what it sent")
                    }
                    _ => "the apply could not be started; nothing was sent".into(),
                },
            ),
            (Some(end), effect) => {
                let how = match end {
                    End::Failed { signal, .. } => format!(
                        "the apply was ended by signal {} that sykli did not send",
                        signal.unwrap_or(0)
                    ),
                    End::Interrupted { signal, .. } => {
                        format!("sykli stopped the apply on signal {signal}")
                    }
                    End::Lost { reason, .. } => format!("the apply was lost: {reason}"),
                    End::Acknowledged { .. } => unreachable!(),
                };
                match (effect, attempt.reconciled.last()) {
                    // The reason says it: "acknowledgement lost, effect observed: …".
                    (Effect::Observed, Some(reconciled)) => (
                        "acknowledgement-lost",
                        "observed",
                        reconciled.reason.clone(),
                    ),
                    (_, Some(reconciled)) => (
                        "unknown",
                        "unknown",
                        format!("{how}; reconciled with the target: {}", reconciled.reason),
                    ),
                    _ => (
                        "unknown",
                        "unknown",
                        format!(
                            "{how}; whether it took effect is unknown until `sykli resume \
                             --delivery {id}` reconciles it with the target"
                        ),
                    ),
                }
            }
        },
    };
    let attempts: Vec<Value> = history
        .lineage
        .iter()
        .map(|attempt_id| {
            let attempt = &history.attempts[attempt_id];
            let (end, ended_at, exit_code, signal) = match &attempt.end {
                None => (None, None, None, None),
                Some(End::Acknowledged { at }) => (Some("acknowledged"), Some(*at), Some(0), None),
                Some(End::Failed {
                    at,
                    exit_code,
                    signal,
                }) => (Some("failed"), Some(*at), *exit_code, *signal),
                Some(End::Interrupted { at, signal }) => {
                    (Some("interrupted"), Some(*at), None, Some(*signal))
                }
                Some(End::Lost { at, .. }) => (Some("lost"), Some(*at), None, None),
            };
            json!({
                "id": attempt.id,
                "generation": attempt.generation,
                "supersedes": attempt.supersedes,
                "reapply": attempt.reapply,
                "dispatched_at_ms": attempt.dispatched_at,
                "started_at_ms": attempt.started.map(|(_, at)| at),
                "process_group": attempt.started.map(|(group, _)| group),
                "end": end,
                "ended_at_ms": ended_at,
                "exit_code": exit_code,
                "signal": signal,
                "reason": match &attempt.end { Some(End::Lost { reason, .. }) => Some(reason.clone()), _ => None },
                "reconciled": attempt.reconciled.iter().map(|r| json!({
                    "effect": r.effect, "observation": r.observation, "reason": r.reason, "at_ms": r.at,
                })).collect::<Vec<_>>(),
                "effect": match attempt.effect() {
                    Effect::Pending => "pending",
                    Effect::Applied => "applied",
                    Effect::Refused => "refused",
                    Effect::Unknown => "unknown",
                    Effect::Observed => "observed",
                },
            })
        })
        .collect();
    let mut operation = json!({
        "dimension": "A",
        "state": state,
        "effect": effect,
        "detail": detail,
        "coordinator_live": live,
        "superseded_by": superseded.as_ref().map(|(other, _)| other),
        "rollback_of": request.rollback_of,
        "attempts": attempts,
    });

    // Observations in the order their observers began, never the order their
    // records landed: one that began before an observation already recorded
    // may have read the target before that one did, so it is out of order
    // and decides nothing (it is kept, and listed).
    let (in_order, out_of_order) = by_start(&history.seen);
    // Which observations speak about this apply: recorded after its dispatch,
    // and begun once it had ended (`observable_from`), so never one begun
    // before its dispatch.
    let (since, from) = last.map_or((0, 0), |attempt| {
        (attempt.dispatched_sequence, attempt.observable_from())
    });
    let after: Vec<&Seen> = in_order
        .iter()
        .copied()
        .filter(|s| s.sequence > since && s.at() >= from)
        .collect();
    let latest = after.last().copied();
    let usable = after.iter().rev().find(|s| s.usable().is_some()).copied();
    let observation = usable.and_then(|s| s.usable());

    // Superseded, as this clone's journal says (`superseded`), or else as
    // the target says: its desired state names another operation that is no
    // delivery this clone keeps (another clone's, say). That one is seen
    // only by an observation, so only from the next one on.
    let superseded_seen = match (&superseded, observation, usable) {
        (None, Some(o), Some(seen)) => o
            .desired
            .as_ref()
            .and_then(|d| d.operation.as_ref())
            .filter(|op| op.as_str() != id.as_str() && !known.contains_key(op.as_str()))
            .map(|op| (op.clone(), seen.sequence)),
        _ => None,
    };
    operation["superseded_by"] = json!(
        superseded
            .as_ref()
            .map(|(other, _)| other)
            .or(superseded_seen.as_ref().map(|(op, _)| op))
    );
    operation["superseded_seen"] = json!(match (&superseded, &superseded_seen) {
        (Some(_), _) => Some("journal"),
        (None, Some(_)) => Some("observation"),
        _ => None,
    });

    // The target object as first observed after this apply: one with
    // another uid since was recreated, and this delivery never applied to it.
    let uid_after_apply = after
        .iter()
        .filter_map(|s| s.usable())
        .filter(|o| o.present == Some(true))
        .find_map(|o| o.uid.clone());
    let recreated = match (
        &uid_after_apply,
        observation
            .filter(|o| o.present == Some(true))
            .and_then(|o| o.uid.as_ref()),
    ) {
        (Some(first), Some(now)) if first != now => Some((first.clone(), now.clone())),
        _ => None,
    };

    // B: the artifact.
    let artifact = match observation {
        None => json!({"dimension": "B", "intended": intended, "verdict": "unknown"}),
        Some(o) => {
            let running: Vec<String> = o
                .observed
                .as_ref()
                .map(|r| {
                    r.artifacts
                        .iter()
                        .cloned()
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect()
                })
                .unwrap_or_default();
            let verdict = if o.present != Some(true) {
                "absent"
            } else if running.is_empty() {
                "none-running"
            } else if running.iter().all(|a| a == intended) {
                "match"
            } else if running.contains(intended) {
                "partial"
            } else {
                "mismatch"
            };
            json!({
                "dimension": "B",
                "intended": intended,
                "desired": o.desired.as_ref().and_then(|d| d.artifact.clone()),
                "running": running,
                "verdict": verdict,
            })
        }
    };

    // C: the desired state.
    let desired = match observation {
        None => json!({"dimension": "C", "verdict": "unknown"}),
        Some(o) if o.present != Some(true) => {
            json!({"dimension": "C", "verdict": "absent", "explanation": "the target is missing"})
        }
        Some(o) => {
            let d = o.desired.clone().unwrap_or(Desired {
                revision: None,
                artifact: None,
                operation: None,
                writer: None,
            });
            // Attributed only when the target names this delivery as the
            // operation behind its desired state: the artifact alone may be
            // any delivery's (the same artifact delivered again, say).
            let ours = o.shows(id, intended);
            let unattributed = !ours && o.desires(intended) && d.operation.is_none();
            let explanation = if ours {
                "the target's desired state is this delivery's".to_string()
            } else if unattributed {
                "the target's desired state is this delivery's artifact, but the target names no \
                 operation, so it is not attributed to this delivery"
                    .to_string()
            } else if let Some((other, _)) = &superseded {
                format!("superseded: delivery {other} to this target followed this one")
            } else if let Some((op, _)) = &superseded_seen {
                format!(
                    "superseded: the target's desired state is operation {op}'s, which is no \
                     delivery this clone keeps (another clone's delivery, or one this clone \
                     removed)"
                )
            } else if d.operation.as_deref() == Some(id.as_str()) {
                format!(
                    "modified out of band: the target names this delivery but asks for {}{}",
                    d.artifact.as_deref().unwrap_or("no artifact"),
                    d.writer
                        .as_ref()
                        .map(|w| format!(" (written by {w})"))
                        .unwrap_or_default()
                )
            } else if let Some((earlier, its)) = d
                .operation
                .as_ref()
                .and_then(|op| known.get(op).map(|its| (op, its)))
            {
                if d.artifact.as_ref() == Some(its) {
                    format!(
                        "rolled back out of band: the target asks again for delivery {earlier}'s \
                         artifact {its}{}",
                        d.writer
                            .as_ref()
                            .map(|w| format!(" (written by {w})"))
                            .unwrap_or_default()
                    )
                } else {
                    format!(
                        "modified out of band: the target names delivery {earlier} but asks for \
                         {}",
                        d.artifact.as_deref().unwrap_or("no artifact")
                    )
                }
            } else {
                format!(
                    "the target asks for {} of operation {}, not this delivery's {intended}{}",
                    d.artifact.as_deref().unwrap_or("no artifact"),
                    d.operation.as_deref().unwrap_or("(none)"),
                    d.writer
                        .as_ref()
                        .map(|w| format!(" (written by {w})"))
                        .unwrap_or_default()
                )
            };
            json!({
                "dimension": "C",
                "revision": d.revision,
                "artifact": d.artifact,
                "operation": d.operation,
                "writer": d.writer,
                "uid": o.uid,
                "uid_after_apply": uid_after_apply,
                "attributed": ours,
                "verdict": if ours {
                    "this-delivery"
                } else if unattributed {
                    "unattributed"
                } else {
                    "other"
                },
                "explanation": explanation,
            })
        }
    };

    // D: the observed state.
    let observed = match observation {
        None => json!({"dimension": "D", "verdict": "unknown"}),
        Some(o) if o.present != Some(true) => json!({"dimension": "D", "verdict": "absent"}),
        Some(o) => {
            let running = o.observed.clone().unwrap();
            let wanted = o.desired.as_ref().and_then(|d| d.revision.clone());
            let equal =
                running.converged && running.revision.is_some() && running.revision == wanted;
            // The controller is the authority on whether it is done: only its
            // own `complete` confirms what the observation says.
            let state = o.controller.as_ref().map(|c| c.state.as_str());
            json!({
                "dimension": "D",
                "revision": running.revision,
                "converged": running.converged,
                "revision_matches": equal,
                "controller": o.controller,
                "verdict": if state == Some("failed") {
                    "stalled"
                } else if equal && state == Some("complete") {
                    "equals-desired"
                } else if equal {
                    "unconfirmed"
                } else if !running.converged {
                    "converging"
                } else {
                    "differs"
                },
            })
        }
    };

    // E: health.
    let smoke_declared = definition.smoke.is_some();
    let health = match (observation, usable) {
        (Some(o), Some(seen)) if o.present == Some(true) => {
            let readiness = o.readiness.clone().unwrap();
            let ready = readiness.wanted > 0 && readiness.ready >= readiness.wanted;
            let failed_checks: Vec<&HealthCheck> = o.checks.iter().filter(|c| !c.passed).collect();
            let smoke = match (&seen.smoke, smoke_declared) {
                (_, false) => json!({"declared": false}),
                (None, true) => json!({"declared": true, "ran": false}),
                (Some(smoke), true) => json!({
                    "declared": true, "ran": true, "passed": smoke.passed(),
                    "exit_code": smoke.exit_code, "signal": smoke.signal,
                    "at_ms": smoke.started_at_ms, "stdout_digest": smoke.stdout_digest,
                    "stderr": smoke.stderr,
                }),
            };
            let smoke_failed = seen.smoke.as_ref().is_some_and(|s| !s.passed()) && smoke_declared;
            let smoke_passed =
                !smoke_declared || seen.smoke.as_ref().is_some_and(Execution::passed);
            let verdict = if !failed_checks.is_empty() || smoke_failed {
                "failed"
            } else if ready && smoke_passed {
                "passed"
            } else {
                "pending"
            };
            json!({
                "dimension": "E",
                "readiness": {"ready": readiness.ready, "wanted": readiness.wanted, "passed": ready},
                "checks": o.checks,
                "smoke": smoke,
                "verdict": verdict,
            })
        }
        (Some(_), _) => json!({"dimension": "E", "verdict": "absent"}),
        _ => json!({"dimension": "E", "verdict": "unknown"}),
    };

    // F: freshness.
    let bound_ms = definition.freshness_seconds.saturating_mul(1000);
    let mut changed_since = Vec::new();
    if let (Some((other, dispatched_at)), Some(seen)) = (&superseded, usable) {
        if *dispatched_at >= seen.at() {
            changed_since.push(format!(
                "delivery {other} dispatched an apply to this target at {}",
                iso(*dispatched_at)
            ));
        }
    }
    let freshness = match usable {
        None => json!({
            "dimension": "F",
            "bound_ms": bound_ms,
            "fresh": false,
            "verdict": "unobserved",
        }),
        Some(seen) => {
            // Dated ahead of now by more than the skew: the clock stepped
            // back since. Its age is unknown, never 0, and it is not fresh.
            let ahead = seen.at().saturating_sub(now_ms);
            let future = ahead > CLOCK_SKEW_MS;
            let age = now_ms.saturating_sub(seen.at());
            let fresh = !future && age <= bound_ms && changed_since.is_empty();
            json!({
                "dimension": "F",
                "observation": seen.sequence,
                "observed_at_ms": seen.at(),
                "observed_at": iso(seen.at()),
                "age_ms": if future { Value::Null } else { json!(age) },
                "ahead_ms": if future { json!(ahead) } else { Value::Null },
                "bound_ms": bound_ms,
                "changed_since": changed_since,
                "token": seen.observation.as_ref().and_then(|o| o.token.clone()),
                "fresh": fresh,
                "verdict": if future {
                    "future"
                } else if fresh {
                    "fresh"
                } else {
                    "stale"
                },
            })
        }
    };

    // The overall line, from the dimensions.
    let mut reasons: Vec<String> = Vec::new();
    let healthy_dims = |reasons: &mut Vec<String>| -> &'static str {
        let o = observation.unwrap();
        // A refused apply is said first, whatever else holds.
        if effect == "refused" {
            reasons.push(detail.clone());
        }
        if o.present != Some(true) {
            reasons.push("target missing: the observer found no target object".into());
            return DEGRADED;
        }
        if let Some((first, now)) = &recreated {
            reasons.push(format!(
                "target recreated: the object first observed after this delivery's apply was \
                 {first}; it is now {now}, which this delivery never applied to"
            ));
            return NOT_VERIFIED_HEALTHY;
        }
        if desired["verdict"] == "other" || desired["verdict"] == "unattributed" {
            reasons.push(desired["explanation"].as_str().unwrap_or("").into());
            return NOT_VERIFIED_HEALTHY;
        }
        let mut degraded = Vec::new();
        if let Some(controller) = o.controller.as_ref().filter(|c| c.state == "failed") {
            degraded.push(format!(
                "the controller reports failure{}",
                controller
                    .reason
                    .as_ref()
                    .map(|r| format!(": {r}"))
                    .unwrap_or_default()
            ));
        }
        for check in o.checks.iter().filter(|c| !c.passed) {
            degraded.push(format!(
                "check {} failed{}",
                check.name,
                check
                    .detail
                    .as_ref()
                    .map(|d| format!(": {d}"))
                    .unwrap_or_default()
            ));
        }
        if health["smoke"]["ran"] == true && health["smoke"]["passed"] == false {
            degraded.push("the smoke command failed".into());
        }
        if artifact["verdict"] != "match" && (!degraded.is_empty()) {
            degraded.push(format!(
                "intended {intended} runs {}",
                if artifact["verdict"] == "partial" {
                    "beside other artifacts"
                } else {
                    "nowhere"
                }
            ));
        }
        if !degraded.is_empty() {
            reasons.extend(degraded);
            return DEGRADED;
        }
        if effect == "refused" {
            return NOT_VERIFIED_HEALTHY;
        }
        let mut incomplete = Vec::new();
        if artifact["verdict"] != "match" {
            incomplete.push(format!(
                "artifact {}: running {}",
                artifact["verdict"].as_str().unwrap_or("?"),
                artifact["running"]
            ));
        }
        if observed["verdict"] != "equals-desired" {
            let state = observed["controller"]["state"]
                .as_str()
                .unwrap_or("no state");
            if state != "complete" {
                incomplete.push(format!(
                    "the controller reports {state}, not complete{}",
                    if observed["converged"] == true {
                        ", though the observation says converged"
                    } else {
                        ""
                    }
                ));
            }
            if observed["revision_matches"] != true {
                incomplete.push(format!(
                    "observed revision {} {} desired revision {}",
                    observed["revision"],
                    if observed["converged"] == true {
                        "differs from"
                    } else {
                        "is converging on"
                    },
                    desired["revision"]
                ));
            }
        }
        if health["verdict"] != "passed" {
            if health["readiness"]["passed"] != true {
                incomplete.push(format!(
                    "ready {} of {}",
                    health["readiness"]["ready"], health["readiness"]["wanted"]
                ));
            }
            if health["smoke"]["declared"] == true && health["smoke"]["ran"] != true {
                incomplete.push("the smoke command did not run with this observation".into());
            }
        }
        if !incomplete.is_empty() {
            reasons.push(format!("not converged: {}", incomplete.join("; ")));
            return NOT_VERIFIED_HEALTHY;
        }
        VERIFIED_HEALTHY
    };
    // Nothing applied yet, or an apply still running: nothing to judge.
    let label = if effect == "pending" || state == "requested" {
        reasons.push(detail.clone());
        NOT_VERIFIED_HEALTHY
    } else if effect == "unknown" {
        reasons.push(detail.clone());
        UNKNOWN
    } else if latest.is_none() {
        reasons.push("not observed since the apply: nothing is known of the target".into());
        UNKNOWN
    } else if let Some(failed) = latest.filter(|l| l.usable().is_none()) {
        reasons.push(format!(
            "the last observation, at {}, established nothing: {}",
            iso(failed.at()),
            failed
                .error
                .clone()
                .or_else(|| failed.observation.as_ref().map(|o| format!(
                    "the target's controller was unreachable{}",
                    o.reason
                        .as_ref()
                        .map(|r| format!(": {r}"))
                        .unwrap_or_default()
                )))
                .unwrap_or_default()
        ));
        if let Some(seen) = usable {
            reasons.push(match freshness["ahead_ms"].as_u64() {
                Some(ahead) => format!(
                    "the last usable observation, at {}, is dated {} s after now (the clock \
                     moved back)",
                    iso(seen.at()),
                    ahead / 1000
                ),
                None => format!(
                    "the last usable observation, at {}, is {} s old",
                    iso(seen.at()),
                    now_ms.saturating_sub(seen.at()) / 1000
                ),
            });
        }
        UNKNOWN
    } else if let Some((other, _)) = &superseded {
        reasons.push(format!(
            "superseded: delivery {other} to {} followed this one",
            request.target
        ));
        NOT_VERIFIED_HEALTHY
    } else if let Some((op, sequence)) = &superseded_seen {
        reasons.push(format!(
            "superseded: observation {sequence} shows {}'s desired state put there by operation \
             {op}, a delivery this clone has no record of (another clone's, or one it removed)",
            request.target
        ));
        NOT_VERIFIED_HEALTHY
    } else if freshness["fresh"] != true {
        let mut as_observed = Vec::new();
        let label = healthy_dims(&mut as_observed);
        if let Some(ahead) = freshness["ahead_ms"].as_u64() {
            reasons.push(format!(
                "the last observation, at {}, is dated {} s after now: this machine's clock \
                 moved back since, so how old it is is unknown, and it says nothing about now",
                freshness["observed_at"].as_str().unwrap_or("?"),
                ahead / 1000
            ));
        } else {
            reasons.push(format!(
                "the last observation, at {}, is {} s old; the bound is {} s{}",
                freshness["observed_at"].as_str().unwrap_or("?"),
                freshness["age_ms"].as_u64().unwrap_or(0) / 1000,
                definition.freshness_seconds,
                if changed_since.is_empty() {
                    String::new()
                } else {
                    format!("; since then: {}", changed_since.join("; "))
                }
            ));
        }
        reasons.push(format!("as last observed: {label}"));
        operation["as_last_observed"] = json!({"label": label, "reasons": as_observed});
        STALE
    } else {
        healthy_dims(&mut reasons)
    };

    let observations = json!({
        "count": history.seen.len(),
        "since_dispatch": after.len(),
        "latest": latest.map(seen_summary),
        "latest_usable": usable.map(|s| s.sequence),
        "out_of_order": out_of_order.iter().map(|s| s.sequence).collect::<Vec<_>>(),
        "recent": history.seen.iter().rev().take(20).map(seen_summary).collect::<Vec<_>>(),
    });
    Ok(json!({
        "schema": STATUS,
        "delivery": id,
        "target": request.target,
        "label": label,
        "reasons": reasons,
        "contract": request.contract_path,
        "contract_digest": request.contract_digest,
        "artifact": {
            "production": request.artifact.production,
            "store": request.artifact.store,
            "production_target": request.artifact.target,
            "product": request.artifact.product,
            "content": request.artifact.artifact.content,
            "type": request.artifact.artifact.ty,
        },
        "intended": intended,
        "identified_by": request.identified_by,
        "rollback_of": request.rollback_of,
        "previous": request.previous,
        "reapply_over": request.reapply_over,
        "requested_at_ms": request.requested_at_ms,
        "evaluated_at_ms": now_ms,
        "generation": history.generation,
        "coordinator_live": live,
        "dimensions": {
            "operation": operation,
            "artifact": artifact,
            "desired": desired,
            "observed": observed,
            "health": health,
            "freshness": freshness,
        },
        "observations": observations,
        "through_sequence": history.records.len(),
        "trust": TRUST,
    }))
}

/// The observations whose observers began no earlier than any observation
/// recorded before them, in journal order; and the rest, out of order. An
/// observer that began earlier may have read the target before a recorded
/// observation did, however late its own record landed, so it can never be
/// the latest word on the target.
fn by_start(seen: &[Seen]) -> (Vec<&Seen>, Vec<&Seen>) {
    let mut began = 0;
    let (mut in_order, mut out_of_order) = (Vec::new(), Vec::new());
    for s in seen {
        if s.at() < began {
            out_of_order.push(s);
        } else {
            began = s.at();
            in_order.push(s);
        }
    }
    (in_order, out_of_order)
}

fn seen_summary(seen: &Seen) -> Value {
    json!({
        "sequence": seen.sequence,
        "by": if seen.generation.is_some() { "coordinator" } else { "observe" },
        "observed_at_ms": seen.at(),
        "observed_at": iso(seen.at()),
        "observer_exit": seen.observer.exit_code,
        "stdout_digest": seen.observer.stdout_digest,
        "reachable": seen.observation.as_ref().map(|o| o.reachable),
        "present": seen.observation.as_ref().and_then(|o| o.present),
        "error": seen.error,
        "smoke_passed": seen.smoke.as_ref().map(Execution::passed),
    })
}

/// A time as UTC ISO 8601 (`2026-10-14T12:00:00Z`), from milliseconds.
fn iso(ms: u64) -> String {
    let seconds = ms / 1000;
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    // Civil from days (Howard Hinnant's algorithm), days since 1970-01-01.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// A delivery's status for a person: the overall line, why, and every
/// dimension, each led by its verdict.
pub(crate) fn describe(status: &Value) -> String {
    let d = &status["dimensions"];
    let text = |v: &Value| match v {
        Value::Null => "-".to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let mut lines = vec![format!(
        "delivery {} to {}{}: {}",
        text(&status["delivery"]),
        text(&status["target"]),
        status["rollback_of"]
            .as_str()
            .map(|of| format!(" (rollback of {of})"))
            .unwrap_or_default(),
        text(&status["label"])
    )];
    for reason in status["reasons"].as_array().into_iter().flatten() {
        lines.push(format!("  why: {}", text(reason)));
    }
    for over in status["reapply_over"].as_array().into_iter().flatten() {
        lines.push(format!(
            "  applied over delivery {} (--reapply-over), whose effect was unknown: {}",
            text(&over["delivery"]),
            text(&over["reason"])
        ));
    }
    let a = &d["operation"];
    lines.push(format!(
        "  A operation: {}, effect {} — {}",
        text(&a["state"]),
        text(&a["effect"]),
        text(&a["detail"])
    ));
    let b = &d["artifact"];
    lines.push(format!(
        "  B artifact:  {} — intended {}; running {}",
        text(&b["verdict"]),
        text(&b["intended"]),
        b["running"]
            .as_array()
            .map(|running| running.iter().map(&text).collect::<Vec<_>>().join(", "))
            .filter(|running| !running.is_empty())
            .unwrap_or_else(|| "nothing".into())
    ));
    let c = &d["desired"];
    lines.push(format!(
        "  C desired:   {} — revision {}, artifact {}, operation {}",
        text(&c["verdict"]),
        text(&c["revision"]),
        text(&c["artifact"]),
        if c["operation"] == status["delivery"] {
            "this delivery".to_string()
        } else {
            text(&c["operation"])
        }
    ));
    let o = &d["observed"];
    lines.push(format!(
        "  D observed:  {} — revision {}, converged {}, controller {}",
        text(&o["verdict"]),
        text(&o["revision"]),
        text(&o["converged"]),
        text(&o["controller"]["state"])
    ));
    let e = &d["health"];
    let checks = e["checks"].as_array().map_or(0, Vec::len);
    let passed = e["checks"]
        .as_array()
        .map_or(0, |c| c.iter().filter(|c| c["passed"] == true).count());
    lines.push(format!(
        "  E health:    {} — ready {}/{}; checks {passed}/{checks} passed; smoke {}",
        text(&e["verdict"]),
        text(&e["readiness"]["ready"]),
        text(&e["readiness"]["wanted"]),
        match (
            &e["smoke"]["declared"],
            &e["smoke"]["ran"],
            &e["smoke"]["passed"]
        ) {
            (Value::Bool(false), _, _) => "not declared".to_string(),
            (_, Value::Bool(true), Value::Bool(true)) => "passed".to_string(),
            (_, Value::Bool(true), _) => "failed".to_string(),
            (Value::Bool(true), _, _) => "not run".to_string(),
            _ => "-".to_string(),
        }
    ));
    let f = &d["freshness"];
    lines.push(match f["observed_at"].as_str() {
        Some(at) if f["verdict"] == "future" => format!(
            "  F freshness: future — observed {at}, {} s after now: the clock moved back (bound \
             {} s)",
            f["ahead_ms"].as_u64().unwrap_or(0) / 1000,
            f["bound_ms"].as_u64().unwrap_or(0) / 1000,
        ),
        Some(at) => format!(
            "  F freshness: {} — observed {at}, {} s ago (bound {} s){}",
            text(&f["verdict"]),
            f["age_ms"].as_u64().unwrap_or(0) / 1000,
            f["bound_ms"].as_u64().unwrap_or(0) / 1000,
            f["changed_since"]
                .as_array()
                .filter(|changes| !changes.is_empty())
                .map(|changes| format!(
                    "; changed since: {}",
                    changes.iter().map(&text).collect::<Vec<_>>().join("; ")
                ))
                .unwrap_or_default()
        ),
        None => "  F freshness: unobserved — never observed since the apply".to_string(),
    });
    if matches!(a["effect"].as_str(), Some("unknown"))
        || a["state"] == "requested" && a["coordinator_live"] != true
    {
        lines.push(format!(
            "reconcile it with: sykli resume --delivery {}",
            text(&status["delivery"])
        ));
    }
    lines.push("Use --json for every attempt and observation.".into());
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Retention.

/// Keep at most the delivery budget: remove the oldest settled deliveries
/// (`prunable`), never `keep`, never the newest to any target.
fn prune(common: &Path, keep: &str) {
    sweep_leftovers(common);
    let budget = std::env::var("SYKLI_DELIVERY_BUDGET")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(DEFAULT_BUDGET);
    if budget == 0 {
        return;
    }
    let listed: Vec<Listed> = list(common).into_iter().filter(|d| d.id != keep).collect();
    if listed.len() < budget {
        return;
    }
    let newest: BTreeSet<&str> = listed
        .iter()
        .fold(BTreeMap::new(), |mut newest: BTreeMap<&str, &Listed>, d| {
            let entry = newest.entry(d.request.target.as_str()).or_insert(d);
            if d.request.target_sequence > entry.request.target_sequence {
                *entry = d;
            }
            newest
        })
        .into_values()
        .map(|d| d.id.as_str())
        .collect();
    let over = listed.len() + 1 - budget;
    let candidates: Vec<&Listed> = listed
        .iter()
        .filter(|d| !newest.contains(d.id.as_str()))
        .filter(|d| !Lease::observe(&d.path.join("lease")).unwrap_or(true))
        .filter(|d| prunable(&d.path, &d.id))
        .take(over)
        .collect();
    for delivery in candidates {
        let Ok(Some(_lease)) = Lease::acquire_existing(&delivery.path) else {
            continue;
        };
        let aside = deliveries_directory(common).join(format!(
            ".pruning-{}-{}-{}",
            delivery.id,
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        if fs::rename(&delivery.path, &aside).is_ok() {
            let _ = fs::remove_dir_all(&aside);
        }
    }
}

/// Whether a delivery may be pruned: its journal reads, no apply of it is
/// running or unfinished, and none left an effect that is still unknown. A
/// delivery a resume still needs is the only record of a side effect that may
/// have happened, so it is kept past the budget.
fn prunable(directory: &Path, id: &str) -> bool {
    let Ok(_journal) = Lease::journal_read(directory) else {
        return false;
    };
    let Ok(history) = History::load(directory, id) else {
        return false;
    };
    history.attempts.values().all(|attempt| {
        attempt.end.is_some()
            && attempt.effect() != Effect::Unknown
            && !attempt_alive(directory, attempt).unwrap_or(true)
    })
}

/// What a creator or remover that died left behind, once no one holds it
/// and it is a minute old.
fn sweep_leftovers(common: &Path) {
    let Ok(entries) = fs::read_dir(deliveries_directory(common)) else {
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !(name.starts_with(".pruning-")
            || name.starts_with(".creating-")
            || name.starts_with(".identify-"))
        {
            continue;
        }
        let old = fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(60));
        if old && !Lease::observe(&path.join("lease")).unwrap_or(true) {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(run: &str) -> Step {
        Step {
            run: run.into(),
            env: BTreeMap::new(),
            inherit: vec![],
        }
    }

    fn request() -> Request {
        let target = Target {
            artifact: Product {
                target: "app".into(),
                product: "file".into(),
            },
            identity: Identity::Content,
            apply: step("true"),
            observe: step("true"),
            smoke: None,
            freshness_seconds: 60,
            settle_seconds: 0,
        };
        Request {
            contract: Contract {
                schema: CONTRACT.into(),
                production: "sykli.production.json".into(),
                targets: BTreeMap::from([("staging".to_string(), target)]),
            },
            contract_path: "/r/sykli.delivery.json".into(),
            contract_digest: "d".repeat(64),
            target: "staging".into(),
            workdir: "/r".into(),
            common_dir: "/r/.git".into(),
            artifact: Delivered {
                production: "p".repeat(64),
                store: "/r/.sykli/production".into(),
                target: "app".into(),
                product: "file".into(),
                artifact: Artifact {
                    content: "a".repeat(64),
                    ty: super::super::ArtifactType::File {
                        media_type: "application/octet-stream".into(),
                    },
                },
            },
            intended: format!("sha256:{}", "a".repeat(64)),
            identified_by: Identified::Content,
            rollback_of: None,
            previous: None,
            target_sequence: 1,
            reapply_over: Vec::new(),
            runtime: "shell".into(),
            requested_at_ms: 1,
            executor: EXECUTOR.into(),
            detached: false,
            nonce: "n".into(),
        }
    }

    fn id(c: char) -> String {
        c.to_string().repeat(64)
    }

    fn history(facts: Vec<Fact>) -> Result<History, String> {
        let request = request();
        let delivery = identity("sykli-delivery.v1", &request).unwrap();
        let mut history = History::new(&delivery);
        let mut all = vec![Fact::Requested {
            request: Box::new(RequestFact(request)),
        }];
        all.extend(facts);
        for fact in all {
            let envelope = journal::seal(delivery.clone(), &history.records, fact)?;
            history.apply(&envelope.record)?;
            history.records.push(envelope);
        }
        Ok(history)
    }

    fn delivery_id() -> String {
        identity("sykli-delivery.v1", &request()).unwrap()
    }

    fn dispatched(attempt: char, generation: u64, supersedes: Option<char>, reapply: bool) -> Fact {
        Fact::Dispatched {
            attempt: id(attempt),
            generation,
            supersedes: supersedes.map(id),
            reapply,
            // Attempts a, b, … come in that order in the target's sequence.
            target_sequence: u64::from(attempt),
        }
    }

    fn started(attempt: char, generation: u64) -> Fact {
        Fact::Started {
            attempt: id(attempt),
            generation,
            process_group: 7,
            started_at_ms: 2,
        }
    }

    /// A time after every record a test writes: observations are taken then.
    fn later() -> u64 {
        static LATER: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        *LATER.get_or_init(|| now() + 3_600_000)
    }

    fn execution() -> Execution {
        Execution {
            exit_code: Some(0),
            signal: None,
            started_at_ms: later(),
            ended_at_ms: later() + 1,
            stdout_digest: id('f'),
            stderr_digest: id('f'),
            stderr: String::new(),
            inherited_digests: BTreeMap::new(),
        }
    }

    fn observation(artifact: &str, operation: Option<&str>) -> Observation {
        Observation {
            schema: OBSERVATION.into(),
            reachable: true,
            present: Some(true),
            uid: Some("u1".into()),
            desired: Some(Desired {
                revision: Some("3".into()),
                artifact: Some(artifact.into()),
                operation: operation.map(str::to_string),
                writer: None,
            }),
            observed: Some(Running {
                revision: Some("3".into()),
                artifacts: vec![artifact.into()],
                converged: true,
            }),
            readiness: Some(Readiness {
                ready: 2,
                wanted: 2,
            }),
            checks: vec![],
            controller: Some(Controller {
                state: "complete".into(),
                reason: None,
                message: None,
            }),
            token: None,
            reason: None,
            detail: None,
        }
    }

    fn observed(observation: Option<Observation>, generation: Option<u64>) -> Fact {
        let error = observation.is_none().then(|| "unreachable".to_string());
        Fact::observed(generation, execution(), observation, error, None)
    }

    fn intended() -> String {
        format!("sha256:{}", "a".repeat(64))
    }

    /// Invariant H in the journal itself: a second apply is refused unless
    /// the caller asked for it and the first's effect is known not to be
    /// there, or was reconciled with the target and is still unknown.
    #[test]
    fn no_apply_runs_again_without_reapply_and_reconciliation() {
        let lost = vec![
            dispatched('a', 1, None, false),
            started('a', 1),
            Fact::Resumed { generation: 2 },
            Fact::Lost {
                attempt: id('a'),
                generation: 2,
                reason: "gone".into(),
            },
        ];
        // Not asked for: refused.
        let mut blind = lost.clone();
        blind.push(dispatched('b', 2, Some('a'), false));
        assert!(history(blind).err().unwrap().contains("not superseded"));
        // Asked for, but never reconciled with the target: refused.
        let mut unreconciled = lost.clone();
        unreconciled.push(dispatched('b', 2, Some('a'), true));
        assert!(
            history(unreconciled)
                .err()
                .unwrap()
                .contains("not superseded")
        );
        // Asked for, reconciled, but the observation never reached the
        // target (the observer failed, or the target was unreachable):
        // nothing was learned, so refused.
        let unreachable = Observation {
            reachable: false,
            present: None,
            uid: None,
            desired: None,
            observed: None,
            readiness: None,
            controller: None,
            reason: Some("connection refused".into()),
            ..observation(&intended(), None)
        };
        for blind in [None, Some(unreachable)] {
            let mut unreached = lost.clone();
            unreached.push(observed(blind, Some(2)));
            unreached.push(Fact::Reconciled {
                attempt: id('a'),
                generation: 2,
                effect: "unknown".into(),
                observation: 6,
                reason: "unreachable".into(),
            });
            unreached.push(dispatched('b', 2, Some('a'), true));
            assert!(history(unreached).err().unwrap().contains("not superseded"));
        }
        // Reconciled from an observation that reached the target and showed
        // something else, asked for: a new attempt that supersedes.
        let mut retried = lost.clone();
        retried.push(observed(Some(observation("sha256:other", None)), Some(2)));
        retried.push(Fact::Reconciled {
            attempt: id('a'),
            generation: 2,
            effect: "unknown".into(),
            observation: 6,
            reason: "another artifact".into(),
        });
        retried.push(dispatched('b', 2, Some('a'), true));
        let retried = history(retried).unwrap();
        assert_eq!(retried.lineage, vec![id('a'), id('b')]);
        // Acknowledged: never again, whatever the caller asks.
        let acknowledged = vec![
            dispatched('a', 1, None, false),
            started('a', 1),
            Fact::Acknowledged {
                attempt: id('a'),
                generation: 1,
                output: id('e'),
            },
            dispatched('b', 1, Some('a'), true),
        ];
        assert!(history(acknowledged).is_err());
        // Refused (exit 1) and asked for: allowed without reconciliation.
        let refused = vec![
            dispatched('a', 1, None, false),
            started('a', 1),
            Fact::Failed {
                attempt: id('a'),
                generation: 1,
                exit_code: Some(1),
                signal: None,
                output: id('e'),
            },
            dispatched('b', 1, Some('a'), true),
        ];
        assert!(history(refused).is_ok());
    }

    /// Recovery never creates a fact: an effect is recorded as observed only
    /// when the observation it cites shows this delivery's artifact as the
    /// target's desired state.
    #[test]
    fn an_effect_is_observed_only_from_an_observation_that_shows_it() {
        let delivery = delivery_id();
        let base = |observation: Option<Observation>| {
            vec![
                dispatched('a', 1, None, false),
                started('a', 1),
                Fact::Resumed { generation: 2 },
                Fact::Lost {
                    attempt: id('a'),
                    generation: 2,
                    reason: "gone".into(),
                },
                observed(observation, Some(2)),
            ]
        };
        let reconciled = |effect: &str| Fact::Reconciled {
            attempt: id('a'),
            generation: 2,
            effect: effect.into(),
            observation: 6,
            reason: "r".into(),
        };
        let mut shows = base(Some(observation(&intended(), Some(&delivery))));
        shows.push(reconciled("observed"));
        assert_eq!(
            history(shows).unwrap().attempts[&id('a')].effect(),
            Effect::Observed
        );
        for other in [
            Some(observation("sha256:other", Some(&delivery))),
            Some(observation(&intended(), Some(&id('z')))),
            // The artifact alone, no operation: not attributed to anyone.
            Some(observation(&intended(), None)),
            None,
        ] {
            let mut claims = base(other);
            claims.push(reconciled("observed"));
            assert!(
                history(claims).is_err(),
                "an unsupported effect was accepted"
            );
        }
        // An observation from before the dispatch says nothing of it.
        let early = vec![
            observed(Some(observation(&intended(), Some(&delivery))), Some(1)),
            dispatched('a', 1, None, false),
            started('a', 1),
            Fact::Resumed { generation: 2 },
            Fact::Lost {
                attempt: id('a'),
                generation: 2,
                reason: "gone".into(),
            },
            Fact::Reconciled {
                attempt: id('a'),
                generation: 2,
                effect: "observed".into(),
                observation: 2,
                reason: "r".into(),
            },
        ];
        assert!(history(early).is_err());
    }

    /// An apply has one end, a stale coordinator's end is refused, and an
    /// exit 0 is never a failure nor a start-less exit an acknowledgement.
    #[test]
    fn an_apply_has_one_end_and_a_stale_one_is_fenced_off() {
        let ended = vec![
            dispatched('a', 1, None, false),
            started('a', 1),
            Fact::Acknowledged {
                attempt: id('a'),
                generation: 1,
                output: id('e'),
            },
        ];
        let mut twice = ended.clone();
        twice.push(Fact::Failed {
            attempt: id('a'),
            generation: 1,
            exit_code: Some(1),
            signal: None,
            output: id('e'),
        });
        assert!(history(twice).is_err());
        let stale = vec![
            dispatched('a', 1, None, false),
            started('a', 1),
            Fact::Resumed { generation: 2 },
            Fact::Acknowledged {
                attempt: id('a'),
                generation: 1,
                output: id('e'),
            },
        ];
        assert!(history(stale).err().unwrap().contains("stale completion"));
        let unstarted = vec![
            dispatched('a', 1, None, false),
            Fact::Acknowledged {
                attempt: id('a'),
                generation: 1,
                output: id('e'),
            },
        ];
        assert!(history(unstarted).is_err());
        let zero = vec![
            dispatched('a', 1, None, false),
            started('a', 1),
            Fact::Failed {
                attempt: id('a'),
                generation: 1,
                exit_code: Some(0),
                signal: None,
                output: id('e'),
            },
        ];
        assert!(history(zero).is_err());
        // A stale coordinator's observation is refused; an outside one is not.
        let mut stale_observation = ended.clone();
        stale_observation.push(Fact::Resumed { generation: 2 });
        stale_observation.push(observed(None, Some(1)));
        assert!(history(stale_observation).is_err());
        let mut outside = ended;
        outside.push(Fact::Resumed { generation: 2 });
        outside.push(observed(None, None));
        assert!(history(outside).is_ok());
    }

    /// Judged `after` ms past the observations' time.
    fn judged(facts: Vec<Fact>, after: u64, superseded: Option<(String, u64)>) -> Value {
        let now_ms = later() + after;
        let history = history(facts).unwrap();
        let directory = std::env::temp_dir().join("sykli-delivery-judge-nothing-here");
        judge(
            &directory,
            &history,
            false,
            now_ms,
            superseded,
            &BTreeMap::new(),
        )
        .unwrap()
    }

    fn acknowledged() -> Vec<Fact> {
        vec![
            dispatched('a', 1, None, false),
            started('a', 1),
            Fact::Acknowledged {
                attempt: id('a'),
                generation: 1,
                output: id('e'),
            },
        ]
    }

    /// The overall line follows the dimensions: healthy only when every one
    /// holds, and each way of not holding has its own label.
    #[test]
    fn the_overall_label_follows_every_dimension() {
        let delivery = delivery_id();
        let healthy = observation(&intended(), Some(&delivery));
        let with = |o: Observation| {
            let mut facts = acknowledged();
            facts.push(observed(Some(o), Some(1)));
            facts
        };
        let label = |v: &Value| v["label"].as_str().unwrap().to_string();
        assert_eq!(
            label(&judged(with(healthy.clone()), 20, None)),
            VERIFIED_HEALTHY
        );
        // F: older than the bound.
        let stale = judged(with(healthy.clone()), 61_000, None);
        assert_eq!(label(&stale), STALE);
        assert_eq!(
            stale["dimensions"]["operation"]["as_last_observed"]["label"],
            VERIFIED_HEALTHY
        );
        // B, D: rollout incomplete.
        let mut converging = healthy.clone();
        converging.observed = Some(Running {
            revision: Some("2".into()),
            artifacts: vec![intended(), "sha256:old".into()],
            converged: false,
        });
        let v = judged(with(converging), 20, None);
        assert_eq!(label(&v), NOT_VERIFIED_HEALTHY);
        assert_eq!(v["dimensions"]["artifact"]["verdict"], "partial");
        assert_eq!(v["dimensions"]["observed"]["verdict"], "converging");
        // E: a failing check is degraded, even with Available and an old
        // artifact serving.
        let mut pulling = healthy.clone();
        pulling.observed = Some(Running {
            revision: Some("2".into()),
            artifacts: vec!["sha256:old".into()],
            converged: false,
        });
        pulling.checks = vec![HealthCheck {
            name: "pull".into(),
            passed: false,
            detail: Some("ImagePullBackOff".into()),
        }];
        let v = judged(with(pulling), 20, None);
        assert_eq!(label(&v), DEGRADED);
        assert_eq!(v["dimensions"]["artifact"]["verdict"], "mismatch");
        // C: modified out of band.
        let mut manual = healthy.clone();
        manual.desired.as_mut().unwrap().artifact = Some("sha256:manual".into());
        manual.desired.as_mut().unwrap().writer = Some("kubectl-set".into());
        let v = judged(with(manual), 20, None);
        assert_eq!(label(&v), NOT_VERIFIED_HEALTHY);
        assert!(
            v["reasons"][0]
                .as_str()
                .unwrap()
                .contains("modified out of band")
        );
        // Target missing.
        let missing = Observation {
            present: Some(false),
            desired: None,
            observed: None,
            readiness: None,
            controller: None,
            ..healthy.clone()
        };
        assert_eq!(label(&judged(with(missing), 20, None)), DEGRADED);
        // Unreachable after a healthy one: unknown, not healthy.
        let unreachable = Observation {
            reachable: false,
            present: None,
            desired: None,
            observed: None,
            readiness: None,
            controller: None,
            reason: Some("connection refused".into()),
            ..healthy.clone()
        };
        let mut facts = with(healthy.clone());
        facts.push(observed(Some(unreachable), None));
        assert_eq!(label(&judged(facts, 20, None)), UNKNOWN);
        // Superseded.
        assert_eq!(
            label(&judged(
                with(healthy.clone()),
                20,
                Some((id('s'), later() + 5))
            )),
            NOT_VERIFIED_HEALTHY
        );
        // No observation since the apply.
        assert_eq!(label(&judged(acknowledged(), 20, None)), UNKNOWN);
    }

    /// Invariant H: an apply that started ends with an exit or a signal. One
    /// whose end could not be established is never a refusal (which may be
    /// applied again on --reapply): it keeps no end, and a later coordinator
    /// finds it lost, its effect unknown.
    #[test]
    fn a_started_apply_never_ends_as_refused_without_an_exit() {
        let unknown_end = Fact::Failed {
            attempt: id('a'),
            generation: 1,
            exit_code: None,
            signal: None,
            output: id('e'),
        };
        let started_then = vec![
            dispatched('a', 1, None, false),
            started('a', 1),
            unknown_end.clone(),
        ];
        assert!(history(started_then).is_err());
        // Never started (its shell could not be spawned): nothing was sent.
        let unstarted = vec![dispatched('a', 1, None, false), unknown_end];
        assert_eq!(
            history(unstarted).unwrap().attempts[&id('a')].effect(),
            Effect::Refused
        );
    }

    /// Invariant B: an observation is evidence about an apply only when it
    /// began after the apply ended, whatever its place in the journal. An
    /// observer started before the dispatch and recorded after it (`sykli
    /// observe`, beside the coordinator) reports the state before.
    #[test]
    fn an_observation_begun_before_the_apply_ended_is_no_evidence() {
        let delivery = delivery_id();
        let mut early = execution();
        early.started_at_ms = 10;
        let mut facts = acknowledged();
        facts.push(Fact::observed(
            None,
            early.clone(),
            Some(observation(&intended(), None)),
            None,
            None,
        ));
        let v = judged(facts, 20, None);
        assert_eq!(v["label"], UNKNOWN, "{v}");
        // Nor can a reconciliation cite one.
        let cited = vec![
            dispatched('a', 1, None, false),
            started('a', 1),
            Fact::Resumed { generation: 2 },
            Fact::Lost {
                attempt: id('a'),
                generation: 2,
                reason: "gone".into(),
            },
            Fact::observed(
                Some(2),
                early,
                Some(observation(&intended(), Some(&delivery))),
                None,
                None,
            ),
            Fact::Reconciled {
                attempt: id('a'),
                generation: 2,
                effect: "observed".into(),
                observation: 6,
                reason: "r".into(),
            },
        ];
        assert!(history(cited).is_err());
    }

    /// Observations are taken in the order their observers began, never in
    /// the order their records landed: an observer that read the target
    /// before a newer observation was recorded, and finished after it, never
    /// becomes the latest or the usable one.
    #[test]
    fn an_observation_begun_before_one_already_recorded_never_decides() {
        let delivery = delivery_id();
        let healthy = observation(&intended(), Some(&delivery));
        let mut modified = healthy.clone();
        modified.desired.as_mut().unwrap().artifact = Some("sha256:manual".into());
        let early = execution();
        let mut late = execution();
        late.started_at_ms = later() + 800;
        let mut facts = acknowledged();
        // Records 5 (began later, recorded first) and 6 (began first).
        facts.push(Fact::observed(None, late, Some(modified), None, None));
        facts.push(Fact::observed(None, early, Some(healthy), None, None));
        let v = judged(facts, 2_000, None);
        assert_eq!(v["label"], NOT_VERIFIED_HEALTHY, "{v}");
        assert_eq!(v["observations"]["latest"]["sequence"], 5, "{v}");
        assert_eq!(v["observations"]["latest_usable"], 5, "{v}");
        assert_eq!(v["observations"]["out_of_order"], json!([6]), "{v}");
        assert!(
            v["reasons"].to_string().contains("modified out of band"),
            "{v}"
        );
    }

    /// Only the controller's own `complete` counts toward VERIFIED HEALTHY:
    /// an observation that says converged while its controller says it is
    /// still working, or does not know, contradicts itself.
    #[test]
    fn only_a_complete_controller_counts_toward_verified() {
        let delivery = delivery_id();
        for state in ["pending", "progressing", "unknown"] {
            let mut o = observation(&intended(), Some(&delivery));
            o.controller.as_mut().unwrap().state = state.into();
            let mut facts = acknowledged();
            facts.push(observed(Some(o), Some(1)));
            let v = judged(facts, 20, None);
            assert_eq!(v["label"], NOT_VERIFIED_HEALTHY, "{state}: {v}");
            assert!(
                v["reasons"]
                    .to_string()
                    .contains(&format!("the controller reports {state}, not complete")),
                "{state}: {v}"
            );
            assert_ne!(
                v["dimensions"]["observed"]["verdict"], "equals-desired",
                "{state}: {v}"
            );
        }
        let mut failed = observation(&intended(), Some(&delivery));
        failed.controller.as_mut().unwrap().state = "failed".into();
        let mut facts = acknowledged();
        facts.push(observed(Some(failed), Some(1)));
        assert_eq!(judged(facts, 20, None)["label"], DEGRADED);
    }

    /// After the clock steps back, an observation is dated in the future. It
    /// is never fresh and never age 0: it says nothing about now. Within the
    /// documented skew it is taken as now.
    #[test]
    fn a_future_dated_observation_is_never_fresh() {
        let delivery = delivery_id();
        let mut facts = acknowledged();
        facts.push(observed(
            Some(observation(&intended(), Some(&delivery))),
            Some(1),
        ));
        let history = history(facts).unwrap();
        let at = |now_ms: u64| {
            judge(
                &std::env::temp_dir().join("sykli-delivery-judge-nothing-here"),
                &history,
                false,
                now_ms,
                None,
                &BTreeMap::new(),
            )
            .unwrap()
        };
        // Judged 50 minutes before the observation's own time.
        let v = at(later() - 3_000_000);
        assert_eq!(v["label"], STALE, "{v}");
        let freshness = &v["dimensions"]["freshness"];
        assert_eq!(freshness["fresh"], false, "{v}");
        assert_eq!(freshness["verdict"], "future", "{v}");
        assert_ne!(freshness["age_ms"], 0, "{v}");
        assert_eq!(freshness["ahead_ms"], 3_000_000, "{v}");
        assert!(v["reasons"].to_string().contains("clock"), "{v}");
        // Within the skew, it is now.
        let v = at(later() - CLOCK_SKEW_MS);
        assert_eq!(v["label"], VERIFIED_HEALTHY, "{v}");
        assert_eq!(v["dimensions"]["freshness"]["age_ms"], 0, "{v}");
    }

    /// Which delivery superseded which follows the target's own sequence,
    /// taken under its lock, never the clock: a delivery requested after a
    /// backward clock step, so with an earlier `requested_at_ms`, still
    /// supersedes the one whose apply came before it.
    #[test]
    fn superseding_follows_the_targets_sequence_not_the_clock() {
        let common = std::env::temp_dir().join(format!(
            "sykli-delivery-sequence-{}-{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&common).unwrap();
        let deliver = |requested_at_ms: u64| {
            let _lock = target_lock(&common, "staging").unwrap();
            let mut request = request();
            request.common_dir = common.to_string_lossy().into_owned();
            request.requested_at_ms = requested_at_ms;
            request.target_sequence = next_in_sequence(&common, "staging").unwrap();
            drop(_lock);
            let coordinator = Coordinator::create(request).unwrap();
            let attempt = coordinator.dispatch(false).unwrap();
            drop(attempt);
            coordinator.id().to_string()
        };
        let first = deliver(1_000_000);
        // The clock stepped back: the second is requested "earlier".
        let second = deliver(5_000);
        let load = |id: &str| History::load(&delivery_directory(&common, id).unwrap(), id).unwrap();
        assert_eq!(
            superseded_by(&common, &first, &load(&first)).map(|(by, _)| by),
            Some(second.clone())
        );
        assert_eq!(superseded_by(&common, &second, &load(&second)), None);
        let _ = fs::remove_dir_all(&common);
    }

    #[test]
    fn times_are_utc() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_791_931_644_000), "2026-10-13T22:47:24Z");
        assert_eq!(iso(951_782_400_000), "2000-02-29T00:00:00Z");
    }
}
