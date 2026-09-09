//! The `inspect` and `assess` commands: acquisition, publication, assessment
//! and rendering of one result as text, JSON or Mermaid.
//!
//! Exit codes: 0 established (or observations saved), 1 refuted, 2 invalid
//! input or tool failure, 3 unproven, 4 conflict. Existing commands keep theirs.
use crate::assessment::{
    self, AUTHENTICITY, Assessment, Candidate, MODE, Observations, Request, Requirements, TRUST,
    short,
};
use crate::canonical::canonical;
use crate::evidence::{Bundle, Store};
use crate::github;
use serde_json::json;
use std::path::Path;
use std::process::ExitCode;

struct Fault {
    code: &'static str,
    message: String,
}

fn fault(code: &'static str) -> impl Fn(String) -> Fault {
    move |message| Fault { code, message }
}

fn fail(json: bool, fault: Fault) -> ExitCode {
    if json {
        println!(
            "{}",
            json!({"schema": "sykli-error.v1", "code": fault.code, "message": fault.message})
        );
    } else {
        eprintln!("error [{}]: {}", fault.code, fault.message);
    }
    ExitCode::from(2)
}

fn load_requirements(path: &Path) -> Result<(Requirements, String), Fault> {
    let bytes = std::fs::read(path).map_err(|e| Fault {
        code: "invalid-requirements",
        message: format!("{}: {e}", path.display()),
    })?;
    let requirements = Requirements::parse(&bytes).map_err(|e| Fault {
        code: "invalid-requirements",
        message: format!("{}: {e}", path.display()),
    })?;
    let id = requirements.id().map_err(fault("invalid-requirements"))?;
    Ok((requirements, id))
}

/// Evaluate a loaded bundle. The same path serves live inspection and offline
/// replay; persistence is separate so a read-only bundle still yields a verdict.
fn assess_bundle(
    bundle: &Bundle,
    requirements: &Requirements,
    candidate: Candidate,
    observations: &Observations,
    at: Option<u64>,
) -> Result<(Request, Assessment), Fault> {
    let request = Request::new(candidate, requirements).map_err(fault("repository-mismatch"))?;
    let assessment = assessment::evaluate(
        requirements,
        &request,
        &format!("sha256:{}", bundle.id),
        observations,
        at,
    )
    .map_err(fault("invalid-time"))?;
    Ok((request, assessment))
}

/// Save requirements, request and assessment beside the collection. Best
/// effort after the verdict is printed: an archived or read-only bundle is
/// still assessable, and the failure is reported on stderr, never as a verdict.
fn persist(
    bundle: &Bundle,
    requirements: &Requirements,
    request: &Request,
    assessment: &Assessment,
) -> Result<(), String> {
    fn save<T: serde::Serialize>(
        bundle: &Bundle,
        kind: &str,
        id: &str,
        value: &T,
    ) -> Result<(), String> {
        bundle.save(kind, id, &canonical(value)?).map(|_| ())
    }
    save(bundle, "requirements", &requirements.id()?, requirements)?;
    save(bundle, "requests", &request.id()?, request)?;
    save(bundle, "assessments", &assessment.id()?, assessment)
}

fn persist_or_warn(
    bundle: &Bundle,
    requirements: &Requirements,
    request: &Request,
    assessment: &Assessment,
) {
    if let Err(error) = persist(bundle, requirements, request, assessment) {
        eprintln!(
            "warning: assessment not saved beside {}: {error}",
            bundle.path.display()
        );
    }
}

fn observations_text(candidate: &Candidate, observations: &Observations, saved: &Path) -> String {
    let mut out = format!(
        "{} #{} at {}\n\nObserved\n",
        candidate.repository.name,
        candidate.number,
        short(&candidate.head.commit)
    );
    let mut runs = observations.runs.clone();
    runs.sort_by_key(|r| (r.workflow_id, std::cmp::Reverse(r.run_number)));
    if runs.is_empty() {
        out.push_str("  Runs: none observed for the head commit\n");
    }
    for run in &runs {
        let outcome = match (run.status.as_str(), run.conclusion.as_deref()) {
            ("completed", Some(conclusion)) => format!("GitHub reports {conclusion}"),
            (status, _) => format!("GitHub reports {status}"),
        };
        out.push_str(&format!(
            "  {} (workflow {}) run {} attempt {} via {}: {outcome}\n",
            run.workflow_name.as_deref().unwrap_or("workflow"),
            run.workflow_id,
            run.run_number,
            run.run_attempt,
            run.event
        ));
    }
    if observations.reviews.is_empty() {
        out.push_str("  Reviews: none observed\n");
    }
    for review in &observations.reviews {
        out.push_str(&format!(
            "  Review {} by account {}: {} on {}\n",
            review.id,
            review
                .user_id
                .map_or("unknown".to_string(), |u| u.to_string()),
            review.state,
            review.commit_id.as_deref().map(short).unwrap_or_default()
        ));
    }
    out.push_str(&format!("  Author account: {}\n", candidate.author_user_id));
    if !observations.workflows.is_empty() {
        out.push_str("  Workflows: ");
        out.push_str(
            &observations
                .workflows
                .iter()
                .map(|w| format!("{} {} ({})", w.name, w.id, w.path))
                .collect::<Vec<_>>()
                .join("; "),
        );
        out.push('\n');
    }
    for (scope, coverage) in &observations.coverage {
        if !coverage.complete {
            out.push_str(&format!(
                "  Coverage {scope}: incomplete ({})\n",
                coverage.note.as_deref().unwrap_or("unknown")
            ));
        }
    }
    if observations.gaps.is_empty() {
        out.push_str("Gaps: none\n");
    }
    for gap in &observations.gaps {
        out.push_str(&format!(
            "Gap: {} ({}): {}\n",
            gap.code, gap.scope, gap.detail
        ));
    }
    out.push_str(&format!(
        "\nRequirements: not configured\nSaved observations: {}\n",
        saved.display()
    ));
    out
}

pub fn inspect(
    repository: &str,
    pull: u64,
    requirements: Option<&Path>,
    store: &Path,
    json: bool,
) -> ExitCode {
    if let Err(error) = github::repository(repository) {
        return fail(
            json,
            Fault {
                code: "invalid-repository",
                message: error,
            },
        );
    }
    let requirements_path = requirements
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let requirements = match requirements.map(load_requirements) {
        Some(Err(fault)) => return fail(json, fault),
        Some(Ok(loaded)) => Some(loaded),
        None => None,
    };
    let store = match Store::open(store) {
        Ok(store) => store,
        Err(error) => {
            return fail(
                json,
                Fault {
                    code: "store-failure",
                    message: error,
                },
            );
        }
    };
    let previous = store.latest(repository, pull);
    let draft = match github::collect(
        repository,
        pull,
        requirements.as_ref().map(|(_, id)| format!("sha256:{id}")),
        requirements.as_ref().map(|(r, _)| r.repository.id),
        previous,
    ) {
        Ok(draft) => draft,
        Err(github::CollectError::Unreadable(message)) => {
            return fail(
                json,
                Fault {
                    code: "provider-unavailable",
                    message,
                },
            );
        }
        Err(github::CollectError::RepositoryMismatch(message)) => {
            return fail(
                json,
                Fault {
                    code: "repository-mismatch",
                    message,
                },
            );
        }
    };
    let bundle = match store.publish(&draft.manifest, &draft.objects, &draft.diagnostics) {
        Ok(bundle) => bundle,
        Err(error) => {
            return fail(
                json,
                Fault {
                    code: "store-failure",
                    message: error,
                },
            );
        }
    };
    let (candidate, observations) = match github::normalize(&bundle) {
        Ok(normalized) => normalized,
        Err(error) => {
            return fail(
                json,
                Fault {
                    code: "invalid-bundle",
                    message: error,
                },
            );
        }
    };
    let shown = store_path(store_root(&bundle), &bundle.id);
    let Some((requirements, _)) = requirements else {
        if json {
            println!(
                "{}",
                json!({
                    "schema": "sykli-inspect.v1",
                    "kind": "observations-only",
                    "bundle": shown.display().to_string(),
                    "collection": format!("sha256:{}", bundle.id),
                    "candidate": candidate,
                    "observations": observations,
                    "trust": TRUST,
                    "authenticity": AUTHENTICITY,
                    "mode": MODE,
                })
            );
        } else {
            print!("{}", observations_text(&candidate, &observations, &shown));
        }
        return ExitCode::SUCCESS;
    };
    let (request, assessment) =
        match assess_bundle(&bundle, &requirements, candidate, &observations, None) {
            Ok(assessed) => assessed,
            Err(fault) => return fail(json, fault),
        };
    if json {
        println!(
            "{}",
            json!({
                "schema": "sykli-inspect.v1",
                "kind": "assessment",
                "bundle": shown.display().to_string(),
                "collection": format!("sha256:{}", bundle.id),
                "assessment": assessment,
            })
        );
    } else {
        print!(
            "{}",
            assessment::render(
                &assessment,
                &shown.display().to_string(),
                &requirements_path
            )
        );
        println!("Saved observations: {}", shown.display());
    }
    persist_or_warn(&bundle, &requirements, &request, &assessment);
    ExitCode::from(assessment.result.exit_code())
}

fn store_root(bundle: &Bundle) -> &Path {
    bundle.path.parent().unwrap_or(&bundle.path)
}

/// Show the bundle the way the operator addressed the store when possible.
fn store_path(root: &Path, id: &str) -> std::path::PathBuf {
    let current = std::env::current_dir().unwrap_or_default();
    match root.strip_prefix(&current) {
        Ok(relative) => relative.join(id),
        Err(_) => root.join(id),
    }
}

pub fn assess(
    bundle: &Path,
    requirements_path: &Path,
    at: Option<&str>,
    graph: Option<&str>,
    why: Option<&str>,
    json: bool,
) -> ExitCode {
    let (requirements, _) = match load_requirements(requirements_path) {
        Ok(loaded) => loaded,
        Err(fault) => return fail(json, fault),
    };
    let at = match at.map(assessment::parse_time) {
        Some(Err(error)) => {
            return fail(
                json,
                Fault {
                    code: "invalid-time",
                    message: error,
                },
            );
        }
        Some(Ok(seconds)) => Some(seconds),
        None => None,
    };
    let loaded = match Bundle::load(bundle) {
        Ok(loaded) => loaded,
        Err(error) => {
            return fail(
                json,
                Fault {
                    code: "invalid-bundle",
                    message: error,
                },
            );
        }
    };
    let (candidate, observations) = match github::normalize(&loaded) {
        Ok(normalized) => normalized,
        Err(error) => {
            return fail(
                json,
                Fault {
                    code: "invalid-bundle",
                    message: error,
                },
            );
        }
    };
    let (request, assessment) =
        match assess_bundle(&loaded, &requirements, candidate, &observations, at) {
            Ok(assessed) => assessed,
            Err(fault) => return fail(json, fault),
        };
    let assessment = &assessment;
    if let Some(id) = why {
        let Some(obligation) = assessment.obligations.get(id) else {
            return fail(
                json,
                Fault {
                    code: "unknown-obligation",
                    message: format!(
                        "unknown obligation {id:?}; declared: {:?}",
                        assessment.obligations.keys().collect::<Vec<_>>()
                    ),
                },
            );
        };
        if json {
            println!(
                "{}",
                json!({
                    "schema": "sykli-why.v1",
                    "obligation": id,
                    "result": obligation,
                    "gaps": assessment.gaps,
                    "assessment": format!("sha256:{}", assessment.id().unwrap_or_default()),
                })
            );
        } else {
            print!(
                "{}",
                assessment::explain(assessment, id).unwrap_or_default()
            );
        }
    } else if graph.is_some() {
        print!("{}", assessment::mermaid(assessment));
    } else if json {
        println!("{}", serde_json::to_string(assessment).unwrap_or_default());
    } else {
        print!(
            "{}",
            assessment::render(
                assessment,
                &bundle.display().to_string(),
                &requirements_path.display().to_string()
            )
        );
        println!("Replaying supplied evidence from {}", bundle.display());
    }
    persist_or_warn(&loaded, &requirements, &request, assessment);
    ExitCode::from(assessment.result.exit_code())
}
