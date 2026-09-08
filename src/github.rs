//! Bounded read-only GitHub acquisition through the operator's `gh`, and
//! normalization of retained raw responses into observations that reference
//! the exact object and field they came from.
//!
//! The reader issues explicit GET requests to fixed github.com endpoints built
//! from validated components. It never executes a shell, runs candidate code,
//! follows user-supplied URLs, mutates the provider or stores credentials.
use crate::assessment::{
    Base, Candidate, CandidateRepository, Coverage, Gap, Head, Interval, Observations, Review, Run,
    Workflow, format_time, parse_time,
};
use crate::evidence::{Bundle, COLLECTION_SCHEMA, Manifest, Response, Window};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const READER: &str = "github-gh.v1";
const HOST: &str = "github.com";
const PER_PAGE: u64 = 100;
const MAX_PAGES: u64 = 10;
const MAX_BODY: usize = 8 << 20;
const MAX_STDERR: usize = 16 << 10;
const TIMEOUT: Duration = Duration::from_secs(60);

fn seconds() -> u64 {
    crate::canonical::now() / 1000
}

/// `owner/name` with the characters GitHub allows; anything else could
/// rewrite the request path.
pub fn repository(text: &str) -> Result<(String, String), String> {
    let invalid = || format!("invalid repository {text:?}; use OWNER/NAME");
    let (owner, name) = text.split_once('/').ok_or_else(invalid)?;
    for part in [owner, name] {
        if part.is_empty()
            || part.len() > 100
            || part.starts_with('.')
            || !part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(invalid());
        }
    }
    Ok((owner.into(), name.into()))
}

struct Http {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

/// Read a pipe up to `limit` bytes; report whether more was available.
fn bounded(mut reader: impl Read, limit: usize) -> (Vec<u8>, bool) {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                if truncated {
                    continue;
                }
                let room = limit - bytes.len();
                if count > room {
                    bytes.extend_from_slice(&buffer[..room]);
                    truncated = true;
                } else {
                    bytes.extend_from_slice(&buffer[..count]);
                }
            }
        }
    }
    (bytes, truncated)
}

/// Remove anything that looks like a credential before diagnostics are kept.
fn sanitize(text: &[u8]) -> String {
    let text = String::from_utf8_lossy(text);
    let mut out = String::new();
    for line in text.lines() {
        if line.to_ascii_lowercase().contains("authorization") {
            out.push_str("[redacted header]\n");
            continue;
        }
        let mut cleaned = String::new();
        for word in line.split(' ') {
            let token = word.len() >= 30
                && (word.starts_with("gh") && word.as_bytes().get(3) == Some(&b'_')
                    || word.starts_with("github_pat_"));
            cleaned.push_str(if token { "[redacted]" } else { word });
            cleaned.push(' ');
        }
        out.push_str(cleaned.trim_end());
        out.push('\n');
    }
    out
}

fn parse_http(output: &[u8]) -> Result<Http, String> {
    let split = output
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4))
        .or_else(|| output.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2)))
        .ok_or("response has no header/body separator")?;
    let head = String::from_utf8_lossy(&output[..split.0]);
    let mut lines = head.lines();
    let status_line = lines.next().ok_or("empty response")?;
    let status: u16 = status_line
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("unrecognized status line {status_line:?}"))?;
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    Ok(Http {
        status,
        headers,
        body: output[split.0 + split.1..].to_vec(),
    })
}

struct Outcome {
    http: Option<Http>,
    error: Option<String>,
    exit: Option<i32>,
    stderr: String,
}

/// One bounded `gh api` GET. Transport failures come back as `error`; HTTP
/// errors come back as a response with its status.
fn gh(endpoint: &str) -> Outcome {
    let spawned = Command::new("gh")
        .args([
            "api",
            "--method",
            "GET",
            "--hostname",
            HOST,
            "--include",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "X-GitHub-Api-Version: 2022-11-28",
            endpoint,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Outcome {
                http: None,
                error: Some(
                    "gh not found on PATH; install GitHub CLI and run `gh auth login`".into(),
                ),
                exit: None,
                stderr: String::new(),
            };
        }
        Err(error) => {
            return Outcome {
                http: None,
                error: Some(format!("cannot start gh: {error}")),
                exit: None,
                stderr: String::new(),
            };
        }
    };
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let out = std::thread::spawn(move || bounded(stdout, MAX_BODY + (64 << 10)));
    let err = std::thread::spawn(move || bounded(stderr, MAX_STDERR));
    let deadline = Instant::now() + TIMEOUT;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                timed_out = true;
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break None,
        }
    };
    let (output, truncated) = out.join().unwrap_or_default();
    let (stderr, _) = err.join().unwrap_or_default();
    let stderr = sanitize(&stderr);
    let exit = status.and_then(|s| s.code());
    if timed_out {
        return Outcome {
            http: None,
            error: Some(format!(
                "gh did not answer within {} seconds",
                TIMEOUT.as_secs()
            )),
            exit,
            stderr,
        };
    }
    if truncated {
        return Outcome {
            http: None,
            error: Some(format!("response exceeded {} bytes", MAX_BODY)),
            exit,
            stderr,
        };
    }
    if output.is_empty() {
        return Outcome {
            http: None,
            error: Some(format!(
                "gh produced no response (exit {})",
                exit.map_or("signal".to_string(), |c| c.to_string())
            )),
            exit,
            stderr,
        };
    }
    match parse_http(&output) {
        Ok(http) if http.body.len() > MAX_BODY => Outcome {
            http: None,
            error: Some(format!("response exceeded {} bytes", MAX_BODY)),
            exit,
            stderr,
        },
        Ok(http) => Outcome {
            http: Some(http),
            error: None,
            exit,
            stderr,
        },
        Err(error) => Outcome {
            http: None,
            error: Some(error),
            exit,
            stderr,
        },
    }
}

/// Everything one live inspection produced, before publication.
pub struct Draft {
    pub manifest: Manifest,
    pub objects: BTreeMap<String, Vec<u8>>,
    pub diagnostics: Value,
}

struct Collector {
    owner: String,
    name: String,
    objects: BTreeMap<String, Vec<u8>>,
    responses: Vec<Response>,
    gaps: Vec<Gap>,
    diagnostics: Vec<Value>,
    selectors: BTreeMap<String, String>,
}

fn scope_of(role: &str) -> &'static str {
    match role.trim_end_matches("-confirm") {
        "repository" | "pull" => "candidate",
        "commit" => "tree",
        "workflows" => "workflows",
        "runs" => "runs",
        "reviews" => "reviews",
        _ => "candidate",
    }
}

impl Collector {
    fn repo(&self) -> String {
        format!("repos/{}/{}", self.owner, self.name)
    }

    /// Record one request. Returns the parsed body for 200 responses.
    fn call(&mut self, role: &str, page: u64, endpoint: &str) -> Option<Value> {
        let requested_at = format_time(seconds());
        let outcome = gh(endpoint);
        let mut entry = json!({
            "role": role,
            "endpoint": endpoint,
            "exit": outcome.exit,
            "stderr": outcome.stderr,
        });
        let Some(http) = outcome.http else {
            let detail = outcome.error.unwrap_or_else(|| "no response".into());
            entry["error"] = json!(detail);
            self.diagnostics.push(entry);
            self.gaps.push(Gap {
                scope: scope_of(role).into(),
                code: "provider-unavailable".into(),
                detail: format!("{role}: {detail}"),
            });
            return None;
        };
        let object = crate::sha256(&http.body);
        let next = http
            .headers
            .get("link")
            .is_some_and(|link| link.contains("rel=\"next\""));
        entry["status"] = json!(http.status);
        entry["request_id"] = json!(http.headers.get("x-github-request-id"));
        entry["rate_limit_remaining"] = json!(http.headers.get("x-ratelimit-remaining"));
        self.diagnostics.push(entry);
        self.responses.push(Response {
            role: role.into(),
            page,
            endpoint: endpoint.into(),
            status: http.status,
            object: object.clone(),
            bytes: http.body.len() as u64,
            requested_at,
            request_id: http.headers.get("x-github-request-id").cloned(),
            next,
        });
        let body = http.body;
        self.objects.entry(object).or_insert_with(|| body.clone());
        if http.status != 200 {
            self.gaps.push(Gap {
                scope: scope_of(role).into(),
                code: "provider-unavailable".into(),
                detail: format!("{role}: HTTP {}", http.status),
            });
            return None;
        }
        match serde_json::from_slice(&body) {
            Ok(value) => Some(value),
            Err(error) => {
                self.gaps.push(Gap {
                    scope: scope_of(role).into(),
                    code: "unsupported-response".into(),
                    detail: format!("{role}: body is not JSON: {error}"),
                });
                None
            }
        }
    }

    /// Paginate a listing up to the page cap. Coverage is derived later from
    /// the retained responses, so a cap simply stops here.
    fn list(&mut self, role: &str, selector: &str) {
        let endpoint = format!("{}/{selector}", self.repo());
        self.selectors.insert(role.into(), selector.into());
        for page in 1..=MAX_PAGES {
            let separator = if endpoint.contains('?') { '&' } else { '?' };
            let paged = format!("{endpoint}{separator}per_page={PER_PAGE}&page={page}");
            if self.call(role, page, &paged).is_none() {
                return;
            }
            let last = self.responses.last().expect("recorded");
            if !last.next {
                return;
            }
        }
    }
}

/// Live read-only acquisition for one pull request. Fails only when the
/// candidate itself cannot be read; every other problem is a recorded gap.
pub fn collect(
    repository: &str,
    pull: u64,
    requirements: Option<String>,
    previous_collection: Option<String>,
) -> Result<Draft, String> {
    let (owner, name) = super::github::repository(repository)?;
    let mut collector = Collector {
        owner,
        name,
        objects: BTreeMap::new(),
        responses: vec![],
        gaps: vec![],
        diagnostics: vec![],
        selectors: BTreeMap::new(),
    };
    let start = seconds();
    let repo = collector.repo();
    let repository_value = collector.call("repository", 1, &repo);
    let pull_value = collector.call("pull", 1, &format!("{repo}/pulls/{pull}"));
    let (Some(repository_value), Some(pull_value)) = (repository_value, pull_value) else {
        let detail = collector
            .gaps
            .iter()
            .map(|g| g.detail.clone())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!("cannot read {repository}#{pull}: {detail}"));
    };
    let observed_name = repository_value["full_name"].as_str().unwrap_or("");
    if !observed_name.eq_ignore_ascii_case(repository) {
        return Err(format!(
            "requested {repository} but GitHub resolved it to {observed_name:?}; use the canonical name"
        ));
    }
    let head_sha = pull_value["head"]["sha"].as_str().unwrap_or("").to_string();
    if head_sha.len() != 40 {
        return Err("pull request has no readable head commit".into());
    }
    collector.call("commit", 1, &format!("{repo}/git/commits/{head_sha}"));
    collector.list("workflows", "actions/workflows");
    collector.list("runs", &format!("actions/runs?head_sha={head_sha}"));
    collector.list("reviews", &format!("pulls/{pull}/reviews"));
    // Second pass: detect observed change during acquisition. Not a snapshot.
    collector.call("pull-confirm", 1, &format!("{repo}/pulls/{pull}"));
    collector.list("runs-confirm", &format!("actions/runs?head_sha={head_sha}"));
    collector.list("reviews-confirm", &format!("pulls/{pull}/reviews"));
    let end = seconds().max(start);
    let manifest = Manifest {
        schema: COLLECTION_SCHEMA.into(),
        reader: READER.into(),
        host: HOST.into(),
        repository: repository.into(),
        pull_request: pull,
        interval: Window {
            start: format_time(start),
            end: format_time(end),
        },
        selectors: collector.selectors,
        responses: collector.responses,
        gaps: collector.gaps,
        requirements,
        previous_collection,
        tool: format!("sykli {}", env!("CARGO_PKG_VERSION")),
    };
    Ok(Draft {
        manifest,
        objects: collector.objects,
        diagnostics: json!({
            "schema": "sykli-collection-diagnostics.v1",
            "entries": collector.diagnostics,
        }),
    })
}

// -------------------------------------------------------- normalization

fn u64_at(value: &Value, pointer: &str) -> Option<u64> {
    value.pointer(pointer).and_then(Value::as_u64)
}

fn str_at(value: &Value, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(String::from)
}

struct Page {
    object: String,
    value: Value,
}

/// All 200 pages of one role, in page order, plus derived coverage.
fn pages(bundle: &Bundle, role: &str, key: Option<&str>) -> Result<(Vec<Page>, Coverage), String> {
    let mut responses: Vec<&Response> = bundle
        .manifest
        .responses
        .iter()
        .filter(|r| r.role == role)
        .collect();
    responses.sort_by_key(|r| r.page);
    let mut pages = vec![];
    let mut coverage = Coverage {
        complete: !responses.is_empty(),
        pages: responses.len() as u64,
        observed: 0,
        total: None,
        note: None,
    };
    if responses.is_empty() {
        coverage.note = Some(format!("no {role} response retained"));
    }
    for (index, response) in responses.iter().enumerate() {
        if response.page != index as u64 + 1 {
            coverage.complete = false;
            coverage.note = Some(format!("{role} page {} missing", index + 1));
            break;
        }
        if response.status != 200 {
            coverage.complete = false;
            coverage.note = Some(format!(
                "{role} page {} returned HTTP {}",
                response.page, response.status
            ));
            break;
        }
        let value: Value = serde_json::from_slice(bundle.object(&response.object)?)
            .map_err(|e| format!("{role} object {} is not JSON: {e}", response.object))?;
        if let Some(key) = key {
            if let Some(total) = u64_at(&value, "/total_count") {
                coverage.total = Some(total);
            }
            coverage.observed += value[key].as_array().map_or(0, Vec::len) as u64;
        } else {
            coverage.observed += value.as_array().map_or(0, Vec::len) as u64;
        }
        pages.push(Page {
            object: response.object.clone(),
            value,
        });
        if response.next && index + 1 == responses.len() {
            coverage.complete = false;
            coverage.note = Some(format!(
                "{role} listing continues beyond the {} retained page(s); GitHub caps filtered listings at 1000 results",
                responses.len()
            ));
        }
    }
    if coverage.complete
        && coverage
            .total
            .is_some_and(|total| total != coverage.observed)
    {
        coverage.complete = false;
        coverage.note = Some(format!(
            "{role} reports {} results but {} were listed",
            coverage.total.unwrap_or(0),
            coverage.observed
        ));
    }
    Ok((pages, coverage))
}

fn runs_of(pages: &[Page], gaps: &mut Vec<Gap>) -> Vec<Run> {
    let mut runs = vec![];
    for page in pages {
        for (index, item) in page.value["workflow_runs"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let source = format!("sha256:{}#/workflow_runs/{index}", page.object);
            let required = (
                u64_at(item, "/id"),
                u64_at(item, "/workflow_id"),
                u64_at(item, "/repository/id"),
                str_at(item, "/head_sha"),
                str_at(item, "/event"),
                u64_at(item, "/run_number"),
                u64_at(item, "/run_attempt"),
                str_at(item, "/status"),
            );
            let (
                Some(id),
                Some(workflow_id),
                Some(repository_id),
                Some(head_commit),
                Some(event),
                Some(run_number),
                Some(run_attempt),
                Some(status),
            ) = required
            else {
                gaps.push(Gap {
                    scope: "runs".into(),
                    code: "unsupported-response".into(),
                    detail: format!("run at {source} lacks a required field"),
                });
                continue;
            };
            runs.push(Run {
                id,
                workflow_id,
                workflow_name: str_at(item, "/name"),
                repository_id,
                head_repository_id: u64_at(item, "/head_repository/id"),
                head_commit,
                event,
                run_number,
                run_attempt,
                status,
                conclusion: str_at(item, "/conclusion"),
                pull_requests: item["pull_requests"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| u64_at(p, "/number"))
                    .collect(),
                source,
            });
        }
    }
    runs
}

fn reviews_of(pages: &[Page], gaps: &mut Vec<Gap>) -> Vec<Review> {
    let mut reviews = vec![];
    for page in pages {
        for (index, item) in page.value.as_array().into_iter().flatten().enumerate() {
            let source = format!("sha256:{}#/{index}", page.object);
            let (Some(id), Some(state)) = (u64_at(item, "/id"), str_at(item, "/state")) else {
                gaps.push(Gap {
                    scope: "reviews".into(),
                    code: "unsupported-response".into(),
                    detail: format!("review at {source} lacks a required field"),
                });
                continue;
            };
            let submitted_at = match str_at(item, "/submitted_at") {
                Some(text) => match parse_time(&text) {
                    Ok(seconds) => Some(seconds),
                    Err(error) => {
                        gaps.push(Gap {
                            scope: "reviews".into(),
                            code: "unsupported-response".into(),
                            detail: format!("review {id}: {error}"),
                        });
                        None
                    }
                },
                None => None,
            };
            reviews.push(Review {
                id,
                user_id: u64_at(item, "/user/id"),
                state,
                commit_id: str_at(item, "/commit_id"),
                submitted_at,
                source,
            });
        }
    }
    reviews
}

fn candidate_of(repository: &Value, pull: &Value, bundle: &Bundle) -> Result<Candidate, String> {
    let field = |name: &str, value: Option<String>| {
        value.ok_or_else(|| format!("pull request response lacks {name}"))
    };
    let number = field("number", u64_at(pull, "/number").map(|n| n.to_string()))?
        .parse()
        .map_err(|_| "invalid pull number".to_string())?;
    if number != bundle.manifest.pull_request {
        return Err("pull request response is for a different number".into());
    }
    let repository_id = u64_at(repository, "/id").ok_or("repository response lacks id")?;
    let base_repository =
        u64_at(pull, "/base/repo/id").ok_or("pull request lacks base repository")?;
    if base_repository != repository_id {
        return Err("pull request base repository differs from the resolved repository".into());
    }
    let head_commit = field("head commit", str_at(pull, "/head/sha"))?;
    Ok(Candidate {
        repository: CandidateRepository {
            host: bundle.manifest.host.clone(),
            id: repository_id,
            name: field("repository name", str_at(repository, "/full_name"))?,
        },
        number,
        head: Head {
            repository_id: u64_at(pull, "/head/repo/id")
                .ok_or("pull request lacks head repository")?,
            commit: head_commit,
            tree: None,
        },
        base: Base {
            repository_id: base_repository,
            commit: field("base commit", str_at(pull, "/base/sha"))?,
        },
        author_user_id: u64_at(pull, "/user/id").ok_or("pull request lacks author")?,
    })
}

/// Rebuild the candidate and observations from retained raw responses. This is
/// the only path into evaluation, live or replayed, so both agree.
pub fn normalize(bundle: &Bundle) -> Result<(Candidate, Observations), String> {
    if bundle.manifest.reader != READER {
        return Err(format!("unsupported reader {}", bundle.manifest.reader));
    }
    let (repository_pages, _) = pages(bundle, "repository", None)?;
    let (pull_pages, _) = pages(bundle, "pull", None)?;
    let (Some(repository), Some(pull)) = (repository_pages.first(), pull_pages.first()) else {
        return Err("collection has no readable repository and pull request".into());
    };
    let mut candidate = candidate_of(&repository.value, &pull.value, bundle)?;
    let mut gaps: Vec<Gap> = bundle.manifest.gaps.clone();
    let (commit_pages, _) = pages(bundle, "commit", None)?;
    if let Some(commit) = commit_pages.first() {
        if str_at(&commit.value, "/sha").as_deref() == Some(candidate.head.commit.as_str()) {
            candidate.head.tree = str_at(&commit.value, "/tree/sha");
        } else {
            gaps.push(Gap {
                scope: "tree".into(),
                code: "wrong-subject".into(),
                detail: "commit response is not the head commit".into(),
            });
        }
    }
    let mut coverage = BTreeMap::new();
    let (workflow_pages, workflows_coverage) = pages(bundle, "workflows", Some("workflows"))?;
    coverage.insert("workflows".to_string(), workflows_coverage);
    let workflows = workflow_pages
        .iter()
        .flat_map(|p| p.value["workflows"].as_array().cloned().unwrap_or_default())
        .filter_map(|w| {
            Some(Workflow {
                id: u64_at(&w, "/id")?,
                name: str_at(&w, "/name")?,
                path: str_at(&w, "/path")?,
            })
        })
        .collect();
    let (run_pages, runs_coverage) = pages(bundle, "runs", Some("workflow_runs"))?;
    coverage.insert("runs".to_string(), runs_coverage);
    let runs = runs_of(&run_pages, &mut gaps);
    let (review_pages, reviews_coverage) = pages(bundle, "reviews", None)?;
    coverage.insert("reviews".to_string(), reviews_coverage);
    let reviews = reviews_of(&review_pages, &mut gaps);

    // Confirmation pass: observed change during acquisition is a gap.
    let (confirm_pull, _) = pages(bundle, "pull-confirm", None)?;
    match confirm_pull.first() {
        None => gaps.push(Gap {
            scope: "candidate".into(),
            code: "candidate-unconfirmed".into(),
            detail: "the pull request could not be re-read after acquisition".into(),
        }),
        Some(after) => {
            let moved = str_at(&after.value, "/head/sha").as_deref()
                != Some(candidate.head.commit.as_str())
                || u64_at(&after.value, "/head/repo/id") != Some(candidate.head.repository_id)
                || str_at(&after.value, "/base/sha").as_deref()
                    != Some(candidate.base.commit.as_str());
            if moved {
                gaps.push(Gap {
                    scope: "candidate".into(),
                    code: "candidate-moved".into(),
                    detail: format!(
                        "head or base changed during acquisition; head now {}",
                        str_at(&after.value, "/head/sha").unwrap_or_default()
                    ),
                });
            }
        }
    }
    let (confirm_runs, confirm_runs_coverage) =
        pages(bundle, "runs-confirm", Some("workflow_runs"))?;
    // A confirmation pass matters only when the first listing was complete;
    // an incomplete first listing is already its own gap.
    if !confirm_runs_coverage.complete && coverage["runs"].complete {
        gaps.push(Gap {
            scope: "runs".into(),
            code: "confirmation-missing".into(),
            detail: confirm_runs_coverage
                .note
                .unwrap_or_else(|| "runs could not be re-read".into()),
        });
    } else {
        let mut ignored = vec![];
        let before: BTreeSet<(u64, u64, String, Option<String>)> = runs
            .iter()
            .map(|r| (r.id, r.run_attempt, r.status.clone(), r.conclusion.clone()))
            .collect();
        let after: BTreeSet<_> = runs_of(&confirm_runs, &mut ignored)
            .iter()
            .map(|r| (r.id, r.run_attempt, r.status.clone(), r.conclusion.clone()))
            .collect();
        if before != after {
            let changed: BTreeSet<u64> = before
                .symmetric_difference(&after)
                .map(|(id, ..)| *id)
                .collect();
            gaps.push(Gap {
                scope: "runs".into(),
                code: "race".into(),
                detail: format!("runs {changed:?} changed during acquisition"),
            });
        }
    }
    let (confirm_reviews, confirm_reviews_coverage) = pages(bundle, "reviews-confirm", None)?;
    if !confirm_reviews_coverage.complete && coverage["reviews"].complete {
        gaps.push(Gap {
            scope: "reviews".into(),
            code: "confirmation-missing".into(),
            detail: confirm_reviews_coverage
                .note
                .unwrap_or_else(|| "reviews could not be re-read".into()),
        });
    } else {
        let mut ignored = vec![];
        let before: BTreeSet<(u64, String, Option<String>)> = reviews
            .iter()
            .map(|r| (r.id, r.state.clone(), r.commit_id.clone()))
            .collect();
        let after: BTreeSet<_> = reviews_of(&confirm_reviews, &mut ignored)
            .iter()
            .map(|r| (r.id, r.state.clone(), r.commit_id.clone()))
            .collect();
        if before != after {
            let changed: BTreeSet<u64> = before
                .symmetric_difference(&after)
                .map(|(id, ..)| *id)
                .collect();
            gaps.push(Gap {
                scope: "reviews".into(),
                code: "race".into(),
                detail: format!("reviews {changed:?} changed during acquisition"),
            });
        }
    }
    let interval = Interval {
        start: parse_time(&bundle.manifest.interval.start)?,
        end: parse_time(&bundle.manifest.interval.end)?,
    };
    Ok((
        candidate,
        Observations {
            interval,
            runs,
            reviews,
            workflows,
            coverage,
            gaps,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_names_are_validated() {
        assert!(repository("false-systems/sykli").is_ok());
        for bad in [
            "sykli",
            "a/b/c",
            "../x/y",
            "a/b?x=1",
            "a/.git",
            "",
            "owner/na me",
        ] {
            assert!(repository(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn http_parsing_handles_crlf_and_lf() {
        let crlf = b"HTTP/2.0 200 OK\r\nLink: <x>; rel=\"next\"\r\nX-Github-Request-Id: A\r\n\r\n{\"a\":1}";
        let http = parse_http(crlf).unwrap();
        assert_eq!(http.status, 200);
        assert_eq!(http.headers["x-github-request-id"], "A");
        assert_eq!(http.body, b"{\"a\":1}");
        let lf = b"HTTP/1.1 404 Not Found\nContent-Type: application/json\n\n{}";
        assert_eq!(parse_http(lf).unwrap().status, 404);
        assert!(parse_http(b"garbage").is_err());
    }

    #[test]
    fn diagnostics_drop_credentials() {
        let text = sanitize(b"error: token ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789ab rejected\nAuthorization: Bearer x\nplain\n");
        assert!(!text.contains("ghp_A"));
        assert!(text.contains("[redacted]"));
        assert!(text.contains("[redacted header]"));
        assert!(text.contains("plain"));
    }
}
