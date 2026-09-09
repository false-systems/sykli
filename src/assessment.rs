//! Candidate assessment: finite requirements, the request binding a candidate
//! to them, pure evaluation of normalized observations, and the projections
//! (rows, Mermaid, explanations) of one result structure.
//!
//! Nothing here touches the network or the file system. Every verdict is
//! limited to the named predicate under the visible trust designation.
use crate::canonical::{decode, identity};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const REQUIREMENTS_SCHEMA: &str = "sykli-requirements.v1";
pub const REQUEST_SCHEMA: &str = "sykli-request.v1";
pub const ASSESSMENT_SCHEMA: &str = "sykli-assessment.v1";
pub const EVALUATOR: &str = "review-readiness.v1";
pub const TRUST: &str = "trusted-local-collector-and-store";
pub const AUTHENTICITY: &str = "not-established";
pub const MODE: &str = "advisory";
/// Upper bound for a freshness allowance: ten years, so the sum with a
/// collection end can never overflow and "never stale" is spelled explicitly.
pub const MAX_OBSERVATION_AGE: u64 = 315_360_000;

// ---------------------------------------------------------------- time

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_index = i64::from((month + 9) % 12);
    let day_of_year = (153 * month_index + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Parse an RFC 3339 UTC timestamp (`YYYY-MM-DDTHH:MM:SS[.fff]Z`) to Unix seconds.
pub fn parse_time(text: &str) -> Result<u64, String> {
    let invalid = || format!("invalid UTC timestamp {text:?}; use YYYY-MM-DDTHH:MM:SSZ");
    let body = text.strip_suffix('Z').ok_or_else(invalid)?;
    let (date, clock) = body.split_once('T').ok_or_else(invalid)?;
    let clock = clock.split_once('.').map_or(clock, |(whole, fraction)| {
        if fraction.is_empty() || !fraction.bytes().all(|b| b.is_ascii_digit()) {
            ""
        } else {
            whole
        }
    });
    let field = |part: &str, width: usize| -> Result<u32, String> {
        if part.len() != width || !part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        part.parse().map_err(|_| invalid())
    };
    let mut dates = date.split('-');
    let mut clocks = clock.split(':');
    let (year, month, day) = (
        field(dates.next().unwrap_or(""), 4)?,
        field(dates.next().unwrap_or(""), 2)?,
        field(dates.next().unwrap_or(""), 2)?,
    );
    let (hour, minute, second) = (
        field(clocks.next().unwrap_or(""), 2)?,
        field(clocks.next().unwrap_or(""), 2)?,
        field(clocks.next().unwrap_or(""), 2)?,
    );
    if dates.next().is_some() || clocks.next().is_some() {
        return Err(invalid());
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return Err(invalid()),
    };
    if year < 1970 || day == 0 || day > days_in_month || hour > 23 || minute > 59 || second > 59 {
        return Err(invalid());
    }
    let days = days_from_civil(i64::from(year), month, day);
    Ok(days as u64 * 86_400 + u64::from(hour) * 3600 + u64::from(minute) * 60 + u64::from(second))
}

/// Format Unix seconds as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn format_time(seconds: u64) -> String {
    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
    let clock = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        clock / 3600,
        clock % 3600 / 60,
        clock % 60
    )
}

fn clock(seconds: u64) -> String {
    let clock = seconds % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        clock / 3600,
        clock % 3600 / 60,
        clock % 60
    )
}

pub fn short(commit: &str) -> String {
    let mut shown: String = commit.chars().take(7).collect();
    if commit.chars().count() > 7 {
        shown.push('…');
    }
    shown
}

// -------------------------------------------------------- requirements

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Requirements {
    pub schema: String,
    pub purpose: String,
    pub repository: Repository,
    pub max_observation_age_seconds: u64,
    pub requirements: BTreeMap<String, Requirement>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    pub host: String,
    pub id: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Requirement {
    WorkflowReportedSuccess {
        source: WorkflowSource,
        selection: String,
    },
    CandidateApproval {
        source: ProviderSource,
        allowed_user_ids: Vec<u64>,
        minimum: u64,
        #[serde(default = "yes")]
        exclude_pr_author: bool,
    },
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowSource {
    pub provider: String,
    pub workflow_id: u64,
    pub event: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderSource {
    pub provider: String,
}

fn label(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || !value.starts_with(|c: char| c.is_ascii_alphabetic())
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
    {
        return Err(format!(
            "invalid requirement id {value:?}; start with a letter and use letters, digits, underscores or hyphens"
        ));
    }
    Ok(())
}

impl Requirements {
    /// Strict decode, validation and canonicalization of set-valued fields.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let mut requirements: Self = decode(bytes)?;
        requirements.validate()?;
        for requirement in requirements.requirements.values_mut() {
            if let Requirement::CandidateApproval {
                allowed_user_ids, ..
            } = requirement
            {
                let set: BTreeSet<u64> = allowed_user_ids.iter().copied().collect();
                *allowed_user_ids = set.into_iter().collect();
            }
        }
        Ok(requirements)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != REQUIREMENTS_SCHEMA {
            return Err(format!("expected schema {REQUIREMENTS_SCHEMA}"));
        }
        if self.purpose != "review-readiness" {
            return Err(format!(
                "unsupported purpose {:?}; only review-readiness is evaluated",
                self.purpose
            ));
        }
        if self.repository.host != "github.com" {
            return Err(format!(
                "unsupported host {:?}; only github.com is supported",
                self.repository.host
            ));
        }
        if self.repository.id == 0 {
            return Err("repository id must be positive".into());
        }
        if self.max_observation_age_seconds == 0
            || self.max_observation_age_seconds > MAX_OBSERVATION_AGE
        {
            return Err(format!(
                "max_observation_age_seconds must be between 1 and {MAX_OBSERVATION_AGE} (ten years)"
            ));
        }
        if self.requirements.is_empty() {
            return Err("requirements must not be empty".into());
        }
        for (id, requirement) in &self.requirements {
            label(id)?;
            match requirement {
                Requirement::WorkflowReportedSuccess { source, selection } => {
                    if source.provider != "github" {
                        return Err(format!("{id}: unsupported provider {:?}", source.provider));
                    }
                    if source.workflow_id == 0 {
                        return Err(format!("{id}: workflow_id must be positive"));
                    }
                    if source.event != "pull_request" {
                        return Err(format!(
                            "{id}: unsupported event {:?}; only pull_request is supported",
                            source.event
                        ));
                    }
                    if selection != "latest-run-latest-attempt" {
                        return Err(format!(
                            "{id}: unsupported selection {selection:?}; use latest-run-latest-attempt"
                        ));
                    }
                }
                Requirement::CandidateApproval {
                    source,
                    allowed_user_ids,
                    minimum,
                    ..
                } => {
                    if source.provider != "github" {
                        return Err(format!("{id}: unsupported provider {:?}", source.provider));
                    }
                    if allowed_user_ids.is_empty() || allowed_user_ids.contains(&0) {
                        return Err(format!(
                            "{id}: allowed_user_ids must be positive and non-empty"
                        ));
                    }
                    let distinct: BTreeSet<u64> = allowed_user_ids.iter().copied().collect();
                    if *minimum == 0 || *minimum as usize > distinct.len() {
                        return Err(format!(
                            "{id}: minimum must be between 1 and the number of allowed reviewers"
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn id(&self) -> Result<String, String> {
        identity(REQUIREMENTS_SCHEMA, self)
    }
}

// ----------------------------------------------------- candidate/request

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub repository: CandidateRepository,
    pub number: u64,
    pub head: Head,
    pub base: Base,
    pub author_user_id: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CandidateRepository {
    pub host: String,
    pub id: u64,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Head {
    pub repository_id: u64,
    pub commit: String,
    pub tree: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Base {
    pub repository_id: u64,
    pub commit: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema: String,
    pub purpose: String,
    pub candidate: Candidate,
    pub requirements: String,
}

impl Request {
    pub fn new(candidate: Candidate, requirements: &Requirements) -> Result<Self, String> {
        if candidate.repository.host != requirements.repository.host
            || candidate.repository.id != requirements.repository.id
        {
            return Err(format!(
                "requirements are bound to {}/{} but the candidate lives in {}/{} ({})",
                requirements.repository.host,
                requirements.repository.id,
                candidate.repository.host,
                candidate.repository.id,
                candidate.repository.name
            ));
        }
        Ok(Self {
            schema: REQUEST_SCHEMA.into(),
            purpose: requirements.purpose.clone(),
            candidate,
            requirements: format!("sha256:{}", requirements.id()?),
        })
    }

    /// The request identity binds purpose, requirements and the candidate's
    /// stable coordinates. The head tree and repository name are descriptive
    /// and can differ between collections of the same candidate, so they are
    /// carried in the assessment but do not enter this identity.
    pub fn id(&self) -> Result<String, String> {
        #[derive(Serialize)]
        struct Bound<'a> {
            schema: &'a str,
            purpose: &'a str,
            requirements: &'a str,
            host: &'a str,
            repository_id: u64,
            number: u64,
            head_repository_id: u64,
            head_commit: &'a str,
            base_repository_id: u64,
            base_commit: &'a str,
            author_user_id: u64,
        }
        let c = &self.candidate;
        identity(
            REQUEST_SCHEMA,
            &Bound {
                schema: &self.schema,
                purpose: &self.purpose,
                requirements: &self.requirements,
                host: &c.repository.host,
                repository_id: c.repository.id,
                number: c.number,
                head_repository_id: c.head.repository_id,
                head_commit: &c.head.commit,
                base_repository_id: c.base.repository_id,
                base_commit: &c.base.commit,
                author_user_id: c.author_user_id,
            },
        )
    }
}

// --------------------------------------------------------- observations

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Interval {
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Run {
    pub id: u64,
    pub workflow_id: u64,
    pub workflow_name: Option<String>,
    pub repository_id: u64,
    pub head_repository_id: Option<u64>,
    pub head_commit: String,
    pub event: String,
    pub run_number: u64,
    pub run_attempt: u64,
    pub status: String,
    pub conclusion: Option<String>,
    pub pull_requests: Vec<u64>,
    pub source: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Review {
    pub id: u64,
    pub user_id: Option<u64>,
    pub state: String,
    pub commit_id: Option<String>,
    pub submitted_at: Option<u64>,
    pub source: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Workflow {
    pub id: u64,
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Coverage {
    pub complete: bool,
    pub pages: u64,
    pub observed: u64,
    pub total: Option<u64>,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Gap {
    pub scope: String,
    pub code: String,
    pub detail: String,
}

/// Provider-neutral view of one collection. Sources reference the retained
/// raw object and the JSON pointer of the field an observation came from.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Observations {
    pub interval: Interval,
    pub runs: Vec<Run>,
    pub reviews: Vec<Review>,
    pub workflows: Vec<Workflow>,
    pub coverage: BTreeMap<String, Coverage>,
    pub gaps: Vec<Gap>,
}

// -------------------------------------------------------------- results

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Satisfied,
    Refuted,
    Unproven,
    Conflict,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Aggregate {
    Established,
    Refuted,
    Unproven,
    Conflict,
}

impl Aggregate {
    pub fn exit_code(self) -> u8 {
        match self {
            Aggregate::Established => 0,
            Aggregate::Refuted => 1,
            Aggregate::Unproven => 3,
            Aggregate::Conflict => 4,
        }
    }
    fn word(self) -> &'static str {
        match self {
            Aggregate::Established => "ESTABLISHED",
            Aggregate::Refuted => "REFUTED",
            Aggregate::Unproven => "UNPROVEN",
            Aggregate::Conflict => "CONFLICT",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub reference: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Exclusion {
    pub reference: String,
    pub label: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Obligation {
    pub kind: String,
    pub rule: String,
    pub subject: String,
    pub result: Verdict,
    pub reason: String,
    pub support: Vec<Evidence>,
    pub counterevidence: Vec<Evidence>,
    pub excluded: Vec<Exclusion>,
    pub missing: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Window {
    pub start: String,
    pub end: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assessment {
    pub schema: String,
    pub kind: String,
    pub request: String,
    pub collection: String,
    pub requirements: String,
    pub candidate: Candidate,
    pub result: Aggregate,
    pub obligations: BTreeMap<String, Obligation>,
    pub evaluated_at: String,
    pub evaluation_basis: String,
    pub collection_interval: Window,
    pub evaluator: String,
    pub trust: String,
    pub authenticity: String,
    pub mode: String,
    pub coverage: BTreeMap<String, Coverage>,
    pub gaps: Vec<Gap>,
    pub limitations: Vec<String>,
}

impl Assessment {
    pub fn id(&self) -> Result<String, String> {
        identity(ASSESSMENT_SCHEMA, self)
    }
}

fn run_label(run: &Run) -> String {
    format!(
        "GitHub run {} (number {}, attempt {}, {})",
        run.id, run.run_number, run.run_attempt, run.event
    )
}

fn review_label(review: &Review) -> String {
    format!(
        "GitHub review {} by account {} ({})",
        review.id,
        review
            .user_id
            .map_or("unknown".to_string(), |id| id.to_string()),
        review.state
    )
}

fn blocked(kind: &str, rule: &str, subject: String, gap: &Gap) -> Obligation {
    Obligation {
        kind: kind.into(),
        rule: rule.into(),
        subject,
        result: Verdict::Unproven,
        reason: gap.code.clone(),
        support: vec![],
        counterevidence: vec![],
        excluded: vec![],
        missing: Some(gap.detail.clone()),
    }
}

fn workflow_obligation(
    source: &WorkflowSource,
    candidate: &Candidate,
    observations: &Observations,
) -> Obligation {
    const KIND: &str = "workflow-reported-success";
    const RULE: &str = "workflow-reported-success.v1";
    let subject = format!(
        "workflow {} run for commit {} via {} in repository {}",
        source.workflow_id, candidate.head.commit, source.event, candidate.repository.id
    );
    let scope = "runs";
    if let Some(gap) = observations.gaps.iter().find(|g| g.scope == scope) {
        return blocked(KIND, RULE, subject, gap);
    }
    let mut obligation = Obligation {
        kind: KIND.into(),
        rule: RULE.into(),
        subject,
        result: Verdict::Unproven,
        reason: "run-missing".into(),
        support: vec![],
        counterevidence: vec![],
        excluded: vec![],
        missing: None,
    };
    let mut applicable: Vec<&Run> = vec![];
    for run in &observations.runs {
        let exclusion = if run.workflow_id != source.workflow_id {
            Some("wrong-workflow")
        } else if run.repository_id != candidate.repository.id {
            Some("wrong-repository")
        } else if run.head_commit != candidate.head.commit {
            Some("wrong-commit")
        } else if run.head_repository_id != Some(candidate.head.repository_id) {
            Some("wrong-head-repository")
        } else if run.event != source.event {
            Some("wrong-event")
        } else if run.pull_requests.is_empty() {
            Some("association-missing")
        } else if !run.pull_requests.contains(&candidate.number) {
            Some("wrong-pull-request")
        } else {
            None
        };
        match exclusion {
            Some(reason) => obligation.excluded.push(Exclusion {
                reference: run.source.clone(),
                label: run_label(run),
                reason: reason.into(),
            }),
            None => applicable.push(run),
        }
    }
    // The same run attempt listed twice (a listing that shifted between
    // pages) is one record when the copies agree and a contradiction when
    // they do not. Distinct attempts are history, not conflict.
    let mut by_identity: BTreeMap<(u64, u64), &Run> = BTreeMap::new();
    for run in &applicable {
        let previous = by_identity.insert((run.id, run.run_attempt), run);
        if let Some(previous) =
            previous.filter(|p| p.status != run.status || p.conclusion != run.conclusion)
        {
            obligation.result = Verdict::Conflict;
            obligation.reason = "contradictory-run-reports".into();
            obligation.counterevidence = vec![
                Evidence {
                    reference: previous.source.clone(),
                    label: run_label(previous),
                },
                Evidence {
                    reference: run.source.clone(),
                    label: run_label(run),
                },
            ];
            obligation.missing =
                Some("two admitted reports of the same run attempt disagree".into());
            return obligation;
        }
    }
    let applicable: Vec<&Run> = by_identity.into_values().collect();
    let coverage = observations.coverage.get(scope);
    if !coverage.is_some_and(|c| c.complete) {
        obligation.reason = "selection-incomplete".into();
        obligation.missing = Some(match coverage.and_then(|c| c.note.clone()) {
            Some(note) => format!("run enumeration incomplete: {note}"),
            None => "run enumeration incomplete".into(),
        });
        return obligation;
    }
    if applicable.is_empty() {
        if obligation
            .excluded
            .iter()
            .any(|e| e.reason == "association-missing")
        {
            // GitHub records no pull-request association for runs triggered
            // from forks; the run exists but cannot be bound to this candidate.
            obligation.reason = "association-missing".into();
            obligation.missing = Some(format!(
                "a matching run exists but the provider associates it with no pull request; runs triggered from forks cannot satisfy this requirement for #{}",
                candidate.number
            ));
            return obligation;
        }
        obligation.missing = Some(format!(
            "no {} run of workflow {} for commit {} associated with pull request #{}",
            source.event, source.workflow_id, candidate.head.commit, candidate.number
        ));
        return obligation;
    }
    let greatest = applicable.iter().map(|r| r.run_number).max().unwrap_or(0);
    let mut latest: Vec<&Run> = applicable
        .iter()
        .copied()
        .filter(|r| r.run_number == greatest)
        .collect();
    let ids: BTreeSet<u64> = latest.iter().map(|r| r.id).collect();
    if ids.len() != 1 {
        obligation.reason = "selection-ambiguous".into();
        obligation.missing = Some(format!(
            "runs {:?} share run number {greatest}; ordering is ambiguous",
            ids
        ));
        return obligation;
    }
    let attempt = latest.iter().map(|r| r.run_attempt).max().unwrap_or(0);
    latest.sort_by_key(|r| std::cmp::Reverse(r.run_attempt));
    let selected = latest[0];
    for run in applicable.iter().filter(|r| r.id != selected.id) {
        obligation.excluded.push(Exclusion {
            reference: run.source.clone(),
            label: run_label(run),
            reason: "superseded".into(),
        });
    }
    for run in latest.iter().skip(1) {
        if run.run_attempt != attempt {
            obligation.excluded.push(Exclusion {
                reference: run.source.clone(),
                label: run_label(run),
                reason: "superseded".into(),
            });
        }
    }
    let evidence = Evidence {
        reference: selected.source.clone(),
        label: run_label(selected),
    };
    match (selected.status.as_str(), selected.conclusion.as_deref()) {
        ("completed", Some("success")) => {
            obligation.result = Verdict::Satisfied;
            obligation.reason = "provider-reported-success".into();
            obligation.support.push(evidence);
        }
        ("completed", Some("failure")) => {
            obligation.result = Verdict::Refuted;
            obligation.reason = "provider-reported-failure".into();
            obligation.counterevidence.push(evidence);
        }
        (status, conclusion) => {
            obligation.reason = format!(
                "provider-outcome:{}",
                if status == "completed" {
                    conclusion.unwrap_or("unknown")
                } else {
                    status
                }
            );
            obligation.missing = Some(format!(
                "{} is the latest attempt; it has not completed successfully",
                evidence.label
            ));
            obligation.excluded.push(Exclusion {
                reference: evidence.reference,
                label: evidence.label,
                reason: "not-terminal-success".into(),
            });
        }
    }
    obligation
}

fn approval_obligation(
    allowed: &[u64],
    minimum: u64,
    exclude_author: bool,
    candidate: &Candidate,
    observations: &Observations,
) -> Obligation {
    const KIND: &str = "candidate-approval";
    const RULE: &str = "candidate-approval.v1";
    let subject = format!(
        "approval of commit {} in pull request #{} by {minimum} of allowed accounts {:?}",
        candidate.head.commit, candidate.number, allowed
    );
    let scope = "reviews";
    if let Some(gap) = observations.gaps.iter().find(|g| g.scope == scope) {
        return blocked(KIND, RULE, subject, gap);
    }
    let mut obligation = Obligation {
        kind: KIND.into(),
        rule: RULE.into(),
        subject,
        result: Verdict::Unproven,
        reason: "approval-missing".into(),
        support: vec![],
        counterevidence: vec![],
        excluded: vec![],
        missing: None,
    };
    let coverage = observations.coverage.get(scope);
    if !coverage.is_some_and(|c| c.complete) {
        obligation.reason = "selection-incomplete".into();
        obligation.missing = Some(match coverage.and_then(|c| c.note.clone()) {
            Some(note) => format!("review enumeration incomplete: {note}"),
            None => "review enumeration incomplete".into(),
        });
        return obligation;
    }
    let mut decisive: BTreeMap<u64, Vec<&Review>> = BTreeMap::new();
    let mut seen: BTreeMap<u64, &Review> = BTreeMap::new();
    for review in &observations.reviews {
        // The same review listed twice (a listing that shifted between pages)
        // is one record, not two; differing copies are a contradiction.
        let previous = seen.insert(review.id, review);
        if let Some(previous) = previous {
            if previous.state == review.state && previous.commit_id == review.commit_id {
                continue;
            }
            obligation.result = Verdict::Conflict;
            obligation.reason = "contradictory-review-reports".into();
            obligation.counterevidence = vec![
                Evidence {
                    reference: previous.source.clone(),
                    label: review_label(previous),
                },
                Evidence {
                    reference: review.source.clone(),
                    label: review_label(review),
                },
            ];
            obligation.missing = Some("two admitted reports of the same review disagree".into());
            return obligation;
        }
        let Some(user) = review.user_id else {
            obligation.excluded.push(Exclusion {
                reference: review.source.clone(),
                label: review_label(review),
                reason: "reviewer-unknown".into(),
            });
            continue;
        };
        if !allowed.contains(&user) {
            obligation.excluded.push(Exclusion {
                reference: review.source.clone(),
                label: review_label(review),
                reason: "reviewer-not-allowed".into(),
            });
            continue;
        }
        match review.state.as_str() {
            "APPROVED" | "CHANGES_REQUESTED" | "DISMISSED" => {}
            "PENDING" | "COMMENTED" => {
                obligation.excluded.push(Exclusion {
                    reference: review.source.clone(),
                    label: review_label(review),
                    reason: "not-decisive".into(),
                });
                continue;
            }
            other => {
                obligation.reason = "unsupported-review-state".into();
                obligation.missing = Some(format!(
                    "review {} has unsupported state {other:?}",
                    review.id
                ));
                return obligation;
            }
        }
        if review.submitted_at.is_none() {
            obligation.reason = "ordering-ambiguous".into();
            obligation.missing = Some(format!(
                "review {} has no submission time; its order is unknown",
                review.id
            ));
            return obligation;
        }
        decisive.entry(user).or_default().push(review);
    }
    let mut approvers: BTreeSet<u64> = BTreeSet::new();
    let mut blocked_by_changes = false;
    for (user, mut reviews) in decisive {
        reviews.sort_by_key(|r| (r.submitted_at, r.id));
        let latest = *reviews.last().expect("non-empty");
        if reviews.len() > 1 && reviews[reviews.len() - 2].submitted_at == latest.submitted_at {
            obligation.reason = "ordering-ambiguous".into();
            obligation.missing = Some(format!(
                "account {user} submitted two decisive reviews at the same second"
            ));
            return obligation;
        }
        for review in &reviews[..reviews.len() - 1] {
            obligation.excluded.push(Exclusion {
                reference: review.source.clone(),
                label: review_label(review),
                reason: "superseded".into(),
            });
        }
        let evidence = Evidence {
            reference: latest.source.clone(),
            label: review_label(latest),
        };
        match latest.state.as_str() {
            "DISMISSED" => obligation.excluded.push(Exclusion {
                reference: evidence.reference,
                label: evidence.label,
                reason: "dismissed".into(),
            }),
            "CHANGES_REQUESTED" => {
                blocked_by_changes = true;
                obligation.counterevidence.push(evidence);
            }
            _ if latest.commit_id.as_deref() != Some(candidate.head.commit.as_str()) => {
                obligation.excluded.push(Exclusion {
                    reference: evidence.reference,
                    label: evidence.label,
                    reason: "stale-approval".into(),
                })
            }
            _ if exclude_author && user == candidate.author_user_id => {
                obligation.excluded.push(Exclusion {
                    reference: evidence.reference,
                    label: evidence.label,
                    reason: "self-review".into(),
                })
            }
            _ => {
                approvers.insert(user);
                obligation.support.push(evidence);
            }
        }
    }
    if blocked_by_changes {
        obligation.result = Verdict::Refuted;
        obligation.reason = "changes-requested".into();
        obligation.missing = Some("an allowed reviewer's change request is still active".into());
    } else if approvers.len() as u64 >= minimum {
        obligation.result = Verdict::Satisfied;
        obligation.reason = "approval-present".into();
    } else {
        let remaining: Vec<&u64> = allowed
            .iter()
            .filter(|u| {
                !(approvers.contains(u) || exclude_author && **u == candidate.author_user_id)
            })
            .collect();
        let needed = minimum - approvers.len() as u64;
        obligation.missing = Some(if remaining.is_empty() {
            format!(
                "{needed} more approval(s) of commit {}, but every allowed account is already counted or excluded as the author; the requirements name no eligible reviewer",
                candidate.head.commit
            )
        } else {
            format!(
                "{needed} more approval(s) of commit {} from allowed accounts {remaining:?}",
                candidate.head.commit
            )
        });
    }
    obligation
}

/// Evaluate a request against one collection's observations. `at` defaults to
/// the collection's end, the historical decision time; an explicit later time
/// can make the whole collection stale.
pub fn evaluate(
    requirements: &Requirements,
    request: &Request,
    collection: &str,
    observations: &Observations,
    at: Option<u64>,
) -> Result<Assessment, String> {
    let candidate = &request.candidate;
    let expected = format!("sha256:{}", requirements.id()?);
    if request.requirements != expected || request.purpose != requirements.purpose {
        return Err("request does not bind these requirements".into());
    }
    let interval = &observations.interval;
    if interval.end < interval.start {
        return Err("collection interval ends before it starts".into());
    }
    let (evaluated_at, basis) = match at {
        None => (interval.end, "collection-end"),
        Some(at) if at < interval.start => {
            return Err(format!(
                "evaluation time {} precedes the collection start {}",
                format_time(at),
                format_time(interval.start)
            ));
        }
        Some(at) => (at, "explicit"),
    };
    let mut blocker: Option<Gap> = observations
        .gaps
        .iter()
        .find(|g| g.scope == "candidate")
        .cloned();
    if blocker.is_none()
        && evaluated_at
            > interval
                .end
                .saturating_add(requirements.max_observation_age_seconds)
    {
        blocker = Some(Gap {
            scope: "candidate".into(),
            code: "stale".into(),
            detail: format!(
                "observations ended at {} and the allowance is {} seconds; at {} they are historical only",
                format_time(interval.end),
                requirements.max_observation_age_seconds,
                format_time(evaluated_at)
            ),
        });
    }
    let mut obligations = BTreeMap::new();
    for (id, requirement) in &requirements.requirements {
        let obligation = match requirement {
            Requirement::WorkflowReportedSuccess { source, .. } => {
                workflow_obligation(source, candidate, observations)
            }
            Requirement::CandidateApproval {
                allowed_user_ids,
                minimum,
                exclude_pr_author,
                ..
            } => approval_obligation(
                allowed_user_ids,
                *minimum,
                *exclude_pr_author,
                candidate,
                observations,
            ),
        };
        let obligation = match &blocker {
            Some(gap) => blocked(&obligation.kind, &obligation.rule, obligation.subject, gap),
            None => obligation,
        };
        obligations.insert(id.clone(), obligation);
    }
    let result = if obligations.values().any(|o| o.result == Verdict::Conflict) {
        Aggregate::Conflict
    } else if obligations.values().any(|o| o.result == Verdict::Refuted) {
        Aggregate::Refuted
    } else if obligations.values().all(|o| o.result == Verdict::Satisfied) {
        Aggregate::Established
    } else {
        Aggregate::Unproven
    };
    let mut limitations = vec![
        "Workflow success is the provider's report for the selected run attempt; it does not establish which jobs or tests ran, nor that the candidate tree was the executed checkout.".to_string(),
        "Reviewer account IDs establish account identity, not independence from the author.".to_string(),
        "Raw responses come from the local collector and store; digests detect accidental change, not a replaced bundle.".to_string(),
        "No merge or deployment authorization follows from this assessment; a consumer must reacquire state and enforce its own preconditions.".to_string(),
    ];
    if basis == "explicit" {
        limitations.push(
            "Evaluated at an explicit time, not at the collection end; this replays supplied evidence."
                .into(),
        );
    }
    Ok(Assessment {
        schema: ASSESSMENT_SCHEMA.into(),
        kind: "assessment".into(),
        request: format!("sha256:{}", request.id()?),
        collection: collection.into(),
        requirements: expected,
        candidate: candidate.clone(),
        result,
        obligations,
        evaluated_at: format_time(evaluated_at),
        evaluation_basis: basis.into(),
        collection_interval: Window {
            start: format_time(interval.start),
            end: format_time(interval.end),
        },
        evaluator: EVALUATOR.into(),
        trust: TRUST.into(),
        authenticity: AUTHENTICITY.into(),
        mode: MODE.into(),
        coverage: observations.coverage.clone(),
        gaps: observations.gaps.clone(),
        limitations,
    })
}

// ---------------------------------------------------------- projections

fn mark(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Satisfied => "✓",
        Verdict::Refuted => "✗",
        Verdict::Unproven => "?",
        Verdict::Conflict => "‼",
    }
}

/// One sentence per reason code, so rows and explanations agree.
pub fn describe(obligation: &Obligation) -> String {
    match obligation.reason.as_str() {
        "provider-reported-success" => "GitHub reports success for the required workflow".into(),
        "provider-reported-failure" => "GitHub reports failure for the latest run attempt".into(),
        "run-missing" => "No run of the required workflow is associated with this candidate".into(),
        "association-missing" => {
            "A matching run exists but GitHub associates it with no pull request (fork runs)".into()
        }
        "selection-incomplete" => {
            "The provider listing is incomplete; the latest run or review cannot be selected".into()
        }
        "selection-ambiguous" => "Run ordering is ambiguous; no run can be selected".into(),
        "contradictory-run-reports" => "Two reports of the same run attempt disagree".into(),
        "contradictory-review-reports" => "Two reports of the same review disagree".into(),
        "approval-present" => "Allowed reviewers approved this exact candidate".into(),
        "approval-missing" => "No allowed reviewer has approved this candidate".into(),
        "changes-requested" => "An allowed reviewer's change request is still active".into(),
        "ordering-ambiguous" => "Review ordering is ambiguous".into(),
        "unsupported-review-state" => {
            "A review has a state this evaluator does not understand".into()
        }
        "candidate-moved" => "The pull request changed while it was being read".into(),
        "race" => "Runs or reviews changed while they were being read".into(),
        "stale" => "Observations are older than the configured allowance".into(),
        "provider-unavailable" => format!(
            "GitHub did not answer the query ({})",
            obligation.missing.as_deref().unwrap_or("no detail")
        ),
        "provider-denied" => format!(
            "GitHub refused the query; check the gh login's access ({})",
            obligation.missing.as_deref().unwrap_or("no detail")
        ),
        "unsupported-response" => "GitHub answered with a body this reader cannot interpret".into(),
        "confirmation-missing" => {
            "Runs or reviews could not be re-read, so change during acquisition is unknown".into()
        }
        "candidate-unconfirmed" => {
            "The pull request could not be re-read, so change during acquisition is unknown".into()
        }
        reason if reason.starts_with("provider-outcome:") => format!(
            "The latest run attempt is {}; not a completed success",
            reason.trim_start_matches("provider-outcome:")
        ),
        other => format!("Unproven: {other}"),
    }
}

pub fn render(assessment: &Assessment, bundle: &str, requirements_path: &str) -> String {
    let mut out = format!(
        "{} — {}\nScope: declared {} conditions; {}\n\n",
        short(&assessment.candidate.head.commit),
        assessment.result.word(),
        assessment
            .evaluator
            .strip_suffix(".v1")
            .unwrap_or(&assessment.evaluator),
        assessment.mode
    );
    let width = assessment
        .obligations
        .keys()
        .map(String::len)
        .max()
        .unwrap_or(0);
    for (id, obligation) in &assessment.obligations {
        out.push_str(&format!(
            "{} {id:width$}  {}\n",
            mark(obligation.result),
            describe(obligation)
        ));
    }
    let (start, end) = (
        parse_time(&assessment.collection_interval.start).unwrap_or(0),
        parse_time(&assessment.collection_interval.end).unwrap_or(0),
    );
    let day = |text: &str| text.get(..10).unwrap_or(text).to_string();
    let (start_day, end_day) = (
        day(&assessment.collection_interval.start),
        day(&assessment.collection_interval.end),
    );
    out.push_str(&format!(
        "\nEvidence window: {} {}–{} {} UTC\n",
        start_day,
        clock(start),
        if end_day == start_day {
            String::new()
        } else {
            format!("{end_day} ")
        },
        clock(end)
    ));
    if assessment.evaluation_basis == "explicit" {
        out.push_str(&format!(
            "Evaluated at: {} (explicit; replaying supplied evidence)\n",
            assessment.evaluated_at
        ));
    }
    for gap in &assessment.gaps {
        out.push_str(&format!(
            "Gap: {} ({}): {}\n",
            gap.code, gap.scope, gap.detail
        ));
    }
    out.push_str("Trust: local collector and store; receipt is not authenticated\n");
    let first = assessment
        .obligations
        .iter()
        .find(|(_, o)| o.result != Verdict::Satisfied)
        .or_else(|| assessment.obligations.iter().next())
        .map(|(id, _)| id.as_str())
        .unwrap_or("ID");
    out.push_str(&format!(
        "Details: sykli assess {bundle} --requirements {requirements_path} --why {first}\n"
    ));
    out
}

pub fn explain(assessment: &Assessment, id: &str) -> Result<String, String> {
    let obligation = assessment
        .obligations
        .get(id)
        .ok_or_else(|| format!("unknown obligation {id:?}"))?;
    let mut out = format!(
        "{id} — {} — {} ({})\nSubject: {}\nRule: {}\n",
        obligation.kind,
        serde_json::to_value(obligation.result)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default(),
        obligation.reason,
        obligation.subject,
        obligation.rule
    );
    let list = |title: &str, items: &[Evidence]| -> String {
        if items.is_empty() {
            return format!("{title}: none\n");
        }
        let mut text = format!("{title}:\n");
        for item in items {
            text.push_str(&format!("  {}  {}\n", item.reference, item.label));
        }
        text
    };
    out.push_str(&list("Support", &obligation.support));
    out.push_str(&list("Counterevidence", &obligation.counterevidence));
    if obligation.excluded.is_empty() {
        out.push_str("Excluded: none\n");
    } else {
        out.push_str("Excluded:\n");
        for item in &obligation.excluded {
            out.push_str(&format!(
                "  {}  {}: {}\n",
                item.reference, item.label, item.reason
            ));
        }
    }
    out.push_str(&format!(
        "Missing: {}\n",
        obligation.missing.as_deref().unwrap_or("nothing")
    ));
    if assessment.gaps.is_empty() {
        out.push_str("Gaps: none\n");
    } else {
        out.push_str("Gaps:\n");
        for gap in &assessment.gaps {
            out.push_str(&format!("  {} ({}): {}\n", gap.code, gap.scope, gap.detail));
        }
    }
    Ok(out)
}

fn mermaid_label(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '#' => out.push_str("#35;"),
            '"' => out.push_str("#quot;"),
            '<' => out.push_str("#lt;"),
            '>' => out.push_str("#gt;"),
            '&' => out.push_str("#amp;"),
            ';' => out.push_str("#59;"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

fn mermaid_id(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Shallow projection: candidate → obligations → aggregate, with support and
/// counterevidence as evidence nodes. Arrows mean support or requirement, never order.
pub fn mermaid(assessment: &Assessment) -> String {
    let mut out = String::from("flowchart BT\n");
    out.push_str(&format!(
        "    C[\"Candidate {}\"]\n    A[\"{}: {}\"]\n",
        mermaid_label(&short(&assessment.candidate.head.commit)),
        mermaid_label(
            assessment
                .evaluator
                .strip_suffix(".v1")
                .unwrap_or(&assessment.evaluator)
        ),
        assessment.result.word()
    ));
    let mut evidence_index = 0;
    for (id, obligation) in &assessment.obligations {
        // Positional ids: `ci-lint` and `ci_lint` must not share a node.
        let node = format!(
            "O{}",
            assessment
                .obligations
                .keys()
                .position(|k| k == id)
                .unwrap_or(0)
        );
        let _ = mermaid_id;
        let verdict = serde_json::to_value(obligation.result)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
        out.push_str(&format!(
            "    {node}[\"{}: {} — {verdict} ({})\"]\n    C -. \"subject\" .-> {node}\n    {node} --> A\n",
            mermaid_label(id),
            mermaid_label(&obligation.kind),
            mermaid_label(&obligation.reason)
        ));
        for (edge, items) in [
            ("supports", &obligation.support),
            ("refutes", &obligation.counterevidence),
        ] {
            for item in items {
                out.push_str(&format!(
                    "    E{evidence_index}[\"{}\"] -->|{edge}| {node}\n",
                    mermaid_label(&item.label)
                ));
                evidence_index += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "0e1982a239a0bd02299ab17de29bd52d17842763";
    const BASE: &str = "6c13a679d35f70b8fc2e90d78a47831a31aa9eaa";
    const OLD: &str = "3d901a9000000000000000000000000000000000";

    fn requirements_json() -> &'static str {
        r#"{"schema":"sykli-requirements.v1","purpose":"review-readiness","repository":{"host":"github.com","id":1323443147},"max_observation_age_seconds":300,"requirements":{"ci":{"kind":"workflow-reported-success","source":{"provider":"github","workflow_id":327406134,"event":"pull_request"},"selection":"latest-run-latest-attempt"},"review":{"kind":"candidate-approval","source":{"provider":"github"},"allowed_user_ids":[789,42],"minimum":1,"exclude_pr_author":true}}}"#
    }

    fn requirements() -> Requirements {
        Requirements::parse(requirements_json().as_bytes()).unwrap()
    }

    fn candidate() -> Candidate {
        Candidate {
            repository: CandidateRepository {
                host: "github.com".into(),
                id: 1323443147,
                name: "false-systems/sykli".into(),
            },
            number: 25,
            head: Head {
                repository_id: 1323443147,
                commit: HEAD.into(),
                tree: Some("a40a1e78ad2e84e05bc49ba7ab9c95e8efaedd39".into()),
            },
            base: Base {
                repository_id: 1323443147,
                commit: BASE.into(),
            },
            author_user_id: 154441282,
        }
    }

    fn run(id: u64, number: u64, attempt: u64, status: &str, conclusion: Option<&str>) -> Run {
        Run {
            id,
            workflow_id: 327406134,
            workflow_name: Some("CI".into()),
            repository_id: 1323443147,
            head_repository_id: Some(1323443147),
            head_commit: HEAD.into(),
            event: "pull_request".into(),
            run_number: number,
            run_attempt: attempt,
            status: status.into(),
            conclusion: conclusion.map(String::from),
            pull_requests: vec![25],
            source: format!("sha256:runs#/workflow_runs/{id}"),
        }
    }

    fn review(id: u64, user: u64, state: &str, commit: &str, at: u64) -> Review {
        Review {
            id,
            user_id: Some(user),
            state: state.into(),
            commit_id: Some(commit.into()),
            submitted_at: Some(at),
            source: format!("sha256:reviews#/{id}"),
        }
    }

    fn observations(runs: Vec<Run>, reviews: Vec<Review>) -> Observations {
        let mut coverage = BTreeMap::new();
        for scope in ["runs", "reviews"] {
            coverage.insert(
                scope.to_string(),
                Coverage {
                    complete: true,
                    pages: 1,
                    observed: 0,
                    total: None,
                    note: None,
                },
            );
        }
        Observations {
            interval: Interval {
                start: 1_788_908_000,
                end: 1_788_908_004,
            },
            runs,
            reviews,
            workflows: vec![],
            coverage,
            gaps: vec![],
        }
    }

    fn assess(observations: &Observations, at: Option<u64>) -> Assessment {
        let requirements = requirements();
        let request = Request::new(candidate(), &requirements).unwrap();
        evaluate(
            &requirements,
            &request,
            "sha256:collection",
            observations,
            at,
        )
        .unwrap()
    }

    #[test]
    fn time_round_trips() {
        for text in [
            "1970-01-01T00:00:00Z",
            "2026-09-09T14:32:04Z",
            "2000-02-29T23:59:59Z",
        ] {
            assert_eq!(format_time(parse_time(text).unwrap()), text);
        }
        assert_eq!(
            parse_time("2026-09-09T14:32:04.123Z").unwrap(),
            parse_time("2026-09-09T14:32:04Z").unwrap()
        );
        for bad in [
            "2026-09-09T14:32:04",
            "2026-13-01T00:00:00Z",
            "2026-02-29T00:00:00Z",
            "2026-09-09 14:32:04Z",
            "1969-12-31T23:59:59Z",
        ] {
            assert!(parse_time(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn requirements_reject_unsupported_shapes() {
        let cases = [
            (
                r#"{"schema":"sykli-requirements.v1","purpose":"review-readiness","repository":{"host":"github.com","id":1},"max_observation_age_seconds":300,"requirements":{}}"#,
                "empty",
            ),
            (
                r#"{"schema":"sykli-requirements.v1","purpose":"review-readiness","repository":{"host":"github.com","id":1},"max_observation_age_seconds":0,"requirements":{"ci":{"kind":"workflow-reported-success","source":{"provider":"github","workflow_id":1,"event":"pull_request"},"selection":"latest-run-latest-attempt"}}}"#,
                "between 1 and",
            ),
            (
                r#"{"schema":"sykli-requirements.v1","purpose":"review-readiness","repository":{"host":"github.com","id":1},"max_observation_age_seconds":18446744073709551615,"requirements":{"ci":{"kind":"workflow-reported-success","source":{"provider":"github","workflow_id":1,"event":"pull_request"},"selection":"latest-run-latest-attempt"}}}"#,
                "between 1 and",
            ),
            (
                r#"{"schema":"sykli-requirements.v1","purpose":"review-readiness","repository":{"host":"github.com","id":1},"max_observation_age_seconds":300,"requirements":{"ci":{"kind":"tests-passed","source":{"provider":"github"}}}}"#,
                "unknown variant",
            ),
            (
                r#"{"schema":"sykli-requirements.v1","purpose":"review-readiness","repository":{"host":"github.com","id":1},"max_observation_age_seconds":300,"requirements":{"ci":{"kind":"workflow-reported-success","source":{"provider":"github","workflow_id":1,"event":"pull_request"},"selection":"any-green"}}}"#,
                "selection",
            ),
            (
                r#"{"schema":"sykli-requirements.v1","purpose":"review-readiness","repository":{"host":"github.com","id":1},"max_observation_age_seconds":300,"requirements":{"ci":{"kind":"workflow-reported-success","source":{"provider":"github","workflow_id":1,"event":"pull_request_target"},"selection":"latest-run-latest-attempt"}}}"#,
                "event",
            ),
            (
                r#"{"schema":"sykli-requirements.v1","purpose":"review-readiness","repository":{"host":"github.com","id":1},"max_observation_age_seconds":300,"complete":true,"requirements":{"ci":{"kind":"workflow-reported-success","source":{"provider":"github","workflow_id":1,"event":"pull_request"},"selection":"latest-run-latest-attempt"}}}"#,
                "unknown field",
            ),
            (
                r#"{"schema":"sykli-requirements.v1","purpose":"review-readiness","repository":{"host":"github.com","id":1},"max_observation_age_seconds":300,"requirements":{"ci":{"kind":"workflow-reported-success","source":{"provider":"github","workflow_id":1,"event":"pull_request"},"selection":"latest-run-latest-attempt"},"ci":{"kind":"workflow-reported-success","source":{"provider":"github","workflow_id":2,"event":"pull_request"},"selection":"latest-run-latest-attempt"}}}"#,
                "duplicate key",
            ),
            (
                r#"{"schema":"sykli-requirements.v1","purpose":"review-readiness","repository":{"host":"github.com","id":1},"max_observation_age_seconds":300,"requirements":{"review":{"kind":"candidate-approval","source":{"provider":"github"},"allowed_user_ids":[7],"minimum":2}}}"#,
                "minimum",
            ),
            (
                r#"{"schema":"sykli-requirements.v1","purpose":"review-readiness","repository":{"host":"github.com","id":1},"max_observation_age_seconds":300,"requirements":{"review":{"kind":"candidate-approval","source":{"provider":"github"},"allowed_user_ids":[-7],"minimum":1}}}"#,
                "invalid",
            ),
        ];
        for (json, expected) in cases {
            let error = Requirements::parse(json.as_bytes()).unwrap_err();
            assert!(
                error.to_lowercase().contains(expected),
                "{expected}: {error}"
            );
        }
    }

    #[test]
    fn requirements_identity_is_canonical() {
        let a = requirements();
        let shuffled = requirements_json()
            .replace(
                r#""allowed_user_ids":[789,42]"#,
                r#""allowed_user_ids":[42,789,42]"#,
            )
            .replace(
                r#""exclude_pr_author":true"#,
                r#""exclude_pr_author": true "#,
            );
        let b = Requirements::parse(shuffled.as_bytes()).unwrap();
        assert_eq!(a.id().unwrap(), b.id().unwrap());
        let lowered = requirements_json().replace(
            r#""exclude_pr_author":true"#,
            r#""exclude_pr_author":false"#,
        );
        let c = Requirements::parse(lowered.as_bytes()).unwrap();
        assert_ne!(
            a.id().unwrap(),
            c.id().unwrap(),
            "changed rules have a new identity"
        );
    }

    #[test]
    fn request_rejects_foreign_repository() {
        let mut candidate = candidate();
        candidate.repository.id = 99;
        assert!(Request::new(candidate, &requirements()).is_err());
    }

    #[test]
    fn green_run_and_no_review_is_unproven() {
        let assessment = assess(
            &observations(vec![run(1, 65, 1, "completed", Some("success"))], vec![]),
            None,
        );
        assert_eq!(assessment.result, Aggregate::Unproven);
        assert_eq!(assessment.obligations["ci"].result, Verdict::Satisfied);
        assert_eq!(assessment.obligations["review"].reason, "approval-missing");
        assert_eq!(assessment.evaluation_basis, "collection-end");
        assert_eq!(assessment.evaluated_at, "2026-09-08T22:53:24Z");
        assert_eq!(assessment.result.exit_code(), 3);
    }

    #[test]
    fn wrong_commit_fork_or_workflow_cannot_satisfy() {
        let mut old = run(1, 64, 1, "completed", Some("success"));
        old.head_commit = OLD.into();
        let mut fork = run(2, 66, 1, "completed", Some("success"));
        fork.head_repository_id = Some(555);
        let mut other = run(3, 67, 1, "completed", Some("success"));
        other.workflow_id = 330221374;
        let mut unassociated = run(4, 68, 1, "completed", Some("success"));
        unassociated.pull_requests.clear();
        let assessment = assess(
            &observations(vec![old, fork, other, unassociated], vec![]),
            None,
        );
        let ci = &assessment.obligations["ci"];
        assert_eq!(ci.result, Verdict::Unproven);
        assert_eq!(
            ci.reason, "association-missing",
            "an unassociated fork run is named, not reported as absent"
        );
        assert!(describe(ci).contains("fork"));
        let reasons: Vec<&str> = ci.excluded.iter().map(|e| e.reason.as_str()).collect();
        assert_eq!(
            reasons,
            [
                "wrong-commit",
                "wrong-head-repository",
                "wrong-workflow",
                "association-missing"
            ]
        );
    }

    #[test]
    fn newer_unresolved_run_hides_older_green() {
        let assessment = assess(
            &observations(
                vec![
                    run(1, 65, 1, "completed", Some("success")),
                    run(2, 66, 1, "in_progress", None),
                ],
                vec![],
            ),
            None,
        );
        let ci = &assessment.obligations["ci"];
        assert_eq!(ci.result, Verdict::Unproven);
        assert_eq!(ci.reason, "provider-outcome:in_progress");
        assert!(ci.excluded.iter().any(|e| e.reason == "superseded"));
        let cancelled = assess(
            &observations(vec![run(2, 66, 1, "completed", Some("cancelled"))], vec![]),
            None,
        );
        assert_eq!(
            cancelled.obligations["ci"].reason,
            "provider-outcome:cancelled"
        );
        let failed = assess(
            &observations(vec![run(2, 66, 2, "completed", Some("failure"))], vec![]),
            None,
        );
        assert_eq!(failed.obligations["ci"].result, Verdict::Refuted);
        assert_eq!(failed.result, Aggregate::Refuted);
    }

    #[test]
    fn incomplete_enumeration_is_unproven_even_with_green() {
        let mut observations =
            observations(vec![run(1, 65, 1, "completed", Some("success"))], vec![]);
        observations.coverage.get_mut("runs").unwrap().complete = false;
        observations.coverage.get_mut("runs").unwrap().note = Some("page cap".into());
        let assessment = assess(&observations, None);
        assert_eq!(assessment.obligations["ci"].reason, "selection-incomplete");
    }

    #[test]
    fn same_run_number_different_ids_is_ambiguous() {
        let assessment = assess(
            &observations(
                vec![
                    run(1, 65, 1, "completed", Some("success")),
                    run(2, 65, 1, "completed", Some("success")),
                ],
                vec![],
            ),
            None,
        );
        assert_eq!(assessment.obligations["ci"].reason, "selection-ambiguous");
    }

    #[test]
    fn contradictory_reports_of_one_attempt_conflict() {
        let assessment = assess(
            &observations(
                vec![
                    run(1, 65, 1, "completed", Some("success")),
                    run(1, 65, 1, "completed", Some("failure")),
                ],
                vec![],
            ),
            None,
        );
        assert_eq!(assessment.obligations["ci"].result, Verdict::Conflict);
        assert_eq!(assessment.result, Aggregate::Conflict);
        assert_eq!(assessment.result.exit_code(), 4);
    }

    #[test]
    fn review_selection_is_exact() {
        let green = run(1, 65, 1, "completed", Some("success"));
        // Approval on the old candidate: stale.
        let stale = assess(
            &observations(
                vec![green.clone()],
                vec![review(1, 789, "APPROVED", OLD, 10)],
            ),
            None,
        );
        assert_eq!(
            stale.obligations["review"].excluded[0].reason,
            "stale-approval"
        );
        // Approval then changes requested: refuted.
        let changed = assess(
            &observations(
                vec![green.clone()],
                vec![
                    review(1, 789, "APPROVED", HEAD, 10),
                    review(2, 789, "CHANGES_REQUESTED", HEAD, 20),
                ],
            ),
            None,
        );
        assert_eq!(changed.obligations["review"].result, Verdict::Refuted);
        assert_eq!(changed.result, Aggregate::Refuted);
        // Dismissed latest record does not resurrect an older approval.
        let dismissed = assess(
            &observations(
                vec![green.clone()],
                vec![
                    review(1, 789, "APPROVED", HEAD, 10),
                    review(2, 789, "DISMISSED", HEAD, 20),
                ],
            ),
            None,
        );
        assert_eq!(dismissed.obligations["review"].reason, "approval-missing");
        // Self-review by the author, even if listed as allowed.
        let mut requirements = requirements();
        if let Requirement::CandidateApproval {
            allowed_user_ids, ..
        } = requirements.requirements.get_mut("review").unwrap()
        {
            allowed_user_ids.push(154441282);
        }
        let request = Request::new(candidate(), &requirements).unwrap();
        let selfie = evaluate(
            &requirements,
            &request,
            "sha256:c",
            &observations(
                vec![green.clone()],
                vec![review(1, 154441282, "APPROVED", HEAD, 10)],
            ),
            None,
        )
        .unwrap();
        assert_eq!(
            selfie.obligations["review"].excluded[0].reason,
            "self-review"
        );
        // Comments are not approvals; a non-allowed approval does not count.
        let noise = assess(
            &observations(
                vec![green.clone()],
                vec![
                    review(1, 789, "COMMENTED", HEAD, 10),
                    review(2, 1000, "APPROVED", HEAD, 11),
                ],
            ),
            None,
        );
        let reasons: Vec<&str> = noise.obligations["review"]
            .excluded
            .iter()
            .map(|e| e.reason.as_str())
            .collect();
        assert_eq!(reasons, ["not-decisive", "reviewer-not-allowed"]);
        // The good case.
        let good = assess(
            &observations(vec![green], vec![review(1, 42, "APPROVED", HEAD, 10)]),
            None,
        );
        assert_eq!(good.result, Aggregate::Established);
        assert_eq!(good.result.exit_code(), 0);
    }

    #[test]
    fn identical_duplicate_review_is_one_record() {
        let green = run(1, 65, 1, "completed", Some("success"));
        let twice = vec![
            review(1, 42, "APPROVED", HEAD, 10),
            review(1, 42, "APPROVED", HEAD, 10),
        ];
        let assessment = assess(&observations(vec![green.clone()], twice), None);
        assert_eq!(assessment.obligations["review"].reason, "approval-present");
        assert_eq!(assessment.result, Aggregate::Established);
        let differing = vec![
            review(1, 42, "APPROVED", HEAD, 10),
            review(1, 42, "DISMISSED", HEAD, 10),
        ];
        let assessment = assess(&observations(vec![green], differing), None);
        assert_eq!(assessment.obligations["review"].result, Verdict::Conflict);
    }

    #[test]
    fn maximum_allowance_never_overflows() {
        let mut requirements = requirements();
        requirements.max_observation_age_seconds = MAX_OBSERVATION_AGE;
        assert!(requirements.validate().is_ok());
        let request = Request::new(candidate(), &requirements).unwrap();
        let observations = observations(
            vec![run(1, 65, 1, "completed", Some("success"))],
            vec![review(1, 42, "APPROVED", HEAD, 10)],
        );
        let fresh = evaluate(
            &requirements,
            &request,
            "sha256:c",
            &observations,
            Some(u64::MAX / 2),
        )
        .unwrap();
        assert_eq!(
            fresh.result,
            Aggregate::Unproven,
            "far future is stale, not a panic"
        );
        let mut huge = observations.clone();
        huge.interval.end = u64::MAX - 1;
        huge.interval.start = u64::MAX - 2;
        let saturated =
            evaluate(&requirements, &request, "sha256:c", &huge, Some(u64::MAX)).unwrap();
        assert_eq!(
            saturated.result,
            Aggregate::Established,
            "saturating add, no wrap to stale"
        );
    }

    #[test]
    fn gaps_block_without_inventing() {
        let mut moved = observations(vec![run(1, 65, 1, "completed", Some("success"))], vec![]);
        moved.gaps.push(Gap {
            scope: "candidate".into(),
            code: "candidate-moved".into(),
            detail: "head changed".into(),
        });
        let assessment = assess(&moved, None);
        assert!(
            assessment
                .obligations
                .values()
                .all(|o| o.reason == "candidate-moved")
        );
        let mut denied = observations(vec![run(1, 65, 1, "completed", Some("success"))], vec![]);
        denied.gaps.push(Gap {
            scope: "reviews".into(),
            code: "provider-unavailable".into(),
            detail: "HTTP 403".into(),
        });
        let assessment = assess(&denied, None);
        assert_eq!(assessment.obligations["ci"].result, Verdict::Satisfied);
        assert_eq!(
            assessment.obligations["review"].reason,
            "provider-unavailable"
        );
        assert!(describe(&assessment.obligations["review"]).contains("HTTP 403"));
        let mut forbidden = denied.clone();
        forbidden.gaps[0].code = "provider-denied".into();
        let assessment = assess(&forbidden, None);
        assert!(describe(&assessment.obligations["review"]).contains("refused"));
        for code in [
            "unsupported-response",
            "confirmation-missing",
            "candidate-unconfirmed",
        ] {
            let mut gapped = denied.clone();
            gapped.gaps[0].code = code.into();
            let assessment = assess(&gapped, None);
            assert!(
                !describe(&assessment.obligations["review"]).starts_with("Unproven:"),
                "{code}"
            );
        }
    }

    #[test]
    fn later_evaluation_time_makes_evidence_stale() {
        let observations = observations(
            vec![run(1, 65, 1, "completed", Some("success"))],
            vec![review(1, 42, "APPROVED", HEAD, 10)],
        );
        assert_eq!(
            assess(&observations, Some(1_788_908_004 + 300)).result,
            Aggregate::Established
        );
        let stale = assess(&observations, Some(1_788_908_004 + 301));
        assert_eq!(stale.result, Aggregate::Unproven);
        assert!(stale.obligations.values().all(|o| o.reason == "stale"));
        assert_eq!(stale.evaluation_basis, "explicit");
        let requirements = requirements();
        let request = Request::new(candidate(), &requirements).unwrap();
        assert!(evaluate(&requirements, &request, "sha256:c", &observations, Some(1)).is_err());
    }

    #[test]
    fn evidence_order_does_not_change_the_result() {
        let runs = vec![
            run(1, 65, 1, "completed", Some("success")),
            run(2, 66, 1, "completed", Some("success")),
            run(3, 64, 1, "completed", Some("failure")),
        ];
        let reviews = vec![
            review(1, 789, "APPROVED", OLD, 5),
            review(2, 42, "APPROVED", HEAD, 10),
            review(3, 789, "COMMENTED", HEAD, 12),
        ];
        let forward = assess(&observations(runs.clone(), reviews.clone()), None);
        let mut reversed_runs = runs;
        reversed_runs.reverse();
        let mut reversed_reviews = reviews;
        reversed_reviews.reverse();
        let backward = assess(&observations(reversed_runs, reversed_reviews), None);
        let strip = |a: &Assessment| {
            let mut value = serde_json::to_value(a).unwrap();
            for obligation in value["obligations"].as_object_mut().unwrap().values_mut() {
                let excluded = obligation["excluded"].as_array_mut().unwrap();
                excluded.sort_by_key(|e| e["reference"].as_str().unwrap().to_string());
            }
            value
        };
        assert_eq!(strip(&forward), strip(&backward));
        assert_eq!(forward.result, Aggregate::Established);
        assert_eq!(
            mermaid(&forward).lines().count(),
            mermaid(&backward).lines().count()
        );
    }

    #[test]
    fn projections_escape_and_explain() {
        let assessment = assess(
            &observations(vec![run(1, 65, 1, "completed", Some("success"))], vec![]),
            None,
        );
        let graph = mermaid(&assessment);
        assert!(graph.starts_with("flowchart BT\n"));
        assert!(graph.contains("|supports| O0"));
        assert!(graph.contains("review-readiness: UNPROVEN"));
        assert_eq!(mermaid_label("a\"b<c>#d;"), "a#quot;b#lt;c#gt;#35;d#59;");
        assert_eq!(
            mermaid_id("ci-lint"),
            mermaid_id("ci_lint"),
            "why ids are positional"
        );
        let why = explain(&assessment, "review").unwrap();
        assert!(why.contains("approval-missing"));
        assert!(why.contains("Missing: 1 more approval"));
        assert!(explain(&assessment, "nope").is_err());
        let text = render(&assessment, "bundle", "req.json");
        assert!(text.starts_with("0e1982a… — UNPROVEN\n"));
        assert!(text.contains("✓ ci      GitHub reports success"));
        assert!(text.contains("--why review"));
    }
}
