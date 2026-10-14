//! Durable graph runs (Unix). A run's lifecycle is journaled before and as
//! it happens, so a crash, a lost coordinator or a signal leaves a record of
//! exactly what is known about each task, and nothing more.
//!
//! A run lives in `<git-common-dir>/sykli/runs/<run-id>/`, beside the refs
//! and objects every worktree of the clone shares, so it outlives the
//! worktree that started it:
//!
//! * `records/<sequence>.json`: the journal (`sykli-run-record.v1`), hash
//!   chained, each record synced before it is acknowledged (`journal`).
//! * `outputs/<sha256>.json`: each attempt's task record (exit, captured
//!   output, digests), synced, named by the SHA-256 of its bytes; a
//!   `finished` record cites one, and only a cited one is accepted.
//! * `lease`: the coordinator's lease; `attempt-leases/<attempt>`: held by
//!   an attempt's shell and everything it starts, so an orphan outliving a
//!   killed coordinator is seen as running, never as finished. A process
//!   that closes its descriptor is still seen through the process group the
//!   journal recorded for the attempt, while that group exists.
//! * `coordinator.log`: a detached coordinator's own errors.
//!
//! The first record, `requested`, is durable before any task is dispatched
//! (it holds the contract as run, the subject, the runtime and the task
//! list; the run id is its identity). Each task then gets either an accepted
//! outcome without running (`reused` from the cache, `settled` as blocked or
//! errored) or attempts: `dispatched` before its shell is spawned, `started`
//! once it runs, and exactly one end: `finished` (exit or signal, its
//! output, the accepted outcome), `interrupted` (sykli stopped it on a
//! signal) or `lost` (a later coordinator found it unfinished). A retry is a
//! new attempt that `supersedes` the last; nothing is ever rewritten. A
//! pass accepted just before a stop reached sykli is `withdrawn` (the same
//! stop may have reached the attempt first), with every pass decided after
//! it: its task is undecided again, and the attempt stays in the history.
//! The run ends `concluded` (its receipt) or `stopped` (interrupted).
//!
//! Every coordinator that takes the run's lease opens a generation
//! (`requested` is 1, each `resumed` the next), and every fact it records
//! carries it. A fact from an older generation is refused when it is
//! appended, so a stale coordinator can never complete an attempt or
//! conclude a run that a newer one has taken over, and an attempt has at
//! most one end. The receipt is built only from accepted outcomes.
//!
//! Retention: at most `SYKLI_RUN_BUDGET` runs (default 100; 0 keeps every
//! run) are kept per clone. When a run is requested beyond that, the oldest
//! runs whose coordinator is not alive are removed, concluded ones first,
//! each leaving a tombstone in `pruned-runs/<run-id>.json` (its state, tree
//! and receipt), at most 1000, the oldest removed first. A run is
//! created aside and renamed into place with its lease held, and a remover
//! takes only an existing lease and moves the run aside before deleting it,
//! so concurrent runs never prune a run being created or leave half of one.
use crate::canonical::{Durability, NONCE, canonical, decode, digest, identity, now, publish_with};
use crate::journal::{self, Envelope, Lease};
use crate::{InputRecords, Outcome, Subject, TaskReceipt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const RECORD: &str = "sykli-run-record.v1";
const EXECUTOR: &str = "local-shell.v1";
/// What a run's status rests on.
pub(crate) const TRUST: &str = "trusted-local-coordinator-and-journal; no authenticity claim";
const DEFAULT_RUN_BUDGET: usize = 100;
/// Tombstones of pruned runs kept per clone (`pruned-runs/`), each a few
/// hundred bytes.
const TOMBSTONE_BUDGET: usize = 1000;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// A receipt's subject as a run's journal keeps it, inputs and HEAD's commit
/// included, so a later coordinator can write the receipt this run began.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubjectRecord {
    pub(crate) repository: String,
    pub(crate) tree_oid: String,
    pub(crate) inputs_digest: String,
    head_tree_oid: String,
    dirty: bool,
    submodules_modified: bool,
    head_commit: Option<String>,
    inputs: InputRecords,
}

impl From<&Subject> for SubjectRecord {
    fn from(subject: &Subject) -> Self {
        SubjectRecord {
            repository: subject.repository.clone(),
            tree_oid: subject.tree_oid.clone(),
            inputs_digest: subject.inputs_digest.clone(),
            head_tree_oid: subject.head_tree_oid.clone(),
            dirty: subject.dirty,
            submodules_modified: subject.submodules_modified,
            head_commit: subject.head_commit.clone(),
            inputs: subject.inputs.clone(),
        }
    }
}

impl From<&SubjectRecord> for Subject {
    fn from(record: &SubjectRecord) -> Self {
        Subject {
            repository: record.repository.clone(),
            tree_oid: record.tree_oid.clone(),
            inputs_digest: record.inputs_digest.clone(),
            head_tree_oid: record.head_tree_oid.clone(),
            dirty: record.dirty,
            submodules_modified: record.submodules_modified,
            inputs: record.inputs.clone(),
            head_commit: record.head_commit.clone(),
        }
    }
}

/// What a run was asked to do; its identity is the run id.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    /// The contract as it ran, canonical JSON whose SHA-256 is `contract_hash`.
    pub(crate) contract: String,
    /// Where it was read from, absolute.
    pub(crate) contract_path: String,
    pub(crate) contract_hash: String,
    pub(crate) lock_digest: Option<String>,
    /// The directory declared paths resolve from.
    pub(crate) workdir: String,
    pub(crate) common_dir: String,
    pub(crate) subject: SubjectRecord,
    pub(crate) tasks: Vec<String>,
    pub(crate) runtime: String,
    pub(crate) started_at_ms: u64,
    pub(crate) executor: String,
    pub(crate) detached: bool,
    pub(crate) nonce: String,
}

impl Request {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        contract: String,
        contract_path: &Path,
        contract_hash: String,
        lock_digest: Option<String>,
        subject: &Subject,
        tasks: Vec<String>,
        runtime: String,
        started_at_ms: u64,
        detached: bool,
    ) -> Result<Self, String> {
        let repository = crate::repo::repository()?;
        Ok(Request {
            contract,
            contract_path: crate::absolute(contract_path)?.to_string_lossy().into(),
            contract_hash,
            lock_digest,
            workdir: std::env::current_dir()
                .map_err(err)?
                .to_string_lossy()
                .into(),
            common_dir: crate::absolute(&repository.common_dir)?
                .to_string_lossy()
                .into(),
            subject: subject.into(),
            tasks,
            runtime,
            started_at_ms,
            executor: EXECUTOR.into(),
            detached,
            nonce: format!(
                "{}-{}-{}",
                std::process::id(),
                now(),
                NONCE.fetch_add(1, Ordering::Relaxed)
            ),
        })
    }

    pub(crate) fn subject(&self) -> Subject {
        (&self.subject).into()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum Fact {
    Requested {
        request: Box<RequestFact>,
    },
    Resumed {
        generation: u64,
    },
    Dispatched {
        attempt: String,
        task: String,
        generation: u64,
        cache_key: String,
        supersedes: Option<String>,
        executor: String,
    },
    Started {
        attempt: String,
        generation: u64,
        process_group: i32,
        started_at_ms: u64,
    },
    Finished {
        attempt: String,
        generation: u64,
        exit_code: Option<i32>,
        signal: Option<i32>,
        outcome: Outcome,
        output: String,
    },
    Reused {
        task: String,
        generation: u64,
        cache_key: String,
        provenance: String,
        output: String,
    },
    Settled {
        task: String,
        generation: u64,
        cache_key: Option<String>,
        outcome: Outcome,
        output: String,
    },
    /// sykli's stop signalled the attempt's group. `signal` is what stopped
    /// the run; `exit_code` or `ended_by` is how the attempt's shell then
    /// ended (absent in records written before they were kept).
    Interrupted {
        attempt: String,
        generation: u64,
        signal: i32,
        #[serde(default)]
        exit_code: Option<i32>,
        #[serde(default)]
        ended_by: Option<i32>,
    },
    Lost {
        attempt: String,
        generation: u64,
        reason: String,
    },
    /// A pass this generation accepted is withdrawn: its attempt ended so
    /// close before a stop reached sykli that the stop may have ended it (a
    /// task the same signal reached first, which trapped it and exited 0);
    /// or, with no attempt, a task reused from the cache after such a pass,
    /// which may rest on it. The task is undecided again, and an attempt
    /// stays in its history. A failure is never withdrawn.
    Withdrawn {
        task: String,
        attempt: Option<String>,
        generation: u64,
        reason: String,
    },
    Stopped {
        generation: u64,
        signal: Option<i32>,
        reason: String,
    },
    Concluded {
        generation: u64,
        receipt: String,
        digest: String,
        outcome: Outcome,
    },
}

/// `Request` as a fact; `PartialEq` for the duplicate check.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct RequestFact(Request);

impl PartialEq for RequestFact {
    fn eq(&self, other: &Self) -> bool {
        canonical(&self.0).ok() == canonical(&other.0).ok()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    schema: String,
    run: String,
    sequence: u64,
    previous: Option<String>,
    recorded_at: u64,
    fact: Fact,
}

impl journal::Record for Record {
    type Fact = Fact;
    const SCHEMA: &'static str = RECORD;
    const OWNER: &'static str = "run";
    fn new(
        run: String,
        sequence: u64,
        previous: Option<String>,
        recorded_at: u64,
        fact: Fact,
    ) -> Self {
        Record {
            schema: RECORD.into(),
            run,
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
        &self.run
    }
    fn sequence(&self) -> u64 {
        self.sequence
    }
    fn previous(&self) -> Option<&str> {
        self.previous.as_deref()
    }
}

/// How one attempt ended.
#[derive(Clone, Debug)]
enum End {
    Finished {
        exit_code: Option<i32>,
        signal: Option<i32>,
        outcome: Outcome,
        at: u64,
    },
    Interrupted {
        signal: i32,
        exit_code: Option<i32>,
        ended_by: Option<i32>,
        at: u64,
    },
    Lost {
        at: u64,
        reason: String,
    },
}

#[derive(Clone, Debug)]
struct AttemptState {
    id: String,
    task: String,
    generation: u64,
    cache_key: String,
    supersedes: Option<String>,
    started: Option<(i32, u64)>,
    end: Option<(End, Fact)>,
    output: Option<String>,
    /// Why its accepted pass was withdrawn (`Withdrawn`), if it was.
    withdrawn: Option<String>,
}

/// How a task's outcome was accepted.
#[derive(Clone, Debug)]
enum Decided {
    /// By its attempt's `finished` record.
    Attempt(String),
    /// From the cache, or without running (blocked, errored); the output and
    /// the cache key it was decided under.
    Without {
        outcome: Outcome,
        output: String,
        cache_key: Option<String>,
        provenance: Option<String>,
        generation: u64,
    },
}

/// A run's journal, validated: what is known, and nothing else.
pub(crate) struct History {
    run: String,
    records: Vec<Envelope<Record>>,
    request: Option<Request>,
    generation: u64,
    attempts: BTreeMap<String, AttemptState>,
    /// Each task's attempts, oldest first.
    lineage: BTreeMap<String, Vec<String>>,
    decided: BTreeMap<String, Decided>,
    stopped: Option<(Option<i32>, String)>,
    concluded: Option<(String, String, Outcome)>,
}

impl History {
    fn new(run: &str) -> Self {
        History {
            run: run.into(),
            records: Vec::new(),
            request: None,
            generation: 0,
            attempts: BTreeMap::new(),
            lineage: BTreeMap::new(),
            decided: BTreeMap::new(),
            stopped: None,
            concluded: None,
        }
    }

    fn load(directory: &Path, run: &str) -> Result<Self, String> {
        let mut history = History::new(run);
        history.records = journal::load(&directory.join("records"), run, |record: &Record| {
            history.apply(record)
        })?;
        if history.request.is_none() {
            return Err(format!("run {run} has no request record"));
        }
        Ok(history)
    }

    pub(crate) fn request(&self) -> &Request {
        self.request.as_ref().expect("a loaded run has its request")
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

    fn open(&self) -> Result<(), String> {
        if self.concluded.is_some() {
            return Err("the run is already concluded".into());
        }
        if self.stopped.is_some() {
            return Err("the run was stopped; resume it first".into());
        }
        Ok(())
    }

    fn unresolved(&self, task: &str) -> Option<&AttemptState> {
        self.lineage
            .get(task)
            .and_then(|ids| ids.last())
            .map(|id| &self.attempts[id])
            .filter(|attempt| attempt.end.is_none())
    }

    fn undecided_task(&self, task: &str) -> Result<(), String> {
        if !self.request().tasks.iter().any(|name| name == task) {
            return Err(format!("task {task} is not in this run"));
        }
        if self.decided.contains_key(task) {
            return Err(format!("task {task} already has an accepted outcome"));
        }
        if self.unresolved(task).is_some() {
            return Err(format!("task {task} has an unresolved attempt"));
        }
        Ok(())
    }

    fn apply(&mut self, record: &Record) -> Result<(), String> {
        let at = record.recorded_at;
        let fact = &record.fact;
        if let Fact::Requested { request } = fact {
            if record.sequence != 1 || self.request.is_some() {
                return Err("a run is requested once, in its first record".into());
            }
            if identity("sykli-run.v1", &request.0)? != self.run {
                return Err("the run's request does not match its id".into());
            }
            if request.0.executor != EXECUTOR
                || crate::sha256(request.0.contract.as_bytes()) != request.0.contract_hash
            {
                return Err("the run's request names another executor or contract".into());
            }
            self.request = Some(request.0.clone());
            self.generation = 1;
            return Ok(());
        }
        if self.request.is_none() {
            return Err("a record before the run's request".into());
        }
        match fact {
            Fact::Requested { .. } => unreachable!(),
            Fact::Resumed { generation } => {
                if *generation != self.generation + 1 || self.concluded.is_some() {
                    return Err("a resumed generation must follow the last, before the end".into());
                }
                self.generation = *generation;
                self.stopped = None;
            }
            Fact::Dispatched {
                attempt,
                task,
                generation,
                cache_key,
                supersedes,
                executor,
            } => {
                self.current(*generation)?;
                self.open()?;
                digest(attempt)?;
                digest(cache_key)?;
                self.undecided_task(task)?;
                let latest = self.lineage.get(task).and_then(|ids| ids.last());
                if self.attempts.contains_key(attempt)
                    || supersedes.as_ref() != latest
                    || executor != EXECUTOR
                {
                    return Err("invalid attempt identity, lineage or executor".into());
                }
                self.attempts.insert(
                    attempt.clone(),
                    AttemptState {
                        id: attempt.clone(),
                        task: task.clone(),
                        generation: *generation,
                        cache_key: cache_key.clone(),
                        supersedes: supersedes.clone(),
                        started: None,
                        end: None,
                        output: None,
                        withdrawn: None,
                    },
                );
                self.lineage
                    .entry(task.clone())
                    .or_default()
                    .push(attempt.clone());
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
            Fact::Finished {
                attempt,
                generation,
                exit_code,
                signal,
                outcome,
                output,
            } => {
                let current = self.generation;
                let state = self
                    .attempts
                    .get_mut(attempt)
                    .ok_or("a terminal record without a dispatch")?;
                // A coordinator a newer generation has taken over can never
                // complete anything, whatever became of its attempt.
                if state.generation != *generation || *generation != current {
                    return Err(format!(
                        "stale completion: attempt {attempt} of generation {} is fenced off by \
                         generation {current}",
                        state.generation
                    ));
                }
                if let Some((_, previous)) = &state.end {
                    if previous == fact {
                        return Err(
                            "duplicate terminal record; replay the identical envelope instead"
                                .into(),
                        );
                    }
                    return Err(format!(
                        "conflicting terminal reports: attempt {attempt} already ended"
                    ));
                }
                digest(output)?;
                let consistent = match outcome {
                    // No pass without the evidence of one.
                    Outcome::Passed => *exit_code == Some(0) && signal.is_none(),
                    // Exit 0 can still fail: a declared output is missing.
                    Outcome::Failed => true,
                    Outcome::Errored => true,
                    Outcome::Cached | Outcome::Blocked => false,
                };
                if !consistent || (state.started.is_none() && *outcome != Outcome::Errored) {
                    return Err("a terminal record inconsistent with its exit".into());
                }
                state.end = Some((
                    End::Finished {
                        exit_code: *exit_code,
                        signal: *signal,
                        outcome: *outcome,
                        at,
                    },
                    fact.clone(),
                ));
                state.output = Some(output.clone());
                let task = state.task.clone();
                self.decided.insert(task, Decided::Attempt(attempt.clone()));
            }
            Fact::Reused {
                task,
                generation,
                cache_key,
                provenance,
                output,
            } => {
                self.current(*generation)?;
                self.open()?;
                self.undecided_task(task)?;
                digest(cache_key)?;
                digest(output)?;
                self.decided.insert(
                    task.clone(),
                    Decided::Without {
                        outcome: Outcome::Cached,
                        output: output.clone(),
                        cache_key: Some(cache_key.clone()),
                        provenance: Some(provenance.clone()),
                        generation: *generation,
                    },
                );
            }
            Fact::Settled {
                task,
                generation,
                cache_key,
                outcome,
                output,
            } => {
                self.current(*generation)?;
                self.open()?;
                self.undecided_task(task)?;
                digest(output)?;
                if !matches!(outcome, Outcome::Blocked | Outcome::Errored) {
                    return Err("only a blocked or errored task is settled without running".into());
                }
                self.decided.insert(
                    task.clone(),
                    Decided::Without {
                        outcome: *outcome,
                        output: output.clone(),
                        cache_key: cache_key.clone(),
                        provenance: None,
                        generation: *generation,
                    },
                );
            }
            Fact::Interrupted {
                attempt,
                generation,
                signal,
                exit_code,
                ended_by,
            } => {
                self.current(*generation)?;
                let state = self
                    .attempts
                    .get_mut(attempt)
                    .ok_or("an interruption without a dispatch")?;
                if state.end.is_some() || state.generation != *generation {
                    return Err(
                        "only an unresolved attempt of this generation is interrupted".into(),
                    );
                }
                state.end = Some((
                    End::Interrupted {
                        signal: *signal,
                        exit_code: *exit_code,
                        ended_by: *ended_by,
                        at,
                    },
                    fact.clone(),
                ));
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
                        "only an unresolved attempt of an earlier generation is lost".into(),
                    );
                }
                state.end = Some((
                    End::Lost {
                        at,
                        reason: reason.clone(),
                    },
                    fact.clone(),
                ));
            }
            Fact::Withdrawn {
                task,
                attempt,
                generation,
                reason,
            } => {
                self.current(*generation)?;
                self.open()?;
                let refused = "only a pass this generation accepted is withdrawn, once";
                match (self.decided.get(task), attempt) {
                    (Some(Decided::Attempt(id)), Some(attempt)) if id == attempt => {
                        let state = self.attempts.get_mut(attempt).ok_or(refused)?;
                        let passed = matches!(
                            state.end,
                            Some((
                                End::Finished {
                                    outcome: Outcome::Passed,
                                    ..
                                },
                                _
                            ))
                        );
                        if state.generation != *generation || state.withdrawn.is_some() || !passed {
                            return Err(refused.into());
                        }
                        state.withdrawn = Some(reason.clone());
                    }
                    (
                        Some(Decided::Without {
                            outcome: Outcome::Cached,
                            generation: decided_in,
                            ..
                        }),
                        None,
                    ) if decided_in == generation => {}
                    _ => return Err(refused.into()),
                }
                self.decided.remove(task);
            }
            Fact::Stopped {
                generation,
                signal,
                reason,
            } => {
                self.current(*generation)?;
                self.open()?;
                if self
                    .attempts
                    .values()
                    .any(|a| a.end.is_none() && a.generation == *generation)
                {
                    return Err("a run stops only once its attempts have ended".into());
                }
                self.stopped = Some((*signal, reason.clone()));
            }
            Fact::Concluded {
                generation,
                receipt,
                digest: receipt_digest,
                outcome,
            } => {
                self.current(*generation)?;
                self.open()?;
                digest(receipt_digest)?;
                let request = self.request();
                if request
                    .tasks
                    .iter()
                    .any(|task| !self.decided.contains_key(task))
                    || self.attempts.values().any(|a| a.end.is_none())
                {
                    return Err("a run concludes only once every task is decided".into());
                }
                self.concluded = Some((receipt.clone(), receipt_digest.clone(), *outcome));
            }
        }
        Ok(())
    }
}

/// `<common>/sykli/runs`.
fn runs_directory(common: &Path) -> PathBuf {
    common.join("sykli").join("runs")
}

fn run_directory(common: &Path, run: &str) -> Result<PathBuf, String> {
    digest(run).map_err(|_| format!("{run:?} is not a run id"))?;
    Ok(runs_directory(common).join(run))
}

fn common_dir() -> Result<PathBuf, String> {
    let repository = crate::repo::repository()
        .map_err(|_| "not inside a git repository; runs are kept in its git directory")?;
    crate::absolute(&repository.common_dir)
}

/// A run's coordinator: the holder of its lease, the only writer of its
/// journal, and the parent of its attempts.
pub(crate) struct Coordinator {
    directory: PathBuf,
    id: String,
    lease: Lease,
    history: Mutex<History>,
    /// Each pass this process accepted, when it was decided: an attempt's,
    /// at its end, or a reuse from the cache, at its decision. For a stop
    /// that comes right after (`withdraw_before_stop`).
    passes: Mutex<Vec<Pass>>,
    /// Attempts whose process group still existed when `--assume-ended` let
    /// this coordinator take the run over, with that group.
    assumed: Vec<(String, i32)>,
    /// The generation this coordinator holds, fixed when it creates, receives
    /// or takes over the run; every fact it records carries it.
    generation: AtomicU64,
    /// Set once a newer generation has taken the run over (or the journal
    /// could not be read back after a failed write): nothing more is written.
    fenced: AtomicBool,
}

/// A pass a coordinator accepted, and when.
struct Pass {
    task: String,
    /// The attempt that passed; none for a task reused from the cache.
    attempt: Option<String>,
    at: Instant,
}

/// One attempt, dispatched: its id and its lease, which its shell inherits.
pub(crate) struct Attempt<'a> {
    coordinator: &'a Coordinator,
    id: String,
    task: String,
    generation: u64,
    lease: Lease,
}

impl Coordinator {
    /// Record a new run, durably, and hold it: its lease is taken before its
    /// first record is published, so it is never seen unheld before it ends.
    pub(crate) fn create(request: Request) -> Result<Self, String> {
        let id = identity("sykli-run.v1", &request)?;
        let common = PathBuf::from(&request.common_dir);
        // Never through a symbolic link, as kept receipts are never written
        // through one: the journal is evidence too.
        let mut parent = common.clone();
        for part in ["sykli", "runs"] {
            parent.push(part);
            crate::hook::ensure_directory(&parent, |path| fs::create_dir(path))?;
        }
        prune(&common, &id);
        let directory = run_directory(&common, &id)?;
        // Made aside and renamed into place with its lease already held, so
        // no one (a pruner) ever sees the run without a holder while it is
        // being created.
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
        // The directories' own entries, so the first record cannot be
        // orphaned by a power cut.
        for synced in [&directory, &parent, &common.join("sykli"), &common] {
            crate::canonical::sync_directory(synced, Durability::Fsync)?;
        }
        let coordinator = Coordinator {
            directory,
            id: id.clone(),
            lease,
            history: Mutex::new(History::new(&id)),
            passes: Mutex::new(Vec::new()),
            assumed: Vec::new(),
            generation: AtomicU64::new(1),
            fenced: AtomicBool::new(false),
        };
        coordinator.append(Fact::Requested {
            request: Box::new(RequestFact(request)),
        })?;
        Ok(coordinator)
    }

    /// The run a detached coordinator was started for, with the lease its
    /// client handed it.
    pub(crate) fn received(run: &str, fd: i32) -> Result<Self, String> {
        let directory = run_directory(&common_dir()?, run)?;
        let lease = Lease::received(&directory, fd)?;
        let history = History::load(&directory, run)?;
        if history.generation != 1 || !history.attempts.is_empty() || !history.decided.is_empty() {
            return Err("a detached coordinator starts only a fresh run".into());
        }
        Ok(Coordinator {
            directory,
            id: run.into(),
            lease,
            history: Mutex::new(history),
            passes: Mutex::new(Vec::new()),
            assumed: Vec::new(),
            generation: AtomicU64::new(1),
            fenced: AtomicBool::new(false),
        })
    }

    /// Take over a run whose coordinator is gone. Refused while it is alive,
    /// while any attempt's process still is (its lease is held, or its
    /// process group still exists, unless `assume_ended` says that group is
    /// not the attempt's any more), and once the run is concluded. Nothing is
    /// written: `take_over` records the new generation.
    pub(crate) fn reopen(run: &str, assume_ended: bool) -> Result<Self, String> {
        let common = common_dir()?;
        let directory = run_directory(&common, run)?;
        if !directory.join("records").is_dir() {
            return Err(unknown(&common, run));
        }
        Self::reopen_at(directory, run, assume_ended)
    }

    fn reopen_at(directory: PathBuf, run: &str, assume_ended: bool) -> Result<Self, String> {
        let lease = Lease::acquire(&directory).map_err(|error| {
            if error.starts_with("production-busy") {
                format!("run-busy: run {run}'s coordinator is still running")
            } else {
                error
            }
        })?;
        let history = History::load(&directory, run)?;
        if history.concluded.is_some() {
            return Err(format!(
                "run {run} is concluded; there is nothing to resume"
            ));
        }
        let mut assumed = Vec::new();
        for attempt in history.attempts.values().filter(|a| a.end.is_none()) {
            let group = attempt.started.map(|(group, _)| group);
            if let Some(group) = group.filter(|group| journal::group_exists(*group)) {
                if !assume_ended {
                    return Err(format!(
                        "task {}'s attempt {} may be still running without a coordinator: its \
                         process group {group} still exists. Wait for it to end, or stop it, \
                         then resume; if that group now belongs to an unrelated process, \
                         resume with --assume-ended",
                        attempt.task, attempt.id,
                    ));
                }
                // Taken on the caller's word; the lease below still counts.
                assumed.push((attempt.id.clone(), group));
            }
            if Lease::observe(&directory.join("attempt-leases").join(&attempt.id))? {
                return Err(format!(
                    "task {}'s attempt {} is still running without a coordinator (process \
                     group {}); wait for it to end, or stop it, then resume",
                    attempt.task,
                    attempt.id,
                    attempt
                        .started
                        .map_or("unknown".into(), |(group, _)| group.to_string())
                ));
            }
        }
        let generation = history.generation;
        Ok(Coordinator {
            directory,
            id: run.into(),
            lease,
            history: Mutex::new(history),
            passes: Mutex::new(Vec::new()),
            assumed,
            generation: AtomicU64::new(generation),
            fenced: AtomicBool::new(false),
        })
    }

    /// Open the next generation and record every attempt the last one left
    /// unfinished as lost: its outcome is unknown, and it is never passed.
    pub(crate) fn take_over(&self) -> Result<(), String> {
        let generation = self.generation() + 1;
        self.append(Fact::Resumed { generation })?;
        self.generation.store(generation, Ordering::SeqCst);
        let unresolved = {
            let history = self.history.lock().unwrap();
            history
                .attempts
                .values()
                .filter(|a| a.end.is_none())
                .map(|a| a.id.clone())
                .collect::<Vec<_>>()
        };
        for attempt in unresolved {
            let reason = match self.assumed.iter().find(|(id, _)| *id == attempt) {
                // The override leaves its trace: lost on the caller's word.
                Some((_, group)) => format!(
                    "its process group {group} still existed when `resume --assume-ended` took \
                     the run over; taken as ended on that word, whether it ran to completion \
                     is unknown"
                ),
                None => "its coordinator ended without recording its outcome; whether it ran \
                         to completion is unknown"
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

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub(crate) fn request(&self) -> Request {
        self.history.lock().unwrap().request().clone()
    }

    /// The undecided tasks whose last attempt had started: run again by a
    /// resume, although that attempt may have run to completion (and done
    /// whatever it does) unobserved. Each with that attempt's id.
    pub(crate) fn restarted(&self) -> Vec<(String, String)> {
        let history = self.history.lock().unwrap();
        history
            .request()
            .tasks
            .iter()
            .filter(|task| !history.decided.contains_key(*task))
            .filter_map(|task| {
                let attempt = &history.attempts[history.lineage.get(task)?.last()?];
                attempt
                    .started
                    .is_some()
                    .then(|| (task.clone(), attempt.id.clone()))
            })
            .collect()
    }

    /// Append a fact under the journal lock. Only the coordinator holding
    /// the run's lease writes, so the journal is reread only when a record
    /// beyond the last one known is already there; the new fact is then
    /// judged against all of it. When that record opened a newer generation,
    /// this coordinator was taken over: it is fenced off, and writes nothing
    /// more, whatever it tries next (its facts keep its own generation, so
    /// they could not pass for the newer one's anyway).
    fn append(&self, fact: Fact) -> Result<(), String> {
        let mut history = self.history.lock().unwrap();
        let stale = || {
            format!(
                "stale coordinator: run {} was taken over by a newer generation; this one \
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
            // What was not written is not known: the history goes back to
            // the journal, and a coordinator that cannot read it back writes
            // nothing more.
            match History::load(&self.directory, &self.id) {
                Ok(reloaded) => *history = reloaded,
                Err(_) => self.fenced.store(true, Ordering::SeqCst),
            }
            return Err(error);
        }
        history.records.push(envelope);
        Ok(())
    }

    /// Keep a task record as an output of this run, named by its digest.
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

    fn output(&self, digest: &str) -> Result<TaskReceipt, String> {
        read_output(&self.directory, digest)
    }

    /// Dispatch a new attempt of `task`: its lease is taken and its
    /// `dispatched` record is durable before anything is spawned.
    pub(crate) fn dispatch(&self, task: &str, cache_key: &str) -> Result<Attempt<'_>, String> {
        let generation = self.generation();
        let (supersedes, sequence) = {
            let history = self.history.lock().unwrap();
            (
                history
                    .lineage
                    .get(task)
                    .and_then(|ids| ids.last())
                    .cloned(),
                history.records.len(),
            )
        };
        let id = identity(
            "sykli-attempt.v1",
            &json!({"run": self.id, "task": task, "sequence": sequence, "time": now(),
                    "pid": std::process::id(), "nonce": NONCE.fetch_add(1, Ordering::Relaxed)}),
        )?;
        let lease = Lease::attempt(&self.directory, &id)?;
        self.append(Fact::Dispatched {
            attempt: id.clone(),
            task: task.into(),
            generation,
            cache_key: cache_key.into(),
            supersedes,
            executor: EXECUTOR.into(),
        })?;
        Ok(Attempt {
            coordinator: self,
            id,
            task: task.into(),
            generation,
            lease,
        })
    }

    /// A task decided from the cache.
    pub(crate) fn reuse(
        &self,
        task: &str,
        cache_key: &str,
        record: &TaskReceipt,
    ) -> Result<(), String> {
        let output = self.persist(record)?;
        self.append(Fact::Reused {
            task: task.into(),
            generation: self.generation(),
            cache_key: cache_key.into(),
            provenance: record.provenance.clone().unwrap_or_default(),
            output,
        })?;
        self.passes.lock().unwrap().push(Pass {
            task: task.into(),
            attempt: None,
            at: Instant::now(),
        });
        Ok(())
    }

    /// A task decided without running: blocked, or errored before it could.
    pub(crate) fn settle(
        &self,
        task: &str,
        cache_key: Option<&str>,
        record: &TaskReceipt,
    ) -> Result<(), String> {
        let output = self.persist(record)?;
        self.append(Fact::Settled {
            task: task.into(),
            generation: self.generation(),
            cache_key: cache_key.map(str::to_string),
            outcome: record.outcome,
            output,
        })
    }

    /// Every task's accepted record, in the run's task order, or None where
    /// there is none yet; each with the cache key it was decided under.
    pub(crate) fn decided(&self) -> Result<Vec<Option<TaskReceipt>>, String> {
        let history = self.history.lock().unwrap();
        history
            .request()
            .tasks
            .iter()
            .map(|task| {
                let Some(decided) = history.decided.get(task) else {
                    return Ok(None);
                };
                let (output, key) = match decided {
                    Decided::Attempt(id) => {
                        let attempt = &history.attempts[id];
                        (
                            attempt.output.clone().unwrap_or_default(),
                            Some(attempt.cache_key.clone()),
                        )
                    }
                    Decided::Without {
                        output, cache_key, ..
                    } => (output.clone(), cache_key.clone()),
                };
                let mut record = self.output(&output)?;
                record.cache_key = key;
                Ok(Some(record))
            })
            .collect()
    }

    /// A stop reached sykli: withdraw every pass this process accepted for
    /// an attempt it saw end less than the stop window (100 ms) before its
    /// handler ran. The same stop may have reached that attempt first (a
    /// signal to a process group or a whole service holding both), and a
    /// task that trapped it and exited 0 never finished its work. Every pass
    /// decided after the first of those (a dependent reused from the cache)
    /// may rest on it, and is withdrawn with it. A failure is never
    /// withdrawn: it is never a false pass. A resume runs what was
    /// withdrawn again: at least once, never a pass the stop produced.
    pub(crate) fn withdraw_before_stop(&self) -> Result<(), String> {
        let window = stop_window();
        let Some(cutoff) = signalled_at().map(|at| at.checked_sub(window).unwrap_or(at)) else {
            return Ok(());
        };
        let passes = std::mem::take(&mut *self.passes.lock().unwrap());
        let Some(first) = passes
            .iter()
            .filter(|pass| pass.attempt.is_some() && pass.at >= cutoff)
            .map(|pass| pass.at)
            .min()
        else {
            return Ok(());
        };
        for pass in passes.into_iter().filter(|pass| pass.at >= first) {
            let reason = match &pass.attempt {
                Some(_) => format!(
                    "it ended less than {} ms before a stop reached sykli, which may have \
                     ended it",
                    window.as_millis()
                ),
                None => "it was reused from the cache after a pass that is withdrawn, on \
                          which it may rest"
                    .to_string(),
            };
            self.append(Fact::Withdrawn {
                task: pass.task,
                attempt: pass.attempt,
                generation: self.generation(),
                reason,
            })?;
        }
        Ok(())
    }

    /// Before a run concludes: wait until the stop window has passed since
    /// the last pass an attempt of this process produced, so that a stop
    /// which reached that attempt first, and sykli only now, is still seen
    /// (and the pass withdrawn) even when nothing else is left to run.
    /// Returns as soon as a signal arrives. At most the window, once per run,
    /// and nothing when no attempt passed.
    pub(crate) fn wait_out_stop_window(&self) {
        let last = self
            .passes
            .lock()
            .unwrap()
            .iter()
            .filter(|pass| pass.attempt.is_some())
            .map(|pass| pass.at)
            .max();
        let Some(until) = last.map(|last| last + stop_window()) else {
            return;
        };
        while signalled().is_none() {
            let now = Instant::now();
            if now >= until {
                break;
            }
            std::thread::sleep((until - now).min(Duration::from_millis(5)));
        }
    }

    /// The run stopped on a signal: every attempt has ended.
    pub(crate) fn stop(&self, signal: Option<i32>, reason: &str) -> Result<(), String> {
        self.append(Fact::Stopped {
            generation: self.generation(),
            signal,
            reason: reason.into(),
        })
    }

    /// The run's receipt is durable: the run is complete.
    pub(crate) fn conclude(
        &self,
        receipt: &Path,
        digest: &str,
        outcome: Outcome,
    ) -> Result<(), String> {
        self.append(Fact::Concluded {
            generation: self.generation(),
            receipt: receipt.to_string_lossy().into(),
            digest: digest.into(),
            outcome,
        })
    }

    /// Hand the run to a coordinator in its own session that outlives this
    /// process; it inherits the run's lease. A coordinator that cannot be
    /// started is recorded as such, never as a run that went on.
    pub(crate) fn detach(self) -> Result<String, String> {
        let request = self.request();
        let prepared = crate::repo::repository()
            .and_then(|repository| Ok((repository, std::env::current_exe().map_err(err)?)));
        let (repository, executable) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.stop(
                    None,
                    &format!("its coordinator could not be started: {error}"),
                )?;
                return Err(error);
            }
        };
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
        let log = match fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.directory.join("coordinator.log"))
        {
            Ok(log) => log,
            Err(error) => {
                self.stop(
                    None,
                    &format!("its coordinator could not be started: {error}"),
                )?;
                return Err(error.to_string());
            }
        };
        command
            .arg("--repo-root")
            .arg(&repository.root)
            .arg("--git-common-dir")
            .arg(&request.common_dir)
            .args(["__run_coordinator", "--run", &self.id, "--lease-fd"])
            .arg(fd.to_string())
            .current_dir(&request.workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log);
        // The coordinator outlives this process, and its tasks run under it:
        // neither keeps a descriptor its caller left open (a pipe the caller
        // reads to its end would stay open until the run ends). Only the
        // lease is handed on.
        journal::close_inherited_on_exec();
        match command.spawn() {
            Ok(_) => Ok(self.id.clone()),
            Err(error) => {
                self.stop(
                    None,
                    &format!("its coordinator could not be started: {error}"),
                )?;
                Err(format!(
                    "the run's coordinator could not be started: {error}"
                ))
            }
        }
    }
}

impl Attempt<'_> {
    /// Let the attempt's shell, and everything it starts, hold its lease.
    pub(crate) fn inherit(&self, command: &mut ProcessCommand) {
        self.lease.inherit(command);
    }

    /// The shell is running, in process group `group`.
    pub(crate) fn started(&self, group: i32, started_at_ms: u64) -> Result<(), String> {
        self.coordinator.append(Fact::Started {
            attempt: self.id.clone(),
            generation: self.generation,
            process_group: group,
            started_at_ms,
        })?;
        crate::checkpoint("after-start")
    }

    /// The attempt's process has exited and `record` is what was observed:
    /// kept as its output, then accepted. Exactly one end per attempt; a
    /// stale or duplicate completion is refused by the journal.
    pub(crate) fn finished(&self, record: &TaskReceipt, ended: Instant) -> Result<(), String> {
        crate::checkpoint("after-exit")?;
        let output = self.coordinator.persist(record)?;
        crate::checkpoint("after-output")?;
        self.coordinator.append(Fact::Finished {
            attempt: self.id.clone(),
            generation: self.generation,
            exit_code: record.exit_code,
            signal: record.signal,
            outcome: record.outcome,
            output,
        })?;
        // Only a pass can be a false one; a failure stands whatever comes.
        if record.outcome == Outcome::Passed {
            self.coordinator.passes.lock().unwrap().push(Pass {
                task: self.task.clone(),
                attempt: Some(self.id.clone()),
                at: ended,
            });
        }
        Ok(())
    }

    /// sykli stopped the attempt's process on `signal`: its outcome is
    /// unknown, and nothing it printed is accepted. `exit_code` or
    /// `ended_by` is how its shell ended.
    pub(crate) fn interrupted(
        &self,
        signal: i32,
        exit_code: Option<i32>,
        ended_by: Option<i32>,
    ) -> Result<(), String> {
        self.coordinator.append(Fact::Interrupted {
            attempt: self.id.clone(),
            generation: self.generation,
            signal,
            exit_code,
            ended_by,
        })
    }
}

/// Whether an unfinished attempt may still be running: its shell, or a
/// process it started, holds the attempt's lease; or the process group the
/// journal recorded for it still exists. A process that closed the lease's
/// descriptor (as `sudo` does) is still in the group; one that closed the
/// descriptor and left the group escapes both. A group id the system has
/// reused for an unrelated process reads as alive: never lost too early.
fn attempt_alive(directory: &Path, attempt: &AttemptState) -> Result<bool, String> {
    Ok(
        Lease::observe(&directory.join("attempt-leases").join(&attempt.id))?
            || attempt
                .started
                .is_some_and(|(group, _)| journal::group_exists(group)),
    )
}

fn read_output(directory: &Path, digest_hex: &str) -> Result<TaskReceipt, String> {
    digest(digest_hex)?;
    let path = directory.join("outputs").join(format!("{digest_hex}.json"));
    let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if crate::sha256(&bytes) != digest_hex {
        return Err(format!("{} does not match its digest", path.display()));
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

// Signals. The handler only notes the first signal (and that more came);
// a watcher thread stops every attempt's process group: SIGTERM at once,
// SIGKILL if the group is still there after a grace period or on a second
// signal. Each attempt sykli signalled is then recorded as interrupted,
// never as failed and never as passed: whatever it exited with answered
// the signal, not its task.
static SIGNALLED: AtomicI32 = AtomicI32::new(0);
/// When the first signal's handler ran, in nanoseconds after `CLOCK`
/// (0: none yet).
static SIGNALLED_AT: AtomicU64 = AtomicU64::new(0);
static CLOCK: OnceLock<Instant> = OnceLock::new();
/// How long before sykli's own handler ran an attempt's end counts as
/// possibly caused by the same stop (`withdraw_before_stop`).
const STOP_WINDOW: Duration = Duration::from_millis(100);

/// The stop window; a debug build takes another from
/// `SYKLI_TEST_STOP_WINDOW_MS`, so tests need not race the scheduler.
fn stop_window() -> Duration {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("SYKLI_TEST_STOP_WINDOW_MS")
        .ok()
        .and_then(|ms| ms.parse().ok())
    {
        return Duration::from_millis(ms);
    }
    STOP_WINDOW
}
static REPEATED: AtomicI32 = AtomicI32::new(0);
/// Every running attempt's process group, and whether sykli signalled it.
static GROUPS: Mutex<Vec<(i32, bool)>> = Mutex::new(Vec::new());
const GRACE: std::time::Duration = std::time::Duration::from_secs(3);

extern "C" fn on_signal(number: i32) {
    if SIGNALLED
        .compare_exchange(0, number, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        REPEATED.store(1, Ordering::SeqCst);
    } else if let Some(clock) = CLOCK.get() {
        // Reading the monotonic clock (clock_gettime) is async-signal-safe.
        let at = u64::try_from(clock.elapsed().as_nanos()).unwrap_or(u64::MAX);
        SIGNALLED_AT.store(at.max(1), Ordering::SeqCst);
    }
}

/// Stop the run on SIGINT, SIGTERM or SIGHUP instead of dying with its
/// attempts orphaned and unrecorded. Installed once per process.
pub(crate) fn handle_signals() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        CLOCK.get_or_init(Instant::now);
        // SAFETY: the handler only stores into atomics, which is
        // async-signal-safe; SIGHUP 1, SIGINT 2, SIGTERM 15.
        unsafe {
            for number in [1, 2, 15] {
                journal::signal(number, on_signal as *const () as usize);
            }
        }
        std::thread::spawn(|| {
            let mut since: Option<std::time::Instant> = None;
            let mut killed = false;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(25));
                if SIGNALLED.load(Ordering::SeqCst) == 0 {
                    continue;
                }
                let first = since.is_none();
                let started = *since.get_or_insert_with(std::time::Instant::now);
                let escalate =
                    !killed && (REPEATED.load(Ordering::SeqCst) != 0 || started.elapsed() > GRACE);
                if first || escalate {
                    let number = if escalate { 9 } else { 15 };
                    killed |= escalate;
                    for (group, signalled) in GROUPS.lock().unwrap().iter_mut() {
                        // SAFETY: a process group this process created.
                        unsafe {
                            journal::killpg(*group, number);
                        }
                        *signalled = true;
                    }
                }
            }
        });
    });
}

/// The signal that is stopping this run, if one is.
pub(crate) fn signalled() -> Option<i32> {
    Some(SIGNALLED.load(Ordering::SeqCst)).filter(|number| *number != 0)
}

/// When sykli's handler saw the signal that is stopping this run.
fn signalled_at() -> Option<Instant> {
    let at = SIGNALLED_AT.load(Ordering::SeqCst);
    let clock = CLOCK.get()?;
    (at != 0).then(|| *clock + Duration::from_nanos(at))
}

/// Track an attempt's process group while it runs, so a signal stops it.
/// Checked under the lock the watcher signals under, so a group is either
/// signalled here or by the watcher's next round, never missed.
pub(crate) fn register(group: i32) {
    let mut groups = GROUPS.lock().unwrap();
    let stopping = signalled().is_some();
    if stopping {
        // SAFETY: the group was just created by this process.
        unsafe {
            journal::killpg(group, 15);
        }
    }
    groups.push((group, stopping));
}

/// Whether sykli has signalled `group`, a group it tracks.
pub(crate) fn stopped(group: i32) -> bool {
    GROUPS
        .lock()
        .unwrap()
        .iter()
        .any(|(known, signalled)| *known == group && *signalled)
}

/// Stop tracking `group`; whether sykli signalled it while it was tracked.
pub(crate) fn unregister(group: i32) -> bool {
    let mut groups = GROUPS.lock().unwrap();
    let signalled = groups
        .iter()
        .any(|(known, signalled)| *known == group && *signalled);
    groups.retain(|(known, _)| *known != group);
    signalled
}

/// What sykli knows of one run, from its journal and its leases alone:
/// `sykli-run-status.v1`. Nothing is written, and no git runs.
pub(crate) fn status(run: &str) -> Result<Value, String> {
    let common = common_dir()?;
    let directory = run_directory(&common, run)?;
    if !directory.join("records").is_dir() {
        return Err(unknown(&common, run));
    }
    let history = {
        let _journal = Lease::journal_read(&directory)?;
        History::load(&directory, run)?
    };
    view(&directory, &history)
}

fn view(directory: &Path, history: &History) -> Result<Value, String> {
    let request = history.request();
    let live = Lease::observe(&directory.join("lease"))?;
    let mut tasks = Vec::new();
    for task in &request.tasks {
        let mut attempts = Vec::new();
        for id in history.lineage.get(task).into_iter().flatten() {
            let attempt = &history.attempts[id];
            let (finished, exit, signal, outcome, stopped_by) = match &attempt.end {
                Some((
                    End::Finished {
                        exit_code,
                        signal,
                        outcome,
                        at,
                    },
                    _,
                )) => (
                    Some(*at),
                    *exit_code,
                    *signal,
                    // A withdrawn outcome was not kept: a stop may have caused it.
                    Some(if attempt.withdrawn.is_some() {
                        json!("interrupted")
                    } else {
                        serde_json::to_value(outcome).map_err(err)?
                    }),
                    None,
                ),
                Some((
                    End::Interrupted {
                        signal,
                        exit_code,
                        ended_by,
                        at,
                    },
                    _,
                )) => (
                    Some(*at),
                    *exit_code,
                    *ended_by,
                    Some(json!("interrupted")),
                    Some(*signal),
                ),
                Some((End::Lost { at, .. }, _)) => {
                    (Some(*at), None, None, Some(json!("lost")), None)
                }
                None => {
                    if live || attempt_alive(directory, attempt)? {
                        (None, None, None, None, None)
                    } else {
                        (None, None, None, Some(json!("lost")), None)
                    }
                }
            };
            attempts.push(json!({
                "id": attempt.id,
                "started": attempt.started.map(|(_, at)| at),
                "finished": finished,
                "exit": exit,
                "signal": signal,
                "outcome": outcome,
                "stopped_by": stopped_by,
                "supersedes": attempt.supersedes,
                "generation": attempt.generation,
                "process_group": attempt.started.map(|(group, _)| group),
                "recorded": attempt.end.is_some(),
                "withdrawn": attempt.withdrawn.is_some(),
                // Why it was lost, or why its pass was withdrawn.
                "reason": match &attempt.end {
                    Some((End::Lost { reason, .. }, _)) => Some(reason.clone()),
                    _ => attempt.withdrawn.clone(),
                },
            }));
        }
        let mut entry = json!({"name": task, "attempts": attempts});
        let state = match history.decided.get(task) {
            Some(Decided::Attempt(id)) => match &history.attempts[id].end {
                Some((End::Finished { outcome, .. }, _)) => {
                    serde_json::to_value(outcome).map_err(err)?
                }
                _ => unreachable!("a decided attempt has finished"),
            },
            Some(Decided::Without {
                outcome,
                provenance,
                ..
            }) => {
                if let Some(provenance) = provenance {
                    entry["reused"] = json!({"provenance": provenance});
                }
                serde_json::to_value(outcome).map_err(err)?
            }
            None => match attempts.last() {
                None => json!("pending"),
                Some(last) if last["outcome"].is_null() => json!("running"),
                Some(last) => last["outcome"].clone(),
            },
        };
        entry["state"] = state;
        tasks.push(entry);
    }
    let any_dispatched = !history.attempts.is_empty() || !history.decided.is_empty();
    let state = if history.concluded.is_some() {
        "completed"
    } else if history.stopped.is_some() {
        "interrupted"
    } else if live {
        if any_dispatched {
            "running"
        } else {
            "requested"
        }
    } else {
        // The coordinator is gone without concluding. An orphaned attempt
        // may still be running (its task says so); the run's own outcome
        // is unknown.
        "lost"
    };
    // In one spelling, the file system's, however the run spelled it.
    let receipt = history.concluded.as_ref().map(|(path, digest, _)| {
        let path = crate::spelled(Path::new(path));
        json!({"path": path, "digest": digest, "present": path.is_file()})
    });
    let subject = &request.subject;
    Ok(json!({
        "schema": "sykli-run-status.v1",
        "run": history.run,
        "state": state,
        "generation": history.generation,
        "coordinator_live": live,
        "requested_at_ms": request.started_at_ms,
        "contract": request.contract_path,
        "contract_digest": request.contract_hash,
        "lock_digest": request.lock_digest,
        "subject": {
            "tree": subject.tree_oid,
            "commit": if subject.dirty { None } else { subject.head_commit.clone() },
            "dirty": subject.dirty,
            "head_tree": subject.head_tree_oid,
            "head_commit": subject.head_commit,
            "inputs_digest": subject.inputs_digest,
            "repository": subject.repository,
        },
        "tasks": tasks,
        "receipt": receipt,
        "outcome": history.concluded.as_ref().map(|(_, _, outcome)| *outcome),
        "stopped": history.stopped.as_ref().map(|(signal, reason)| json!({"signal": signal, "reason": reason})),
        "through_sequence": history.records.len(),
        "trust": TRUST,
    }))
}

/// What this clone knows of a run a receipt names.
pub(crate) enum Known {
    /// A run it keeps: its status, or why its journal does not load.
    Kept(Result<Value, String>),
    /// A run it pruned: the tombstone.
    Pruned(Value),
    /// A run it never kept (or whose tombstone is gone).
    Unknown,
}

pub(crate) fn known(run: &str) -> Result<Known, String> {
    let common = common_dir()?;
    if digest(run).is_err() {
        return Ok(Known::Unknown);
    }
    if runs_directory(&common).join(run).exists() {
        return Ok(Known::Kept(status(run)));
    }
    Ok(match pruned(&common, run) {
        Some(note) => Known::Pruned(note),
        None => Known::Unknown,
    })
}

/// Why `run` is not kept: pruned (said so, with what it was), or unknown.
fn unknown(common: &Path, run: &str) -> String {
    match pruned(common, run) {
        Some(note) => format!(
            "run {run} was pruned: it was {}{}; a clone keeps at most SYKLI_RUN_BUDGET runs \
             (default {DEFAULT_RUN_BUDGET}), and removes the oldest first",
            note["state"].as_str().unwrap_or("unknown"),
            note["receipt"]["path"]
                .as_str()
                .map(|path| format!(", with receipt {path}"))
                .unwrap_or_default()
        ),
        None => format!("unknown run {run}"),
    }
}

/// What this clone noted of `run` when it pruned it, if it did
/// (`sykli-run-tombstone.v1`): its state, tree and receipt.
pub(crate) fn pruned(common: &Path, run: &str) -> Option<Value> {
    digest(run).ok()?;
    let path = tombstones(common).join(format!("{run}.json"));
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// `<common>/sykli/pruned-runs`: a note of each pruned run, at most
/// `TOMBSTONE_BUDGET`, the oldest removed first.
fn tombstones(common: &Path) -> PathBuf {
    common.join("sykli").join("pruned-runs")
}

/// The runs this clone keeps whose subject tree is `tree`, newest first,
/// each with its status (or `unreadable` and why, when its journal does not
/// load); and every run whose first record cannot be read, so whose tree is
/// unknown, with why. None is skipped silently.
pub(crate) fn runs_of_tree(tree: &str) -> Result<(Vec<Value>, Vec<Value>), String> {
    let directory = runs_directory(&common_dir()?);
    let Ok(entries) = fs::read_dir(&directory) else {
        return Ok((Vec::new(), Vec::new()));
    };
    let mut found = Vec::new();
    let mut unreadable = Vec::new();
    for entry in entries.flatten() {
        let Some(run) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if digest(&run).is_err() {
            continue;
        }
        // The first record holds the subject: read it alone to decide.
        let first = journal::record_path(&entry.path().join("records"), 1);
        let request = fs::read(&first)
            .map_err(err)
            .and_then(|bytes| decode::<Envelope<Record>>(&bytes))
            .and_then(|envelope| match envelope.record.fact {
                Fact::Requested { request } => Ok(request),
                _ => Err("its first record is not its request".into()),
            });
        let request = match request {
            Ok(request) => request,
            Err(error) => {
                unreadable
                    .push(json!({"run": run, "error": format!("{}: {error}", first.display())}));
                continue;
            }
        };
        if request.0.subject.tree_oid != tree {
            continue;
        }
        let status = status(&run).unwrap_or_else(
            |error| json!({"schema": "sykli-run-status.v1", "run": run, "state": "unreadable", "error": error}),
        );
        found.push((request.0.started_at_ms, status));
    }
    found.sort_by_key(|(started, _)| std::cmp::Reverse(*started));
    Ok((
        found.into_iter().map(|(_, status)| status).collect(),
        unreadable,
    ))
}

/// Keep at most the run budget: remove the oldest runs whose coordinator
/// is not alive, concluded ones first. Never `keep`, never a held run.
fn prune(common: &Path, keep: &str) {
    sweep_leftovers(common);
    let budget = std::env::var("SYKLI_RUN_BUDGET")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(DEFAULT_RUN_BUDGET);
    if budget == 0 {
        return;
    }
    let directory = runs_directory(common);
    let Ok(entries) = fs::read_dir(&directory) else {
        return;
    };
    let runs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name != keep && digest(name).is_ok())
        })
        .collect();
    // The new run is one more.
    if runs.len() < budget {
        return;
    }
    let mut candidates = Vec::new();
    for path in runs.iter() {
        if Lease::observe(&path.join("lease")).unwrap_or(true) {
            continue;
        }
        let records = path.join("records");
        let first = journal::record_path(&records, 1);
        let when = fs::metadata(&first)
            .and_then(|meta| meta.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let concluded = fs::read_dir(&records)
            .ok()
            .and_then(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| !name.starts_with('.'))
                    })
                    .max()
            })
            .and_then(|last| fs::read(last).ok())
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|value| value["record"]["fact"]["kind"] == "concluded");
        // Concluded runs go first (false sorts before true), oldest first.
        candidates.push((!concluded, when, path.clone()));
    }
    candidates.sort();
    let over = runs.len() + 1 - budget;
    for (_, _, path) in candidates.into_iter().take(over) {
        remove_run(common, &path);
    }
    bound_tombstones(common);
}

/// Remove the run in `path`, if no one holds it. Its lease is taken as it
/// is, never re-created (another remover may be deleting the directory
/// around it), so a run with no lease file is left alone. Held, the run is
/// moved aside under a name no listing reads, so only one remover goes on;
/// then its tombstone is written, unless one is there already, and only
/// then is it deleted: what a receipt naming it needs to know (did the run
/// conclude with it?) never goes with it.
fn remove_run(common: &Path, path: &Path) {
    let Some(run) = path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    let Ok(Some(_lease)) = Lease::acquire_existing(path) else {
        return;
    };
    let aside = runs_directory(common).join(format!(
        ".pruning-{run}-{}-{}",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    if fs::rename(path, &aside).is_err() {
        return;
    }
    if note_pruned(common, &aside, run).is_ok() {
        let _ = fs::remove_dir_all(&aside);
    }
}

/// What a remover that died left behind: a run moved aside to be pruned
/// (its tombstone is written if it is missing, then it goes), or one being
/// created that was never renamed into place. Only once no one holds it,
/// and a minute old, so a creation or removal under way is never touched.
fn sweep_leftovers(common: &Path) {
    let Ok(entries) = fs::read_dir(runs_directory(common)) else {
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let pruning = name
            .strip_prefix(".pruning-")
            .and_then(|rest| rest.get(..64));
        if pruning.is_none() && !name.starts_with(".creating-") {
            continue;
        }
        let old = fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(60));
        if !old || Lease::observe(&path.join("lease")).unwrap_or(true) {
            continue;
        }
        if let Some(run) = pruning {
            let _ = note_pruned(common, &path, run);
        }
        let _ = fs::remove_dir_all(&path);
    }
}

/// Write the tombstone of the run in `path`, about to be pruned.
fn note_pruned(common: &Path, path: &Path, run: &str) -> Result<(), String> {
    let tombstone = tombstones(common).join(format!("{run}.json"));
    // The first note stands: another remover may have written it.
    if tombstone.exists() {
        return Ok(());
    }
    let note = match History::load(path, run) {
        Ok(history) => json!({
            "schema": "sykli-run-tombstone.v1",
            "run": run,
            "pruned_at_ms": now(),
            "state": if history.concluded.is_some() {
                "completed"
            } else if history.stopped.is_some() {
                "interrupted"
            } else {
                "lost"
            },
            "tree": history.request().subject.tree_oid,
            "receipt": history.concluded.as_ref().map(|(path, digest, _)| json!({"path": path, "digest": digest})),
            "outcome": history.concluded.as_ref().map(|(_, _, outcome)| *outcome),
        }),
        Err(error) => json!({
            "schema": "sykli-run-tombstone.v1",
            "run": run,
            "pruned_at_ms": now(),
            "state": "unreadable",
            "error": error,
            "tree": null,
            "receipt": null,
            "outcome": null,
        }),
    };
    let directory = tombstones(common);
    // Never through a symbolic link, as the journal is not.
    crate::hook::ensure_directory(&directory, |path| fs::create_dir(path))?;
    publish_with(
        &directory.join(format!("{run}.json")),
        &serde_json::to_vec(&note).map_err(err)?,
        Durability::Fsync,
    )
}

/// Keep at most `TOMBSTONE_BUDGET` tombstones, removing the oldest.
fn bound_tombstones(common: &Path) {
    let Ok(entries) = fs::read_dir(tombstones(common)) else {
        return;
    };
    let mut notes = entries
        .flatten()
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect::<Vec<_>>();
    if notes.len() <= TOMBSTONE_BUDGET {
        return;
    }
    notes.sort();
    let over = notes.len() - TOMBSTONE_BUDGET;
    for (_, path) in notes.into_iter().take(over) {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Request {
        let contract = r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"true"}]}"#;
        Request {
            contract: contract.into(),
            contract_path: "/r/sykli.json".into(),
            contract_hash: crate::sha256(contract.as_bytes()),
            lock_digest: None,
            workdir: "/r".into(),
            common_dir: "/r/.git".into(),
            subject: SubjectRecord {
                repository: "/r".into(),
                tree_oid: "a".repeat(40),
                inputs_digest: "b".repeat(64),
                head_tree_oid: "a".repeat(40),
                dirty: false,
                submodules_modified: false,
                head_commit: None,
                inputs: InputRecords::new(),
            },
            tasks: vec!["t".into()],
            runtime: "shell".into(),
            started_at_ms: 1,
            executor: EXECUTOR.into(),
            detached: false,
            nonce: "n".into(),
        }
    }

    /// A history with `facts` applied in order, or the first refusal.
    fn history(facts: Vec<Fact>) -> Result<History, String> {
        let request = request();
        let run = identity("sykli-run.v1", &request).unwrap();
        let mut history = History::new(&run);
        let mut all = vec![Fact::Requested {
            request: Box::new(RequestFact(request)),
        }];
        all.extend(facts);
        for fact in all {
            let envelope = journal::seal(run.clone(), &history.records, fact)?;
            history.apply(&envelope.record)?;
            history.records.push(envelope);
        }
        Ok(history)
    }

    fn id(c: char) -> String {
        c.to_string().repeat(64)
    }

    fn dispatched(attempt: char, generation: u64, supersedes: Option<char>) -> Fact {
        Fact::Dispatched {
            attempt: id(attempt),
            task: "t".into(),
            generation,
            cache_key: id('c'),
            supersedes: supersedes.map(id),
            executor: EXECUTOR.into(),
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

    fn finished(attempt: char, generation: u64, outcome: Outcome, exit: i32) -> Fact {
        Fact::Finished {
            attempt: id(attempt),
            generation,
            exit_code: Some(exit),
            signal: None,
            outcome,
            output: id('e'),
        }
    }

    #[test]
    fn an_attempt_has_at_most_one_end() {
        let done = vec![
            dispatched('a', 1, None),
            started('a', 1),
            finished('a', 1, Outcome::Passed, 0),
        ];
        assert!(history(done.clone()).is_ok());
        let mut duplicate = done.clone();
        duplicate.push(finished('a', 1, Outcome::Passed, 0));
        assert!(
            history(duplicate)
                .err()
                .unwrap()
                .contains("duplicate terminal record")
        );
        let mut conflicting = done.clone();
        conflicting.push(finished('a', 1, Outcome::Failed, 1));
        assert!(
            history(conflicting)
                .err()
                .unwrap()
                .contains("conflicting terminal reports")
        );
        let mut interrupted = done;
        interrupted.push(Fact::Interrupted {
            attempt: id('a'),
            generation: 1,
            signal: 15,
            exit_code: None,
            ended_by: Some(15),
        });
        assert!(history(interrupted).is_err());
    }

    /// A withdrawn outcome leaves its task undecided, with its attempt kept:
    /// a new attempt supersedes it. Only an outcome this generation accepted
    /// is withdrawn, once, and never after the run concluded.
    #[test]
    fn a_withdrawn_outcome_leaves_its_task_undecided() {
        let withdrawn = |attempt: char, generation: u64| Fact::Withdrawn {
            task: "t".into(),
            attempt: Some(id(attempt)),
            generation,
            reason: "x".into(),
        };
        let passed = vec![
            dispatched('a', 1, None),
            started('a', 1),
            finished('a', 1, Outcome::Passed, 0),
        ];
        let mut once = passed.clone();
        once.push(withdrawn('a', 1));
        let kept = history(once.clone()).unwrap();
        assert!(!kept.decided.contains_key("t"));
        assert_eq!(kept.attempts[&id('a')].withdrawn.as_deref(), Some("x"));
        let mut retried = once.clone();
        retried.push(dispatched('b', 1, Some('a')));
        retried.push(started('b', 1));
        retried.push(finished('b', 1, Outcome::Passed, 0));
        assert!(history(retried).unwrap().decided.contains_key("t"));

        let mut twice = once;
        twice.push(withdrawn('a', 1));
        assert!(history(twice).is_err(), "withdrawn twice");
        let mut unaccepted = vec![dispatched('a', 1, None), started('a', 1)];
        unaccepted.push(withdrawn('a', 1));
        assert!(history(unaccepted).is_err(), "nothing was accepted");
        let mut stale = passed.clone();
        stale.push(Fact::Resumed { generation: 2 });
        stale.push(withdrawn('a', 2));
        assert!(history(stale).is_err(), "another generation's outcome");
        let mut late = passed;
        late.push(Fact::Concluded {
            generation: 1,
            receipt: "r".into(),
            digest: id('d'),
            outcome: Outcome::Passed,
        });
        late.push(withdrawn('a', 1));
        assert!(history(late).is_err(), "the run concluded");

        // A failure is never withdrawn: it is never a false pass.
        let failed = vec![
            dispatched('a', 1, None),
            started('a', 1),
            finished('a', 1, Outcome::Failed, 1),
            withdrawn('a', 1),
        ];
        assert!(history(failed).is_err(), "a failure withdrawn");
        // A task reused from the cache in this generation can be, with no
        // attempt; one settled blocked cannot.
        let without = |outcome: Option<Outcome>| match outcome {
            None => Fact::Reused {
                task: "t".into(),
                generation: 1,
                cache_key: id('c'),
                provenance: "p".into(),
                output: id('e'),
            },
            Some(outcome) => Fact::Settled {
                task: "t".into(),
                generation: 1,
                cache_key: None,
                outcome,
                output: id('e'),
            },
        };
        let reopen = Fact::Withdrawn {
            task: "t".into(),
            attempt: None,
            generation: 1,
            reason: "x".into(),
        };
        let reused = history(vec![without(None), reopen.clone()]).unwrap();
        assert!(!reused.decided.contains_key("t"));
        assert!(history(vec![without(Some(Outcome::Blocked)), reopen.clone()]).is_err());
        assert!(
            history(vec![
                without(None),
                Fact::Resumed { generation: 2 },
                Fact::Withdrawn {
                    task: "t".into(),
                    attempt: None,
                    generation: 2,
                    reason: "x".into(),
                },
            ])
            .is_err(),
            "a reuse of another generation"
        );
    }

    #[test]
    fn a_newer_generation_fences_off_the_old_one() {
        let taken_over = vec![
            dispatched('a', 1, None),
            started('a', 1),
            Fact::Resumed { generation: 2 },
        ];
        let mut stale = taken_over.clone();
        stale.push(finished('a', 1, Outcome::Passed, 0));
        assert!(history(stale).err().unwrap().contains("stale completion"));
        let mut stale_dispatch = taken_over.clone();
        stale_dispatch.push(Fact::Lost {
            attempt: id('a'),
            generation: 2,
            reason: "gone".into(),
        });
        stale_dispatch.push(dispatched('b', 1, Some('a')));
        assert!(
            history(stale_dispatch)
                .err()
                .unwrap()
                .contains("stale record")
        );
        // Only an earlier generation's attempt is lost, and the retry
        // supersedes it.
        let mut retried = taken_over;
        retried.push(Fact::Lost {
            attempt: id('a'),
            generation: 2,
            reason: "gone".into(),
        });
        retried.push(dispatched('b', 2, Some('a')));
        retried.push(started('b', 2));
        retried.push(finished('b', 2, Outcome::Passed, 0));
        let history = history(retried).unwrap();
        assert_eq!(history.lineage["t"], vec![id('a'), id('b')]);
        assert!(matches!(history.decided["t"], Decided::Attempt(ref b) if *b == id('b')));
    }

    #[test]
    fn no_pass_without_its_evidence_and_no_conclusion_without_every_task() {
        let started_a = vec![dispatched('a', 1, None), started('a', 1)];
        let mut lying = started_a.clone();
        lying.push(finished('a', 1, Outcome::Passed, 1));
        assert!(history(lying).err().unwrap().contains("inconsistent"));
        let mut unstarted_pass = vec![dispatched('a', 1, None)];
        unstarted_pass.push(finished('a', 1, Outcome::Passed, 0));
        assert!(history(unstarted_pass).is_err());
        let mut early = started_a.clone();
        early.push(Fact::Concluded {
            generation: 1,
            receipt: "r".into(),
            digest: id('d'),
            outcome: Outcome::Passed,
        });
        assert!(
            history(early)
                .err()
                .unwrap()
                .contains("every task is decided")
        );
        let mut lost_now = started_a.clone();
        lost_now.push(Fact::Lost {
            attempt: id('a'),
            generation: 1,
            reason: "x".into(),
        });
        assert!(
            history(lost_now).is_err(),
            "a live generation's attempt is not lost"
        );
        let mut wrong_lineage = started_a;
        wrong_lineage.push(Fact::Interrupted {
            attempt: id('a'),
            generation: 1,
            signal: 2,
            exit_code: None,
            ended_by: Some(15),
        });
        wrong_lineage.push(dispatched('b', 1, None));
        assert!(history(wrong_lineage).err().unwrap().contains("lineage"));
    }

    /// Invariant D, the fence itself: once a newer coordinator has taken a
    /// run over, the old one writes nothing more, whatever it tries next. Its
    /// first refused write must not teach it the new generation.
    #[test]
    fn a_fenced_off_coordinator_writes_nothing_more() {
        let common = std::env::temp_dir().join(format!(
            "sykli-fence-{}-{}-{}",
            std::process::id(),
            now(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&common).unwrap();
        let mut request = request();
        request.common_dir = common.to_string_lossy().into();
        let a = Coordinator::create(request).unwrap();
        let directory = a.directory.clone();
        // A loses its lease (its file is replaced), and B takes the run over.
        fs::remove_file(directory.join("lease")).unwrap();
        let b = Coordinator::reopen_at(directory.clone(), &a.id, false).unwrap();
        b.take_over().unwrap();
        assert_eq!(b.generation(), 2);
        // A's first write is refused, and so is every one after it.
        assert!(a.dispatch("t", &id('c')).is_err());
        assert!(a.dispatch("t", &id('c')).is_err());
        assert!(a.stop(None, "stale").is_err());
        let history = History::load(&directory, &a.id).unwrap();
        assert!(
            history.attempts.is_empty(),
            "the stale coordinator dispatched"
        );
        assert!(
            history.stopped.is_none(),
            "the stale coordinator stopped the run"
        );
        // B is unaffected.
        assert!(b.dispatch("t", &id('c')).is_ok());
        drop((a, b));
        fs::remove_dir_all(common).unwrap();
    }
}
