//! The CI shadow experiment. It compares Sykli's changed-path selection and the
//! cache evidence of the pull request's base with the full run the gate just
//! made, and writes what it saw. It is advisory: it never decides the required
//! gate's result. See docs/ci-shadow.md.
//!
//! ```text
//! cargo run --example ci_shadow -- --base <commit> --binary <sykli> \
//!     --receipt <full run receipt> --output <new dir> [--pr-head <commit>]
//! ```
//!
//! Exits 0 when the observation was made and 2 when it was not; either way
//! `report.json` (`sykli-ci-shadow.v1`) says why. It is an example so that
//! `cargo install` never ships it; it is Unix CI glue, and on other platforms
//! it reports that it does not run.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const SCHEMA: &str = "sykli-ci-shadow.v1";
const USAGE: &str = "usage: ci_shadow --base <commit> --binary <sykli> --receipt <file> \
                     --output <new dir> [--pr-head <commit>]";

/// Repository policy, not a blanket Markdown exemption: edits to exactly these
/// existing regular files need no declared input.
const DOCUMENTATION_PATHS: [&str; 3] = ["README.md", "docs/agents.md", "docs/ci-shadow.md"];

type Result<T, E = String> = std::result::Result<T, E>;

pub struct Args {
    pub base: String,
    pub binary: PathBuf,
    pub receipt: PathBuf,
    pub output: PathBuf,
    pub pr_head: Option<String>,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args> {
    let (mut base, mut binary, mut receipt, mut output, mut pr_head) =
        (None, None, None, None, None);
    while let Some(argument) = args.next() {
        // `--flag value` or `--flag=value`.
        let (flag, value) = match argument.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag.to_string(), value.to_string()),
            _ => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("{argument} needs a value"))?;
                (argument, value)
            }
        };
        match flag.as_str() {
            "--base" => base = Some(value),
            "--binary" => binary = Some(PathBuf::from(value)),
            "--receipt" => receipt = Some(PathBuf::from(value)),
            "--output" => output = Some(PathBuf::from(value)),
            "--pr-head" => pr_head = Some(value),
            _ => return Err(format!("unknown argument {flag}")),
        }
    }
    Ok(Args {
        base: base.ok_or("--base is required")?,
        binary: binary.ok_or("--binary is required")?,
        receipt: receipt.ok_or("--receipt is required")?,
        output: output.ok_or("--output is required")?,
        pr_head,
    })
}

fn main() -> ExitCode {
    if std::env::args()
        .skip(1)
        .any(|arg| arg == "-h" || arg == "--help")
    {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if !cfg!(unix) {
        eprintln!("ci_shadow: Unix CI glue; it does not run on this platform");
        return ExitCode::from(2);
    }
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("ci_shadow: {error}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let observed = std::env::current_dir()
        .map_err(|e| format!("current directory: {e}"))
        .and_then(|cwd| {
            let summary = std::env::var_os("GITHUB_STEP_SUMMARY").filter(|path| !path.is_empty());
            observe(&args, &cwd, summary.map(PathBuf::from).as_deref())
        });
    match observed {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(2),
        Err(error) => {
            eprintln!("ci_shadow: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Makes the observation from the repository at `cwd`, or records why it
/// could not, and reports either way; the summary is also appended to
/// `step_summary` when given. Returns whether it was made; an error is only
/// one that leaves no report to read.
pub fn observe(args: &Args, cwd: &Path, step_summary: Option<&Path>) -> Result<bool> {
    let output = std::path::absolute(&args.output).map_err(|e| format!("--output: {e}"))?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    fs::create_dir(&output).map_err(|e| format!("{}: {e}", output.display()))?;
    let started = Instant::now();
    let mut report = experiment(args, cwd, &output).unwrap_or_else(|error| {
        json!({"schema": SCHEMA, "status": "unavailable", "advisory": true, "error": error})
    });
    report["experiment_ms"] = json!(millis(started));
    write_json(&output.join("report.json"), &report)?;
    let rendered = summary(&report);
    fs::write(output.join("summary.md"), &rendered).map_err(|e| format!("summary.md: {e}"))?;
    if let Some(path) = step_summary {
        fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .and_then(|mut file| file.write_all(rendered.as_bytes()))
            .map_err(|e| format!("GITHUB_STEP_SUMMARY: {e}"))?;
    }
    println!(
        "{}",
        json!({"status": report["status"], "report": output.join("report.json")})
    );
    Ok(report["status"] == "observed")
}

fn experiment(args: &Args, cwd: &Path, output: &Path) -> Result<Value> {
    let repo = PathBuf::from(text(git(cwd, &["rev-parse", "--show-toplevel"])?)?.trim());
    let base = text(git(
        &repo,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{}^{{commit}}", args.base),
        ],
    )?)?
    .trim()
    .to_string();
    let candidate = text(git(&repo, &["rev-parse", "HEAD"])?)?
        .trim()
        .to_string();
    let binary = fs::canonicalize(&args.binary)
        .map_err(|e| format!("--binary {}: {e}", args.binary.display()))?;
    let receipt_path = fs::canonicalize(&args.receipt)
        .map_err(|e| format!("--receipt {}: {e}", args.receipt.display()))?;
    let full_bytes =
        fs::read(&receipt_path).map_err(|e| format!("{}: {e}", receipt_path.display()))?;
    let receipt: Value =
        serde_json::from_slice(&full_bytes).map_err(|e| format!("full receipt: {e}"))?;
    fs::write(output.join("full-receipt.json"), &full_bytes)
        .map_err(|e| format!("full-receipt.json: {e}"))?;
    let changed = name_status(&git(
        &repo,
        &[
            "diff",
            "--name-status",
            "-z",
            "--no-renames",
            &base,
            &candidate,
            "--",
        ],
    )?)?;
    let binary_bytes = fs::read(&binary).map_err(|e| format!("{}: {e}", binary.display()))?;
    let mut report = json!({
        "schema": SCHEMA,
        "status": "observed",
        "advisory": true,
        "base_commit": base,
        "candidate_commit": candidate,
        "pr_head": args.pr_head,
        "binary_sha256": sha256(&binary_bytes),
        "full_receipt_sha256": sha256(&full_bytes),
        "changes": changed.iter().map(Change::to_json).collect::<Vec<_>>(),
    });
    write_json(&output.join("changes.json"), &report)?;

    let temporary = TempDir::new("sykli-shadow-")?;
    let mut worktrees = Vec::new();
    let inspected = inspect(
        &Inspection {
            repo: &repo,
            base: &base,
            candidate: &candidate,
            binary: &binary,
            receipt_path: &receipt_path,
            receipt: &receipt,
            changed: &changed,
            output,
            base_dir: temporary.0.join("base"),
            candidate_dir: temporary.0.join("candidate"),
        },
        &mut report,
        &mut worktrees,
    );
    let mut removed = Ok(());
    for path in worktrees.iter().rev() {
        let outcome = git(&repo, &["worktree", "remove", "--force", &display(path)]);
        if removed.is_ok() {
            removed = outcome.map(drop);
        }
    }
    inspected?;
    removed?;
    Ok(report)
}

struct Inspection<'a> {
    repo: &'a Path,
    base: &'a str,
    candidate: &'a str,
    binary: &'a Path,
    receipt_path: &'a Path,
    receipt: &'a Value,
    changed: &'a [Change],
    output: &'a Path,
    base_dir: PathBuf,
    candidate_dir: PathBuf,
}

/// Everything that runs in the two disposable worktrees; `worktrees` lists
/// each one made, so the caller removes them however this ends.
fn inspect(at: &Inspection, report: &mut Value, worktrees: &mut Vec<PathBuf>) -> Result<()> {
    for (path, commit) in [(&at.base_dir, at.base), (&at.candidate_dir, at.candidate)] {
        git(
            at.repo,
            &["worktree", "add", "--detach", &display(path), commit],
        )?;
        worktrees.push(path.clone());
    }
    let sykli = |cwd: &Path, args: &[&str], name: &str, seconds: u64| {
        capture(at.binary, cwd, args, at.output, name, seconds)
    };
    // Verify against the exact committed candidate that was checked out.
    let receipt_path = display(at.receipt_path);
    let (code, _) = sykli(
        &at.candidate_dir,
        &["verify", &receipt_path, "--contract", "sykli.json"],
        "full-verification",
        60,
    )?;
    if !matches!(code, 0 | 1) {
        return Err("full receipt does not verify against the candidate".into());
    }
    let (code, elapsed) = sykli(
        &at.base_dir,
        &["run", "sykli.json", "--json"],
        "base-receipt",
        600,
    )?;
    if !matches!(code, 0 | 1) {
        return Err("baseline graph could not be evaluated".into());
    }
    report["baseline_run_ms"] = json!(elapsed);
    report["baseline_outcome"] = field(&read_json(at.output, "base-receipt")?, "outcome")?.clone();
    // Only baseline evidence reaches the inspection checkout. The
    // authoritative full run's cache is never copied or reused.
    for name in ["cache", "receipts"] {
        let source = at.base_dir.join(".sykli").join(name);
        if source.exists() {
            copy_tree(&source, &at.candidate_dir.join(".sykli").join(name))?;
        }
    }
    let started = Instant::now();
    let (code, _) = sykli(
        &at.candidate_dir,
        &["plan", "--explain", "--json"],
        "candidate-plan",
        60,
    )?;
    if !matches!(code, 0 | 2) {
        return Err("candidate explanation failed".into());
    }
    let plan = read_json(at.output, "candidate-plan")?;
    if field(&plan, "contract_hash")? != field(at.receipt, "contract_hash")? {
        return Err("candidate plan and full receipt describe different contracts".into());
    }
    let affected: BTreeSet<String> = if at.changed.is_empty() {
        write_json(
            &at.output.join("changed-plan.json"),
            &json!({"tasks": [], "reason": "no_changed_paths"}),
        )?;
        BTreeSet::new()
    } else {
        let mut args = vec!["plan", "--explain", "--json"];
        for change in at.changed {
            args.extend(["--changed", change.path.as_str()]);
        }
        let (code, _) = sykli(&at.candidate_dir, &args, "changed-plan", 60)?;
        if !matches!(code, 0 | 2) {
            return Err("changed-path explanation failed".into());
        }
        strings(field(&read_json(at.output, "changed-plan")?, "tasks")?)?
    };
    report["inspection_ms"] = json!(millis(started));
    let contract = fs::read(at.candidate_dir.join("sykli.json"))
        .map_err(|e| format!("candidate sykli.json: {e}"))
        .and_then(|bytes| {
            serde_json::from_slice(&bytes).map_err(|e| format!("candidate sykli.json: {e}"))
        })?;
    let inputs = declared_inputs(&contract)?;
    report["unmapped_paths"] = json!(
        at.changed
            .iter()
            .filter(|change| !inputs.contains(&change.path))
            .map(|change| change.path.as_str())
            .collect::<Vec<_>>()
    );
    report["coverage"] = change_coverage(at.changed, &inputs, &at.candidate_dir);
    let receipts = at.candidate_dir.join(".sykli/receipts");
    let rows = compare(&plan, at.receipt, &affected, &receipts)?;
    // Keep provenance for every proposed reuse, without exporting executable
    // cache artifacts or restoring any candidate outputs.
    for row in &rows {
        if row["cache"]["status"] == "available" {
            let path = receipts.join(str_field(&row["cache"], "receipt")?);
            let file_name = path.file_name().ok_or("a receipt names no file")?;
            let destination = at.output.join("baseline-receipts");
            fs::create_dir_all(&destination).map_err(|e| format!("baseline-receipts: {e}"))?;
            fs::copy(&path, destination.join(file_name))
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    let with = |comparison: &str| {
        rows.iter()
            .filter(move |row| row["comparison"] == comparison)
            .collect::<Vec<_>>()
    };
    let duration = |row: &&Value| row["full_duration_ms"].as_u64().unwrap_or(0);
    report["disagreements"] = json!(
        with("disagrees")
            .iter()
            .map(|row| row["task"].clone())
            .collect::<Vec<_>>()
    );
    report["independent_comparisons"] = json!(with("agrees").len() + with("disagrees").len());
    let reused: u64 = with("agrees").iter().map(duration).sum();
    report["potential_reused_task_ms"] = json!(reused);
    report["coverage_qualified_reused_task_ms"] =
        json!(if report["coverage"]["status"] == "accounted" {
            reused
        } else {
            0
        });
    report["full_task_ms"] = json!(rows.iter().map(|row| duration(&row)).sum::<u64>());
    report["tasks"] = Value::Array(rows);
    Ok(())
}

/// One entry of `git diff --name-status --no-renames`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Change {
    status: String,
    path: String,
}

impl Change {
    fn to_json(&self) -> Value {
        json!({"status": self.status, "path": self.path})
    }
}

fn name_status(bytes: &[u8]) -> Result<Vec<Change>> {
    let mut parts: Vec<&[u8]> = bytes.split(|byte| *byte == 0).collect();
    parts.pop();
    parts
        .chunks(2)
        .map(|pair| match pair {
            [status, path] if status.is_ascii() => Ok(Change {
                status: String::from_utf8_lossy(status).into_owned(),
                path: String::from_utf8(path.to_vec())
                    .map_err(|_| "a changed path is not UTF-8".to_string())?,
            }),
            _ => Err("malformed git diff --name-status output".to_string()),
        })
        .collect()
}

/// Every declared input of every task, as a path from the repository root.
fn declared_inputs(contract: &Value) -> Result<BTreeSet<String>> {
    let mut inputs = BTreeSet::new();
    for task in array(field(contract, "tasks")?)? {
        let workdir = match task.get("workdir") {
            None | Some(Value::Null) => ".",
            Some(Value::String(dir)) if dir.is_empty() => ".",
            Some(Value::String(dir)) => dir,
            Some(_) => return Err("a task's workdir is not a string".into()),
        };
        let paths = match task.get("inputs") {
            None => &[][..],
            Some(paths) => array(paths)?.as_slice(),
        };
        for path in paths {
            let path = path.as_str().ok_or("a declared input is not a string")?;
            inputs.insert(normpath(&join(workdir, path)));
        }
    }
    Ok(inputs)
}

/// Join two POSIX paths; an absolute second path replaces the first.
fn join(dir: &str, path: &str) -> String {
    if path.starts_with('/') || dir.is_empty() {
        path.to_string()
    } else if dir.ends_with('/') {
        format!("{dir}{path}")
    } else {
        format!("{dir}/{path}")
    }
}

/// Normalize a POSIX path lexically, so `..` is kept where nothing precedes
/// it and exactly two leading slashes survive.
fn normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".into();
    }
    let leading = if path.starts_with("//") && !path.starts_with("///") {
        2
    } else if path.starts_with('/') {
        1
    } else {
        0
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if (leading == 0 && parts.is_empty()) || parts.last() == Some(&"..") => {
                parts.push(part)
            }
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    let joined = format!("{}{}", "/".repeat(leading), parts.join("/"));
    if joined.is_empty() {
        ".".into()
    } else {
        joined
    }
}

/// Classify changed paths no declared input covers. `sykli.json` and
/// `sykli.lock` are evaluation metadata, already bound by receipt
/// verification and the contract hash. Modifications of the regular files in
/// DOCUMENTATION_PATHS are exempt; additions, deletions, renames and symlinks
/// are not. Everything else is unresolved.
fn change_coverage(changes: &[Change], inputs: &BTreeSet<String>, root: &Path) -> Value {
    let mut exempted = Vec::new();
    let mut unresolved = Vec::new();
    for change in changes {
        let path = change.path.as_str();
        if inputs.contains(path) {
            continue;
        }
        let on_disk = root.join(path);
        let regular = fs::metadata(&on_disk).is_ok_and(|meta| meta.is_file())
            && !fs::symlink_metadata(&on_disk).is_ok_and(|meta| meta.file_type().is_symlink());
        if matches!(path, "sykli.json" | "sykli.lock") {
            exempted.push(json!({"path": path, "reason": "evaluation-metadata"}));
        } else if DOCUMENTATION_PATHS.contains(&path) && change.status == "M" && regular {
            let reason = if path == "README.md" {
                "repository-readme-only"
            } else {
                "repository-documentation"
            };
            exempted.push(json!({"path": path, "reason": reason}));
        } else {
            unresolved.push(path.to_string());
        }
    }
    json!({
        "status": if unresolved.is_empty() { "accounted" } else { "unresolved" },
        "exempted_paths": exempted,
        "unresolved_paths": unresolved,
    })
}

/// One row per task the candidate plan explains: what its cache evidence
/// proposed, and how that compares with what the full run did.
fn compare(
    plan: &Value,
    receipt: &Value,
    affected: &BTreeSet<String>,
    baseline_receipts: &Path,
) -> Result<Vec<Value>> {
    let mut records = BTreeMap::new();
    for record in array(field(receipt, "tasks")?)? {
        records.insert(str_field(record, "name")?, record);
    }
    let mut rows = Vec::new();
    for item in array(field(plan, "explanations")?)? {
        let name = str_field(item, "task")?;
        let cache = field(item, "cache")?;
        let record = *records
            .get(name)
            .ok_or_else(|| format!("the full run has no task {name:?}"))?;
        let mut comparison = "not_proposed";
        if field(cache, "status")? == "available" {
            let path = baseline_receipts.join(str_field(cache, "receipt")?);
            let baseline: Value = fs::read(&path)
                .map_err(|e| format!("{}: {e}", path.display()))
                .and_then(|bytes| {
                    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
                })?;
            let previous = array(field(&baseline, "tasks")?)?
                .iter()
                .find(|task| task["name"] == name)
                .ok_or_else(|| format!("the baseline receipt has no task {name:?}"))?;
            let empty = json!({});
            let inherited = |task: &Value| task.get("inherited_digests").unwrap_or(&empty).clone();
            // The planner checks this receipt; compare its runtime with the
            // independent full execution too, not merely with this inspection.
            comparison = if field(record, "runtime_fingerprint")?
                != field(previous, "runtime_fingerprint")?
                || inherited(record) != inherited(previous)
            {
                "context_mismatch"
            } else if field(record, "source")? != "task" || field(record, "outcome")? == "blocked" {
                "not_independently_executed"
            } else if field(record, "outcome")? == "passed" && truthy(field(record, "importable")?)
            {
                if field(record, "output_digests")? == field(previous, "output_digests")? {
                    "agrees"
                } else {
                    "disagrees"
                }
            } else if matches!(
                field(record, "outcome")?.as_str(),
                Some("failed" | "errored")
            ) {
                "disagrees"
            } else {
                "unproven"
            };
        }
        rows.push(json!({
            "task": name,
            "affected": affected.contains(name),
            "cache": cache,
            "full_outcome": field(record, "outcome")?,
            "full_source": field(record, "source")?,
            "full_duration_ms": field(record, "duration_ms")?,
            "comparison": comparison,
        }));
    }
    Ok(rows)
}

fn summary(report: &Value) -> String {
    let mut lines: Vec<String> = [
        "### Sykli CI shadow experiment",
        "",
        "Advisory; the full gate remains authoritative.",
        "",
    ]
    .map(String::from)
    .to_vec();
    if report["status"] != "observed" {
        lines.push(format!(
            "Observation unavailable: {}",
            escape(&string(&report["error"]))
        ));
        lines.push(String::new());
        return lines.join("\n");
    }
    lines.push(format!(
        "Compared base <code>{}</code> with tested candidate <code>{}</code>.",
        string(&report["base_commit"]),
        string(&report["candidate_commit"])
    ));
    lines.push(String::new());
    let coverage = &report["coverage"];
    if coverage["status"] == "unresolved" {
        lines.push("**Full execution required: changed paths lack input coverage.**".into());
        lines.push("Declare their dependencies or review an explicit repository exception.".into());
        lines.push(String::new());
        for path in coverage["unresolved_paths"]
            .as_array()
            .into_iter()
            .flatten()
        {
            lines.push(format!(
                "- <code>{}</code>",
                escape(&string(path)).replace('\n', "&#10;")
            ));
        }
    } else {
        lines.push("Changed paths are accounted for; this is not proof that reuse is safe.".into());
    }
    for exemption in coverage["exempted_paths"].as_array().into_iter().flatten() {
        lines.push(format!(
            "Accounted path: <code>{}</code> ({}).",
            escape(&string(&exemption["path"])),
            string(&exemption["reason"])
        ));
    }
    if report["independent_comparisons"] == 0 {
        lines.push(String::new());
        lines.push(
            "**No independent comparison: zero disagreements is not evidence of safe reuse.**"
                .into(),
        );
    }
    lines.extend(
        [
            "",
            "Baseline cache was built separately. Candidate receipt sources below distinguish \
             execution from local reuse.",
            "",
            "| task | affected | baseline cache evidence | candidate result | candidate source | comparison |",
            "|---|---|---|---|---|---|",
        ]
        .map(String::from),
    );
    for row in report["tasks"].as_array().into_iter().flatten() {
        let name = escape(&string(&row["task"]))
            .replace('|', "&#124;")
            .replace('\n', "&#10;");
        lines.push(format!(
            "| <code>{name}</code> | {} | {} | {} | {} | {} |",
            row["affected"],
            string(&row["cache"]["status"]),
            string(&row["full_outcome"]),
            string(&row["full_source"]),
            string(&row["comparison"])
        ));
    }
    let count = |key: &str| report[key].as_array().map_or(0, Vec::len);
    lines.extend([
        String::new(),
        format!(
            "Matching results before coverage review: **{} ms of task duration**, from {} ms \
             observed. Task durations overlap; this is not job wall time saved.",
            report["potential_reused_task_ms"], report["full_task_ms"]
        ),
        format!(
            "After changed-path coverage: **{} ms**. This measurement does not authorize skipping \
             checks.",
            report["coverage_qualified_reused_task_ms"]
        ),
        format!(
            "Baseline cost: {} ms; inspection: {} ms.",
            report["baseline_run_ms"], report["inspection_ms"]
        ),
        format!("Reuse disagreements: **{}**.", count("disagreements")),
        format!(
            "Independent comparisons: **{}**.",
            report["independent_comparisons"]
        ),
        format!(
            "Changed paths without an exact declared input: **{}** (see changes.json and \
             report.json).",
            count("unmapped_paths")
        ),
        String::new(),
        "A matching observation is not proof that the input declarations are complete.".into(),
        String::new(),
    ]);
    lines.join("\n")
}

/// HTML-escape, quotes included: the summary is Markdown with inline HTML.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Whether a receipt field counts as set: false, null, zero and empty do not.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    value.get(key).ok_or_else(|| format!("missing {key:?}"))
}

fn str_field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    field(value, key)?
        .as_str()
        .ok_or_else(|| format!("{key:?} is not a string"))
}

fn array(value: &Value) -> Result<&Vec<Value>> {
    value.as_array().ok_or_else(|| "expected a list".into())
}

fn strings(value: &Value) -> Result<BTreeSet<String>> {
    array(value)?
        .iter()
        .map(|item| {
            item.as_str()
                .map(String::from)
                .ok_or_else(|| "expected a list of names".into())
        })
        .collect()
}

/// A string as the summary quotes it.
fn string(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), String::from)
}

fn text(bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes).map_err(|_| "git printed something that is not UTF-8".into())
}

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn millis(started: Instant) -> u64 {
    (started.elapsed().as_secs_f64() * 1000.0).round() as u64
}

/// Pretty JSON with sorted keys and a trailing newline.
fn write_json(path: &Path, value: &Value) -> Result<()> {
    let mut text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    text.push('\n');
    fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn read_json(output: &Path, name: &str) -> Result<Value> {
    let path = output.join(format!("{name}.json"));
    let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// Copy a directory tree into a destination that must not exist yet,
/// following symbolic links and keeping access and modification times.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    if to.exists() {
        return Err(format!("{} already exists", to.display()));
    }
    fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for entry in fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let entry = entry.map_err(|e| format!("{}: {e}", from.display()))?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        if fs::metadata(&source).is_ok_and(|meta| meta.is_dir()) {
            copy_tree(&source, &target)?;
        } else {
            fs::copy(&source, &target).map_err(|e| format!("{}: {e}", source.display()))?;
            keep_times(&source, &target)?;
        }
    }
    keep_times(from, to)
}

fn keep_times(from: &Path, to: &Path) -> Result<()> {
    let meta = fs::metadata(from).map_err(|e| format!("{}: {e}", from.display()))?;
    let mut times = fs::FileTimes::new();
    if let Ok(modified) = meta.modified() {
        times = times.set_modified(modified);
    }
    if let Ok(accessed) = meta.accessed() {
        times = times.set_accessed(accessed);
    }
    File::open(to)
        .and_then(|file| file.set_times(times))
        .map_err(|e| format!("{}: {e}", to.display()))
}

/// `git -C <repo> <args>`, its standard output, within 30 seconds.
fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git: {e}"))?;
    let mut stdout = child.stdout.take().expect("piped");
    let (status, bytes) = finish(child, Duration::from_secs(30), false, move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    })
    .map_err(|()| format!("git {} timed out", args.join(" ")))?;
    let bytes = bytes.map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    let status = status.map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !status.success() {
        return Err(format!("git {} failed: {status}", args.join(" ")));
    }
    Ok(bytes)
}

#[cfg(unix)]
unsafe extern "C" {
    fn killpg(pgrp: i32, signal: i32) -> i32;
}

#[cfg(unix)]
const SIGKILL: i32 = 9;

/// The exit code, or the negated signal that ended the process.
#[cfg(unix)]
fn exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| -status.signal().unwrap_or(1))
}

#[cfg(not(unix))]
fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}

/// Kill the process group `child` leads; `capture` starts each attempt in
/// its own group so descendants go with it.
#[cfg(unix)]
fn kill_group(child: &Child) {
    // SAFETY: killpg(2) takes no pointers. The caller has just found the child
    // unreaped and only that thread reaps it, so the group is still its own.
    unsafe { killpg(child.id() as i32, SIGKILL) };
}

#[cfg(not(unix))]
fn kill_group(_: &Child) {}

/// Runs Sykli with its output kept in files, in its own process group, and
/// kills the whole attempt at the limit. Returns the exit code (a negated
/// signal when one ended it) and how long it took.
fn capture(
    binary: &Path,
    cwd: &Path,
    args: &[&str],
    output: &Path,
    name: &str,
    seconds: u64,
) -> Result<(i32, u64)> {
    let started = Instant::now();
    let suffix = if name == "full-verification" {
        "txt"
    } else {
        "json"
    };
    let create =
        |file: String| File::create(output.join(&file)).map_err(|e| format!("{file}: {e}"));
    let mut command = Command::new(binary);
    command
        .args(args)
        .current_dir(cwd)
        .stdout(create(format!("{name}.{suffix}"))?)
        .stderr(create(format!("{name}.stderr"))?);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let child = command
        .spawn()
        .map_err(|e| format!("{}: {e}", binary.display()))?;
    let (status, ()) = finish(child, Duration::from_secs(seconds), true, || ())
        .map_err(|()| format!("{name} timed out"))?;
    let status = status.map_err(|e| format!("{name}: {e}"))?;
    Ok((exit_code(status), millis(started)))
}

/// Waits for `child` while `read`, on its own thread, drains what it writes.
/// At `limit` it kills the child, or its whole process group when `group`,
/// and answers `Err(())` once the child is gone. Only this thread reaps the
/// child, so whatever it signals is still the child's.
fn finish<T: Send + 'static>(
    mut child: Child,
    limit: Duration,
    group: bool,
    read: impl FnOnce() -> T + Send + 'static,
) -> Result<(std::io::Result<ExitStatus>, T), ()> {
    let reader = std::thread::spawn(read);
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Err(error) => break Err(error),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                if group {
                    kill_group(&child);
                }
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(());
            }
        }
    };
    Ok((status, reader.join().expect("the reader panicked")))
}

/// A directory under the system's temporary directory, removed with
/// everything in it.
struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path = std::env::temp_dir().join(format!("{prefix}{}-{nanos}", std::process::id()));
        fs::create_dir(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self(path))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn change(status: &str, path: &str) -> Change {
        Change {
            status: status.into(),
            path: path.into(),
        }
    }

    #[test]
    fn paths_normalize_lexically() {
        for (path, normal) in [
            ("", "."),
            (".", "."),
            ("./a", "a"),
            ("a/", "a"),
            ("a/./b/../c", "a/c"),
            ("../a", "../a"),
            ("a/../../b", "../b"),
            ("/a/../b", "/b"),
            ("/..", "/"),
            ("//a", "//a"),
            ("///a", "/a"),
        ] {
            assert_eq!(normpath(path), normal, "{path:?}");
        }
    }

    #[test]
    fn inputs_are_paths_from_the_root_through_each_tasks_workdir() {
        let contract = json!({"tasks": [
            {"name": "a", "inputs": ["src/lib.rs", "./Cargo.toml"]},
            {"name": "b", "workdir": "sub", "inputs": ["x", "../shared"]},
            {"name": "c", "workdir": "", "inputs": ["/abs"]},
            {"name": "d"}
        ]});
        assert_eq!(
            declared_inputs(&contract).unwrap(),
            BTreeSet::from(
                ["src/lib.rs", "Cargo.toml", "sub/x", "shared", "/abs"].map(String::from)
            )
        );
        assert!(declared_inputs(&json!({"tasks": [{"workdir": 1, "inputs": ["x"]}]})).is_err());
    }

    #[test]
    fn diff_output_is_read_in_pairs_and_must_be_utf8() {
        assert_eq!(
            name_status(b"M\0README.md\0D\0old name\0A\0line\nbreak\0").unwrap(),
            vec![
                change("M", "README.md"),
                change("D", "old name"),
                change("A", "line\nbreak")
            ]
        );
        assert_eq!(name_status(b"").unwrap(), vec![]);
        assert!(name_status(b"M\0").is_err());
        assert!(name_status(b"M\0\xff\0").is_err());
    }

    #[test]
    fn only_edits_to_the_listed_regular_documents_and_metadata_are_accounted() {
        let root = TempDir::new("ci-shadow-coverage-").unwrap();
        fs::create_dir(root.0.join("docs")).unwrap();
        for path in ["README.md", "docs/agents.md", "guide.md"] {
            fs::write(root.0.join(path), "after").unwrap();
        }
        let inputs = BTreeSet::from(["src/lib.rs".to_string()]);
        let covered = change_coverage(
            &[
                change("M", "src/lib.rs"),
                change("M", "README.md"),
                change("M", "docs/agents.md"),
                change("M", "sykli.json"),
                change("A", "sykli.lock"),
            ],
            &inputs,
            &root.0,
        );
        assert_eq!(
            covered,
            json!({
                "status": "accounted",
                "exempted_paths": [
                    {"path": "README.md", "reason": "repository-readme-only"},
                    {"path": "docs/agents.md", "reason": "repository-documentation"},
                    {"path": "sykli.json", "reason": "evaluation-metadata"},
                    {"path": "sykli.lock", "reason": "evaluation-metadata"}
                ],
                "unresolved_paths": []
            })
        );
        for unexempt in [
            change("A", "README.md"),
            change("D", "README.md"),
            change("M", "guide.md"),
            change("M", "docs/ci-shadow.md"),
        ] {
            let coverage = change_coverage(std::slice::from_ref(&unexempt), &inputs, &root.0);
            assert_eq!(coverage["status"], "unresolved", "{unexempt:?}");
        }
        fs::remove_file(root.0.join("README.md")).unwrap();
        std::os::unix::fs::symlink("guide.md", root.0.join("README.md")).unwrap();
        let linked = change_coverage(&[change("M", "README.md")], &inputs, &root.0);
        assert_eq!(linked["unresolved_paths"], json!(["README.md"]));
    }

    #[test]
    fn each_proposed_reuse_is_compared_with_the_full_run() {
        let receipts = TempDir::new("ci-shadow-receipts-").unwrap();
        let task = |name: &str, outcome: &str, source: &str, digests: &str| {
            json!({
                "name": name, "outcome": outcome, "source": source, "importable": true,
                "duration_ms": 10, "runtime_fingerprint": "rt", "output_digests": {"out": digests}
            })
        };
        let mut context = task("context", "passed", "task", "d");
        context["runtime_fingerprint"] = json!("another");
        let mut inherited = task("inherited", "passed", "task", "d");
        inherited["inherited_digests"] = json!({"dep": "x"});
        let mut unimportable = task("unimportable", "passed", "task", "d");
        unimportable["importable"] = json!(false);
        let full = json!({"tasks": [
            task("agrees", "passed", "task", "d"),
            task("disagrees", "passed", "task", "other"),
            task("failed", "failed", "task", "d"),
            task("errored", "errored", "task", "d"),
            task("cached", "passed", "cache", "d"),
            task("blocked", "blocked", "task", "d"),
            task("skipped", "skipped", "task", "d"),
            context, inherited, unimportable,
            task("fresh", "passed", "task", "d"),
        ]});
        let names = [
            "agrees",
            "disagrees",
            "failed",
            "errored",
            "cached",
            "blocked",
            "skipped",
            "context",
            "inherited",
            "unimportable",
        ];
        let baseline = json!({"tasks": names.map(|name| task(name, "passed", "task", "d"))});
        fs::write(receipts.0.join("base.json"), baseline.to_string()).unwrap();
        let mut explanations: Vec<Value> = names
            .iter()
            .map(|name| json!({"task": name, "cache": {"status": "available", "receipt": "base.json"}}))
            .collect();
        explanations.push(json!({"task": "fresh", "cache": {"status": "missing"}}));
        let plan = json!({"explanations": explanations});
        let affected = BTreeSet::from(["disagrees".to_string()]);
        let rows = compare(&plan, &full, &affected, &receipts.0).unwrap();
        let verdicts: BTreeMap<&str, &str> = rows
            .iter()
            .map(|row| {
                (
                    row["task"].as_str().unwrap(),
                    row["comparison"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            verdicts,
            BTreeMap::from([
                ("agrees", "agrees"),
                ("disagrees", "disagrees"),
                ("failed", "disagrees"),
                ("errored", "disagrees"),
                ("cached", "not_independently_executed"),
                ("blocked", "not_independently_executed"),
                ("skipped", "unproven"),
                ("context", "context_mismatch"),
                ("inherited", "context_mismatch"),
                ("unimportable", "unproven"),
                ("fresh", "not_proposed"),
            ])
        );
        let row = |name: &str| rows.iter().find(|row| row["task"] == name).unwrap();
        assert_eq!(row("disagrees")["affected"], true);
        assert_eq!(row("agrees")["affected"], false);
        assert_eq!(row("fresh")["full_duration_ms"], 10);
        assert_eq!(row("cached")["full_source"], "cache");

        let missing = json!({"explanations": [{"task": "absent", "cache": {"status": "missing"}}]});
        assert!(compare(&missing, &full, &affected, &receipts.0).is_err());
    }

    #[test]
    fn the_summary_escapes_what_it_quotes() {
        let report = json!({
            "status": "observed",
            "base_commit": "b", "candidate_commit": "c",
            "coverage": {
                "status": "unresolved",
                "unresolved_paths": ["line\nbreak", "<a>&'\""],
                "exempted_paths": [{"path": "README.md", "reason": "repository-readme-only"}]
            },
            "tasks": [{
                "task": "a|b\nc", "affected": true, "cache": {"status": "available"},
                "full_outcome": "passed", "full_source": "task", "comparison": "agrees"
            }],
            "potential_reused_task_ms": 5, "full_task_ms": 9, "coverage_qualified_reused_task_ms": 0,
            "baseline_run_ms": 7, "inspection_ms": 3, "independent_comparisons": 1,
            "disagreements": [], "unmapped_paths": ["x"]
        });
        let text = summary(&report);
        assert!(text.contains("- <code>line&#10;break</code>"), "{text}");
        assert!(
            text.contains("- <code>&lt;a&gt;&amp;&#x27;&quot;</code>"),
            "{text}"
        );
        assert!(
            text.contains(
                "| <code>a&#124;b&#10;c</code> | true | available | passed | task | agrees |"
            ),
            "{text}"
        );
        assert!(text.contains("Accounted path: <code>README.md</code> (repository-readme-only)."));
        assert!(text.contains("**5 ms of task duration**, from 9 ms observed."));
        assert!(text.contains("Reuse disagreements: **0**."));
        assert!(text.contains("Independent comparisons: **1**."));
        assert!(!text.contains("No independent comparison"));
        assert!(text.contains("without an exact declared input: **1**"));
        assert!(text.ends_with("are complete.\n"));

        let mut none = report.clone();
        none["independent_comparisons"] = json!(0);
        assert!(summary(&none).contains("**No independent comparison"));

        let unavailable = summary(&json!({"status": "unavailable", "error": "<boom>"}));
        assert_eq!(
            unavailable,
            "### Sykli CI shadow experiment\n\nAdvisory; the full gate remains authoritative.\n\n\
             Observation unavailable: &lt;boom&gt;\n"
        );
    }

    #[test]
    fn arguments_are_flag_value_pairs_and_four_are_required() {
        let parse = |args: &[&str]| parse_args(args.iter().map(|arg| arg.to_string()));
        let args = parse(&[
            "--base",
            "b",
            "--binary",
            "s",
            "--receipt",
            "r",
            "--output",
            "o",
            "--pr-head",
            "h",
        ])
        .unwrap();
        assert_eq!(
            (args.base.as_str(), args.pr_head.as_deref()),
            ("b", Some("h"))
        );
        assert!(parse(&["--base", "b", "--binary", "s", "--receipt", "r"]).is_err());
        assert!(parse(&["--base"]).is_err());
        assert!(parse(&["--unknown", "x"]).is_err());
        let joined = parse(&["--base=b=c", "--binary=s", "--receipt", "r", "--output=o"]).unwrap();
        assert_eq!(
            (joined.base.as_str(), joined.output.as_path()),
            ("b=c", Path::new("o"))
        );
    }

    #[test]
    fn a_child_is_waited_for_with_its_output_drained() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "printf done; exit 3"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (status, bytes) = finish(child, Duration::from_secs(10), false, move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        })
        .unwrap();
        assert_eq!(status.unwrap().code(), Some(3));
        assert_eq!(bytes.unwrap(), b"done");

        let slow = Command::new("/bin/sh")
            .args(["-c", "sleep 30"])
            .spawn()
            .unwrap();
        let started = Instant::now();
        assert!(finish(slow, Duration::from_millis(200), false, || ()).is_err());
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn an_attempt_reports_its_exit_code_or_its_signal_negated_and_keeps_its_output() {
        let out = TempDir::new("ci-shadow-capture-").unwrap();
        let sh = Path::new("/bin/sh");
        let run = |script: &str, name: &str| capture(sh, &out.0, &["-c", script], &out.0, name, 10);
        assert_eq!(
            run("echo '{}'; echo oops >&2; exit 2", "plain").unwrap().0,
            2
        );
        assert_eq!(
            fs::read_to_string(out.0.join("plain.json")).unwrap(),
            "{}\n"
        );
        assert_eq!(
            fs::read_to_string(out.0.join("plain.stderr")).unwrap(),
            "oops\n"
        );
        assert_eq!(run("echo ok", "full-verification").unwrap().0, 0);
        assert_eq!(
            fs::read_to_string(out.0.join("full-verification.txt")).unwrap(),
            "ok\n"
        );
        assert_eq!(run("kill -9 $$", "killed").unwrap().0, -SIGKILL);
    }

    #[test]
    fn a_timeout_kills_the_whole_attempt_and_says_so() {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        let out = TempDir::new("ci-shadow-timeout-").unwrap();
        let grandchild = out.0.join("grandchild");
        let script = format!("sleep 30 & echo $! > '{}'; wait", grandchild.display());
        let started = Instant::now();
        let error = capture(
            Path::new("/bin/sh"),
            &out.0,
            &["-c", &script],
            &out.0,
            "slow",
            1,
        )
        .unwrap_err();
        assert_eq!(error, "slow timed out");
        assert!(started.elapsed() < Duration::from_secs(10));
        let pid: i32 = fs::read_to_string(&grandchild)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // It was in the attempt's process group; once its new parent reaps
        // it, it is gone.
        let deadline = Instant::now() + Duration::from_secs(5);
        // SAFETY: kill(2) with signal 0 sends nothing and takes no pointers.
        while unsafe { kill(pid, 0) } == 0 {
            assert!(
                Instant::now() < deadline,
                "process {pid} outlived the attempt"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_copied_tree_keeps_its_modification_times_and_refuses_to_merge() {
        let root = TempDir::new("ci-shadow-copy-").unwrap();
        let from = root.0.join("from");
        fs::create_dir_all(from.join("nested")).unwrap();
        fs::write(from.join("nested/receipt.json"), "{}").unwrap();
        let then = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        File::options()
            .write(true)
            .open(from.join("nested/receipt.json"))
            .unwrap()
            .set_modified(then)
            .unwrap();
        copy_tree(&from, &root.0.join("to")).unwrap();
        let copied = root.0.join("to/nested/receipt.json");
        assert_eq!(fs::read_to_string(&copied).unwrap(), "{}");
        assert_eq!(fs::metadata(&copied).unwrap().modified().unwrap(), then);
        assert!(copy_tree(&from, &root.0.join("to")).is_err());
    }
}
