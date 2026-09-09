//! End-to-end `inspect`/`assess` scenarios against a fake `gh` on PATH that
//! serves saved GitHub responses for false-systems/sykli#25. No network.
//!
//! The fake is a POSIX shell script, so these run on Unix hosts only;
//! `tests/assess_portable.rs` covers what every platform must do.
#![cfg(unix)]
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const HEAD: &str = "0e1982a239a0bd02299ab17de29bd52d17842763";
const REPO: &str = "repos/false-systems/sykli";

struct Fake {
    root: PathBuf,
    bin: PathBuf,
    responses: PathBuf,
}

fn fixture(name: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/github/pr25")
            .join(name),
    )
    .expect("fixture")
}

/// Fake-gh lookup key: lowercase so a case variant of the repository resolves
/// on case-sensitive file systems the way GitHub itself resolves it.
fn key(endpoint: &str) -> String {
    endpoint
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

const GH: &str = r#"#!/bin/sh
# Fake gh: answers `gh api ... ENDPOINT` from $SYKLI_FAKE_GH. Records every call.
dir="$SYKLI_FAKE_GH"
printf '%s\n' "$*" >> "$dir/args.log"
for a in "$@"; do ep="$a"; done
key=$(printf '%s' "$ep" | tr -c 'A-Za-z0-9' '_' | tr 'A-Z' 'a-z')
printf '%s\n' "$ep" >> "$dir/calls.log"
n=$(grep -c -F -x -- "$ep" "$dir/calls.log")
body="$dir/$key.body"
[ -f "$dir/$key.$n.body" ] && body="$dir/$key.$n.body"
if [ ! -f "$body" ]; then
  printf 'HTTP/2.0 404 Not Found\r\nX-Github-Request-Id: FAKE\r\n\r\n{"message":"Not Found","status":"404"}'
  echo "gh: Not Found (HTTP 404)" >&2
  exit 1
fi
status="200 OK"
[ -f "$dir/$key.status" ] && status=$(cat "$dir/$key.status")
[ -f "$dir/$key.$n.status" ] && status=$(cat "$dir/$key.$n.status")
printf 'HTTP/2.0 %s\r\n' "$status"
[ -f "$dir/$key.headers" ] && cat "$dir/$key.headers"
printf 'X-Github-Request-Id: FAKE\r\n\r\n'
cat "$body"
case "$status" in 2*) exit 0;; *) echo "gh: error (HTTP $status) token ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789ab" >&2; exit 1;; esac
"#;

impl Fake {
    fn new(name: &str) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("sykli-assess-{name}-{nonce}"));
        let bin = root.join("bin");
        let responses = root.join("responses");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&responses).unwrap();
        fs::write(bin.join("gh"), GH).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(bin.join("gh"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let fake = Self {
            root,
            bin,
            responses,
        };
        fake.respond(REPO, &fixture("repository.json"));
        fake.respond(&format!("{REPO}/pulls/25"), &fixture("pull.json"));
        fake.respond(
            &format!("{REPO}/git/commits/{HEAD}"),
            &fixture("commit.json"),
        );
        fake.respond(
            &format!("{REPO}/actions/workflows?per_page=100&page=1"),
            &fixture("workflows.json"),
        );
        fake.respond(&fake.runs_endpoint(1), &fixture("runs.json"));
        fake.respond(&fake.reviews_endpoint(1), &fixture("reviews.json"));
        fake
    }

    fn runs_endpoint(&self, page: u64) -> String {
        format!("{REPO}/actions/runs?head_sha={HEAD}&per_page=100&page={page}")
    }

    fn reviews_endpoint(&self, page: u64) -> String {
        format!("{REPO}/pulls/25/reviews?per_page=100&page={page}")
    }

    fn respond(&self, endpoint: &str, body: &[u8]) {
        fs::write(self.responses.join(format!("{}.body", key(endpoint))), body).unwrap();
    }

    /// Answer differently on the n-th call to the same endpoint.
    fn respond_on_call(&self, endpoint: &str, call: u32, body: &[u8]) {
        fs::write(
            self.responses
                .join(format!("{}.{call}.body", key(endpoint))),
            body,
        )
        .unwrap();
    }

    fn status(&self, endpoint: &str, status: &str) {
        fs::write(
            self.responses.join(format!("{}.status", key(endpoint))),
            status,
        )
        .unwrap();
    }

    fn headers(&self, endpoint: &str, headers: &str) {
        fs::write(
            self.responses.join(format!("{}.headers", key(endpoint))),
            headers,
        )
        .unwrap();
    }

    fn requirements(&self, allowed: &[u64]) -> PathBuf {
        let path = self.root.join("sykli-review.json");
        fs::write(
            &path,
            json!({
                "schema": "sykli-requirements.v1",
                "purpose": "review-readiness",
                "repository": {"host": "github.com", "id": 1323443147},
                "max_observation_age_seconds": 300,
                "requirements": {
                    "ci": {
                        "kind": "workflow-reported-success",
                        "source": {"provider": "github", "workflow_id": 327406134, "event": "pull_request"},
                        "selection": "latest-run-latest-attempt"
                    },
                    "review": {
                        "kind": "candidate-approval",
                        "source": {"provider": "github"},
                        "allowed_user_ids": allowed,
                        "minimum": 1,
                        "exclude_pr_author": true
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        path
    }

    fn sykli(&self, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new(env!("CARGO_BIN_EXE_sykli"))
            .args(args)
            .env("PATH", path)
            .env("SYKLI_FAKE_GH", &self.responses)
            .current_dir(&self.root)
            .output()
            .expect("binary runs")
    }

    fn store(&self) -> String {
        self.root.join("evidence").display().to_string()
    }

    fn inspect(&self, extra: &[&str]) -> Output {
        let store = self.store();
        let mut args = vec![
            "inspect",
            "--repo",
            "false-systems/sykli",
            "--pr",
            "25",
            "--store",
            &store,
        ];
        args.extend_from_slice(extra);
        self.sykli(&args)
    }

    fn bundle_path(&self) -> PathBuf {
        let store = self.root.join("evidence");
        let mut bundles: Vec<PathBuf> = fs::read_dir(store)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("manifest.json").is_file())
            .collect();
        bundles.sort();
        bundles.pop().expect("a bundle was published")
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn json_out(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "not one JSON document: {e}\n{}\n{}",
            stdout(output),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn approval(id: u64, user: u64, state: &str, commit: &str, at: &str) -> Value {
    json!({
        "id": 3_000_000_000u64 + id,
        "node_id": "PRR_fake",
        "user": {"login": format!("account-{user}"), "id": user},
        "body": "",
        "state": state,
        "html_url": "https://github.com/false-systems/sykli/pull/25#pullrequestreview-1",
        "pull_request_url": "https://api.github.com/repos/false-systems/sykli/pulls/25",
        "author_association": "MEMBER",
        "_links": {},
        "submitted_at": at,
        "commit_id": commit
    })
}

#[test]
fn inspect_saves_observations_and_assess_replays_them_for_a_fresh_worker() {
    let fake = Fake::new("replay");
    let observed = fake.inspect(&[]);
    assert_eq!(
        observed.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&observed.stderr)
    );
    let text = stdout(&observed);
    assert!(
        text.contains("false-systems/sykli #25 at 0e1982a"),
        "{text}"
    );
    assert!(text.contains(
        "CI (workflow 327406134) run 65 attempt 1 via pull_request: GitHub reports success"
    ));
    assert!(text.contains("Reviews: none observed"));
    assert!(text.contains("Requirements: not configured"));
    let bundle = fake.bundle_path();
    assert!(text.contains(bundle.file_name().unwrap().to_str().unwrap()));

    // Transport audit: explicit GETs only, nothing that could mutate the provider.
    let args = fs::read_to_string(fake.responses.join("args.log")).unwrap();
    assert!(args.lines().count() >= 9);
    for line in args.lines() {
        assert!(
            line.contains("api --method GET --hostname github.com --include"),
            "{line}"
        );
        for forbidden in [
            "POST", "PATCH", "PUT", "DELETE", "--input", "--field", " -f ", "-X",
        ] {
            assert!(!line.contains(forbidden), "{line}");
        }
    }
    assert!(
        !fs::read_to_string(bundle.join("manifest.json"))
            .unwrap()
            .contains("ghp_")
    );

    let requirements = fake.requirements(&[42]);
    let bundle_arg = bundle.display().to_string();
    let requirements_arg = requirements.display().to_string();
    let assessed = fake.sykli(&["assess", &bundle_arg, "--requirements", &requirements_arg]);
    assert_eq!(assessed.status.code(), Some(3));
    let text = stdout(&assessed);
    assert!(text.starts_with("0e1982a… — UNPROVEN\n"), "{text}");
    assert!(text.contains("✓ ci      GitHub reports success for the required workflow"));
    assert!(text.contains("? review  No allowed reviewer has approved this candidate"));
    assert!(text.contains("Replaying supplied evidence"));

    let first = json_out(&fake.sykli(&[
        "assess",
        &bundle_arg,
        "--requirements",
        &requirements_arg,
        "--json",
    ]));
    assert_eq!(first["schema"], "sykli-assessment.v1");
    assert_eq!(first["kind"], "assessment");
    assert_eq!(first["result"], "unproven");
    assert_eq!(first["evaluation_basis"], "collection-end");
    assert_eq!(first["trust"], "trusted-local-collector-and-store");
    assert_eq!(first["authenticity"], "not-established");
    assert_eq!(first["mode"], "advisory");
    assert_eq!(first["obligations"]["ci"]["result"], "satisfied");
    assert_eq!(
        first["obligations"]["ci"]["support"][0]["label"],
        "GitHub run 34169416432 (number 65, attempt 1, pull_request)"
    );
    assert!(
        first["obligations"]["ci"]["support"][0]["reference"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(first["obligations"]["review"]["reason"], "approval-missing");
    assert_eq!(
        first["candidate"]["head"]["tree"],
        "a40a1e78ad2e84e05bc49ba7ab9c95e8efaedd39"
    );
    assert!(bundle.join("assessments").read_dir().unwrap().count() >= 1);
    assert!(bundle.join("requests").read_dir().unwrap().count() == 1);

    // A fresh worker with only the bundle and the approved requirements.
    let copy = fake.root.join("handoff");
    fs::create_dir_all(&copy).unwrap();
    let status = Command::new("cp")
        .arg("-R")
        .arg(&bundle)
        .arg(&copy)
        .status()
        .unwrap();
    assert!(status.success());
    let copied = copy.join(bundle.file_name().unwrap()).display().to_string();
    let second = json_out(&fake.sykli(&[
        "assess",
        &copied,
        "--requirements",
        &requirements_arg,
        "--json",
    ]));
    assert_eq!(first, second);

    let graph = fake.sykli(&[
        "assess",
        &bundle_arg,
        "--requirements",
        &requirements_arg,
        "--graph",
        "mermaid",
    ]);
    assert_eq!(graph.status.code(), Some(3));
    let graph = stdout(&graph);
    assert!(graph.starts_with("flowchart BT\n"));
    assert!(graph.contains("|supports| O0"), "{graph}");
    let why = fake.sykli(&[
        "assess",
        &bundle_arg,
        "--requirements",
        &requirements_arg,
        "--why",
        "review",
    ]);
    assert_eq!(why.status.code(), Some(3));
    assert!(stdout(&why).contains("Missing: 1 more approval(s)"));
    let unknown = fake.sykli(&[
        "assess",
        &bundle_arg,
        "--requirements",
        &requirements_arg,
        "--why",
        "tests",
        "--json",
    ]);
    assert_eq!(unknown.status.code(), Some(2));
    assert_eq!(json_out(&unknown)["code"], "unknown-obligation");
}

#[test]
fn approval_by_an_allowed_account_on_the_head_establishes_readiness() {
    let fake = Fake::new("approved");
    let reviews = json!([
        approval(1, 42, "COMMENTED", HEAD, "2026-09-08T10:00:00Z"),
        approval(2, 1000, "APPROVED", HEAD, "2026-09-08T10:01:00Z"),
        approval(3, 42, "APPROVED", HEAD, "2026-09-08T10:02:00Z"),
    ]);
    fake.respond(&fake.reviews_endpoint(1), reviews.to_string().as_bytes());
    let requirements = fake.requirements(&[42]).display().to_string();
    let out = fake.inspect(&["--requirements", &requirements, "--json"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc = json_out(&out);
    assert_eq!(doc["schema"], "sykli-inspect.v1");
    assert_eq!(doc["kind"], "assessment");
    assert_eq!(doc["assessment"]["result"], "established");
    let review = &doc["assessment"]["obligations"]["review"];
    assert_eq!(review["support"].as_array().unwrap().len(), 1);
    let excluded: Vec<&str> = review["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["reason"].as_str().unwrap())
        .collect();
    assert_eq!(excluded, ["not-decisive", "reviewer-not-allowed"]);

    // A later change request from the allowed reviewer refutes; exit 1.
    let reviews = json!([
        approval(3, 42, "APPROVED", HEAD, "2026-09-08T10:02:00Z"),
        {"id": 3_000_000_099u64, "user": {"id": 42, "login": "account-42"}, "state": "CHANGES_REQUESTED", "commit_id": HEAD, "submitted_at": "2026-09-08T10:05:00Z", "body": "no"},
    ]);
    fake.respond(&fake.reviews_endpoint(1), reviews.to_string().as_bytes());
    let out = fake.inspect(&["--requirements", &requirements]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stdout(&out).contains("✗ review  An allowed reviewer's change request is still active")
    );

    // The author's own approval never counts, and a stale approval is excluded.
    let reviews = json!([
        approval(4, 154441282, "APPROVED", HEAD, "2026-09-08T10:02:00Z"),
        approval(
            5,
            42,
            "APPROVED",
            "3d901a9000000000000000000000000000000000",
            "2026-09-08T10:03:00Z"
        ),
    ]);
    fake.respond(&fake.reviews_endpoint(1), reviews.to_string().as_bytes());
    let requirements = fake.requirements(&[42, 154441282]).display().to_string();
    let out = fake.inspect(&["--requirements", &requirements, "--json"]);
    assert_eq!(out.status.code(), Some(3));
    let review = &json_out(&out)["assessment"]["obligations"]["review"];
    assert_eq!(review["reason"], "approval-missing");
    let excluded: Vec<&str> = review["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["reason"].as_str().unwrap())
        .collect();
    assert_eq!(excluded, ["stale-approval", "self-review"]);
}

#[test]
fn candidate_or_runs_changing_during_collection_is_a_gap() {
    let fake = Fake::new("race");
    let mut moved: Value = serde_json::from_slice(&fixture("pull.json")).unwrap();
    moved["head"]["sha"] = json!("1111111111111111111111111111111111111111");
    fake.respond_on_call(&format!("{REPO}/pulls/25"), 2, moved.to_string().as_bytes());
    let requirements = fake.requirements(&[42]).display().to_string();
    let out = fake.inspect(&["--requirements", &requirements, "--json"]);
    assert_eq!(out.status.code(), Some(3));
    let assessment = &json_out(&out)["assessment"];
    assert_eq!(assessment["gaps"][0]["code"], "candidate-moved");
    assert_eq!(assessment["obligations"]["ci"]["reason"], "candidate-moved");
    assert_eq!(
        assessment["obligations"]["review"]["reason"],
        "candidate-moved"
    );

    let fake = Fake::new("rerun");
    let mut rerun: Value = serde_json::from_slice(&fixture("runs.json")).unwrap();
    rerun["workflow_runs"][0]["run_attempt"] = json!(2);
    rerun["workflow_runs"][0]["status"] = json!("in_progress");
    rerun["workflow_runs"][0]["conclusion"] = Value::Null;
    fake.respond_on_call(&fake.runs_endpoint(1), 2, rerun.to_string().as_bytes());
    let requirements = fake.requirements(&[42]).display().to_string();
    let out = fake.inspect(&["--requirements", &requirements, "--json"]);
    assert_eq!(out.status.code(), Some(3));
    let assessment = &json_out(&out)["assessment"];
    assert_eq!(assessment["obligations"]["ci"]["reason"], "race");
    assert_eq!(
        assessment["obligations"]["review"]["reason"],
        "approval-missing"
    );
}

#[test]
fn provider_failures_and_caps_are_explicit_gaps_not_empty_success() {
    let fake = Fake::new("denied");
    fake.status(&fake.reviews_endpoint(1), "403 Forbidden");
    fake.respond(
        &fake.reviews_endpoint(1),
        br#"{"message":"Resource not accessible by integration","status":"403"}"#,
    );
    let requirements = fake.requirements(&[42]).display().to_string();
    let out = fake.inspect(&["--requirements", &requirements, "--json"]);
    assert_eq!(out.status.code(), Some(3));
    let doc = json_out(&out);
    assert_eq!(
        doc["assessment"]["obligations"]["ci"]["result"],
        "satisfied"
    );
    assert_eq!(
        doc["assessment"]["obligations"]["review"]["reason"],
        "provider-denied"
    );
    let bundle = fake.bundle_path();
    let diagnostics = fs::read_to_string(bundle.join("diagnostics.json")).unwrap();
    assert!(diagnostics.contains("403"));
    assert!(
        !diagnostics.contains("ghp_A"),
        "credentials must not be kept"
    );
    assert!(!stdout(&out).contains("ghp_"));

    let fake = Fake::new("capped");
    for page in 1..=10 {
        fake.respond(&fake.runs_endpoint(page), &fixture("runs.json"));
        fake.headers(
            &fake.runs_endpoint(page),
            &format!(
                "Link: <https://api.github.com/x?page={}>; rel=\"next\"\r\n",
                page + 1
            ),
        );
    }
    let requirements = fake.requirements(&[42]).display().to_string();
    let out = fake.inspect(&["--requirements", &requirements, "--json"]);
    assert_eq!(out.status.code(), Some(3));
    let ci = &json_out(&out)["assessment"]["obligations"]["ci"];
    assert_eq!(ci["reason"], "selection-incomplete");
    assert!(
        ci["missing"]
            .as_str()
            .unwrap()
            .contains("stops at 10 pages"),
        "{}",
        ci["missing"]
    );
    let calls = fs::read_to_string(fake.responses.join("calls.log")).unwrap();
    assert!(
        calls.lines().filter(|l| l.contains("actions/runs")).count() <= 20,
        "page cap respected"
    );

    // Repository unreadable: the candidate cannot be described, so no bundle claims anything.
    let fake = Fake::new("gone");
    fake.status(REPO, "404 Not Found");
    let out = fake.inspect(&["--json"]);
    assert_eq!(out.status.code(), Some(2));
    let doc = json_out(&out);
    assert_eq!(doc["schema"], "sykli-error.v1");
    assert_eq!(doc["code"], "provider-unavailable");
    assert!(
        !fake.root.join("evidence").exists()
            || fs::read_dir(fake.root.join("evidence")).unwrap().count() == 0
    );
}

#[test]
fn tampered_or_torn_bundles_and_altered_requirements_are_rejected() {
    let fake = Fake::new("tamper");
    assert_eq!(fake.inspect(&[]).status.code(), Some(0));
    let bundle = fake.bundle_path();
    let requirements = fake.requirements(&[42]).display().to_string();
    let bundle_arg = bundle.display().to_string();

    // Requests cannot carry a completion flag or unknown fields.
    let lowered = fake.root.join("lowered.json");
    let mut doc: Value = serde_json::from_str(&fs::read_to_string(&requirements).unwrap()).unwrap();
    doc["complete"] = json!(true);
    fs::write(&lowered, doc.to_string()).unwrap();
    let out = fake.sykli(&[
        "assess",
        &bundle_arg,
        "--requirements",
        lowered.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_out(&out)["code"], "invalid-requirements");

    // Changed rules are a different request with no implicit approval.
    let baseline = json_out(&fake.sykli(&[
        "assess",
        &bundle_arg,
        "--requirements",
        &requirements,
        "--json",
    ]));
    let relaxed = fake.root.join("relaxed.json");
    let mut doc: Value = serde_json::from_str(&fs::read_to_string(&requirements).unwrap()).unwrap();
    doc["requirements"]["review"]["exclude_pr_author"] = json!(false);
    fs::write(&relaxed, doc.to_string()).unwrap();
    let changed = json_out(&fake.sykli(&[
        "assess",
        &bundle_arg,
        "--requirements",
        relaxed.to_str().unwrap(),
        "--json",
    ]));
    assert_ne!(baseline["requirements"], changed["requirements"]);
    assert_ne!(baseline["request"], changed["request"]);
    assert_eq!(changed["result"], "unproven");

    // Requirements bound to another repository never match this candidate.
    let foreign = fake.root.join("foreign.json");
    let mut doc: Value = serde_json::from_str(&fs::read_to_string(&requirements).unwrap()).unwrap();
    doc["repository"]["id"] = json!(1);
    fs::write(&foreign, doc.to_string()).unwrap();
    let out = fake.sykli(&[
        "assess",
        &bundle_arg,
        "--requirements",
        foreign.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_out(&out)["code"], "repository-mismatch");

    // Stale replay: later evaluation time is unproven; earlier than the collection is an error.
    let out = fake.sykli(&[
        "assess",
        &bundle_arg,
        "--requirements",
        &requirements,
        "--at",
        "2030-01-01T00:00:00Z",
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(3));
    let doc = json_out(&out);
    assert_eq!(doc["evaluation_basis"], "explicit");
    assert_eq!(doc["obligations"]["ci"]["reason"], "stale");
    let out = fake.sykli(&[
        "assess",
        &bundle_arg,
        "--requirements",
        &requirements,
        "--at",
        "2000-01-01T00:00:00Z",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid-time"));

    // Tampered object: rejected, never completion.
    let object = fs::read_dir(bundle.join("objects"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| fs::read(p).unwrap().trim_ascii() == b"[]")
        .expect("reviews object");
    let original = fs::read(&object).unwrap();
    fs::write(&object, b"[{\"id\":1}]").unwrap();
    let out = fake.sykli(&[
        "assess",
        &bundle_arg,
        "--requirements",
        &requirements,
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_out(&out)["code"], "invalid-bundle");
    fs::write(&object, original).unwrap();
    assert_eq!(
        fake.sykli(&["assess", &bundle_arg, "--requirements", &requirements])
            .status
            .code(),
        Some(3)
    );

    // Torn publication: a manifest without its objects is not a bundle.
    let torn = fake.root.join("torn");
    fs::create_dir_all(&torn).unwrap();
    fs::copy(bundle.join("manifest.json"), torn.join("manifest.json")).unwrap();
    let out = fake.sykli(&[
        "assess",
        torn.to_str().unwrap(),
        "--requirements",
        &requirements,
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_out(&out)["code"], "invalid-bundle");
    let renamed = fake.root.join("f".repeat(64));
    let status = Command::new("cp")
        .arg("-R")
        .arg(&bundle)
        .arg(&renamed)
        .status()
        .unwrap();
    assert!(status.success());
    let out = fake.sykli(&[
        "assess",
        renamed.to_str().unwrap(),
        "--requirements",
        &requirements,
        "--json",
    ]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a digest-named directory must match its manifest"
    );
}

#[test]
fn missing_gh_and_bad_inputs_are_tool_errors() {
    let fake = Fake::new("nogh");
    let store = fake.store();
    let out = Command::new(env!("CARGO_BIN_EXE_sykli"))
        .args([
            "inspect",
            "--repo",
            "false-systems/sykli",
            "--pr",
            "25",
            "--store",
            &store,
            "--json",
        ])
        .env("PATH", fake.root.join("nothing").display().to_string())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let doc = json_out(&out);
    assert_eq!(doc["code"], "provider-unavailable");
    assert!(doc["message"].as_str().unwrap().contains("gh not found"));
    let out = fake.sykli(&[
        "inspect",
        "--repo",
        "../evil/repo",
        "--pr",
        "25",
        "--store",
        &store,
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid-repository"));
    let out = fake.sykli(&[
        "assess",
        "nowhere",
        "--requirements",
        "nowhere.json",
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_out(&out)["code"], "invalid-requirements");
}

#[test]
fn read_only_bundle_still_yields_a_verdict() {
    use std::os::unix::fs::PermissionsExt;
    let fake = Fake::new("readonly");
    assert_eq!(fake.inspect(&[]).status.code(), Some(0));
    let bundle = fake.bundle_path();
    let requirements = fake.requirements(&[42]).display().to_string();
    let bundle_arg = bundle.display().to_string();
    fs::set_permissions(&bundle, fs::Permissions::from_mode(0o555)).unwrap();
    let out = fake.sykli(&[
        "assess",
        &bundle_arg,
        "--requirements",
        &requirements,
        "--json",
    ]);
    fs::set_permissions(&bundle, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(json_out(&out)["result"], "unproven");
    assert!(String::from_utf8_lossy(&out.stderr).contains("warning: assessment not saved"));
    assert!(!bundle.join("assessments").exists());
}

#[test]
fn non_json_success_body_is_a_gap_and_the_bundle_stays_replayable() {
    let fake = Fake::new("html");
    fake.respond_on_call(&fake.reviews_endpoint(1), 2, b"<html>maintenance</html>");
    let requirements = fake.requirements(&[42]).display().to_string();
    let out = fake.inspect(&["--requirements", &requirements, "--json"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc = json_out(&out);
    assert_eq!(
        doc["assessment"]["obligations"]["ci"]["result"],
        "satisfied"
    );
    // The first listing was fine; the confirmation pass answered with HTML,
    // so change during acquisition is unknown and the raw gap says which pass.
    assert_eq!(
        doc["assessment"]["obligations"]["review"]["reason"],
        "confirmation-missing"
    );
    let scopes: Vec<&str> = doc["assessment"]["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["scope"].as_str().unwrap())
        .collect();
    assert!(scopes.contains(&"reviews-confirm"), "{scopes:?}");
    let bundle = fake.bundle_path().display().to_string();
    let replay = fake.sykli(&["assess", &bundle, "--requirements", &requirements, "--json"]);
    assert_eq!(replay.status.code(), Some(3));
    assert_eq!(
        json_out(&replay)["obligations"]["review"]["reason"],
        "confirmation-missing"
    );
    assert!(!stdout(&replay).contains("Unproven:"));
}

#[test]
fn orphaned_temporary_directories_and_case_variants_do_not_fork_lineage() {
    let fake = Fake::new("lineage");
    assert_eq!(fake.inspect(&[]).status.code(), Some(0));
    let first = fake.bundle_path();
    // An interrupted publish leaves a fully written temporary directory behind.
    let orphan = fake.root.join("evidence").join(".tmp-1-9999999999999-0");
    let status = Command::new("cp")
        .arg("-R")
        .arg(&first)
        .arg(&orphan)
        .status()
        .unwrap();
    assert!(status.success());
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(orphan.join("manifest.json")).unwrap()).unwrap();
    manifest["interval"]["end"] = json!("2099-01-01T00:00:00Z");
    fs::write(orphan.join("manifest.json"), manifest.to_string()).unwrap();
    let store = fake.store();
    let out = fake.sykli(&[
        "inspect",
        "--repo",
        "False-Systems/sykli",
        "--pr",
        "25",
        "--store",
        &store,
        "--json",
    ]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc = json_out(&out);
    let second = Path::new(doc["bundle"].as_str().unwrap()).to_path_buf();
    let second = if second.is_absolute() {
        second
    } else {
        fake.root.join(second)
    };
    let manifest: Value =
        serde_json::from_slice(&fs::read(second.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["previous_collection"],
        json!(first.file_name().unwrap().to_str().unwrap())
    );
    assert_eq!(
        manifest["repository"], "false-systems/sykli",
        "canonical name, not the typed spelling"
    );
}

#[test]
fn incomplete_first_listing_never_becomes_a_race() {
    let fake = Fake::new("norace");
    for page in 1..=2 {
        fake.respond(&fake.runs_endpoint(page), &fixture("runs.json"));
    }
    fake.headers(
        &fake.runs_endpoint(1),
        "Link: <https://api.github.com/x?page=2>; rel=\"next\"\r\n",
    );
    // First pass: page 2 fails; confirmation pass: page 2 succeeds.
    fs::write(
        fake.responses
            .join(format!("{}.1.status", key(&fake.runs_endpoint(2)))),
        "502 Bad Gateway",
    )
    .unwrap();
    let requirements = fake.requirements(&[42]).display().to_string();
    let out = fake.inspect(&["--requirements", &requirements, "--json"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let assessment = &json_out(&out)["assessment"];
    let codes: Vec<&str> = assessment["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["code"].as_str().unwrap())
        .collect();
    assert!(!codes.contains(&"race"), "{codes:?}");
    assert!(!codes.contains(&"confirmation-missing"), "{codes:?}");
    assert_eq!(
        assessment["obligations"]["ci"]["reason"],
        "provider-unavailable"
    );
    let note = assessment["coverage"]["runs"]["note"].as_str().unwrap();
    assert!(note.contains("HTTP 502"), "{note}");
    assert!(!note.contains("1000"), "{note}");

    // A denied endpoint says so, and a page cap names the cap rather than a transport fault.
    let fake = Fake::new("denied-code");
    fake.status(&fake.reviews_endpoint(1), "403 Forbidden");
    let requirements = fake.requirements(&[42]).display().to_string();
    let out = fake.inspect(&["--requirements", &requirements]);
    assert_eq!(out.status.code(), Some(3));
    assert!(
        stdout(&out).contains("GitHub refused the query"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn requirements_for_another_repository_stop_before_anything_is_saved() {
    let fake = Fake::new("foreign");
    let requirements = fake.requirements(&[42]);
    let mut doc: Value = serde_json::from_str(&fs::read_to_string(&requirements).unwrap()).unwrap();
    doc["repository"]["id"] = json!(1);
    fs::write(&requirements, doc.to_string()).unwrap();
    let out = fake.inspect(&["--requirements", requirements.to_str().unwrap(), "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_out(&out)["code"], "repository-mismatch");
    let calls = fs::read_to_string(fake.responses.join("calls.log")).unwrap();
    assert_eq!(
        calls.lines().count(),
        1,
        "only the repository was read: {calls}"
    );
    assert!(
        !fake.root.join("evidence").exists()
            || fs::read_dir(fake.root.join("evidence")).unwrap().count() == 0
    );
}
