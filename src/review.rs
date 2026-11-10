//! `sykli review`: a fresh agent reads the change checked out and says how
//! serious what it found is.
//!
//! The change is every commit between the remote's default branch and HEAD.
//! The reviewer is another agent than the one that wrote it where one is
//! installed, started with reading tools only and none of the author's
//! conversation.
//!
//! Exit codes:
//!
//! - 0: `fine`, `look`, a blocking review a person overruled, or nothing to
//!   review (the commit is already in the base).
//! - 1: `blocking`.
//! - 2: no review happened: uncommitted work, no reviewer installed, a
//!   reviewer that failed, ran out of time or gave no verdict. Never a pass.
//!
//! What a review found is kept as a git note on the commit's tree
//! (`refs/notes/sykli/review`), so every worktree of the clone reads it and
//! `sykli land` can push it. It answers only for the base it was read
//! against: the same tree on another base is another change. One note per
//! tree, replaced when the tree is reviewed again, so the notes grow with the
//! trees reviewed, as the commits themselves do.

use crate::{hook, repo};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long a reviewer may take, from the first byte of its prompt to the
/// last byte of its reply, before it counts as no review.
const TIME_LIMIT: Duration = Duration::from_secs(20 * 60);
const SCHEMA: &str = "sykli-review.v1";
/// Where reviews are kept: one note per tree.
pub(crate) const NOTES: &str = "refs/notes/sykli/review";

const FINE: u8 = 0;
const BLOCKING: u8 = 1;
const NO_REVIEW: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Level {
    Fine,
    Look,
    Blocking,
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Fine => "fine",
            Self::Look => "look",
            Self::Blocking => "blocking",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Agent {
    Codex,
    Claude,
}

impl Agent {
    /// Reviewers in the order they are tried.
    const ALL: [Self; 2] = [Self::Codex, Self::Claude];

    fn program(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

impl fmt::Display for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.program())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Finding {
    pub(crate) level: Level,
    pub(crate) text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Verdict {
    pub(crate) level: Level,
    pub(crate) findings: Vec<Finding>,
}

/// Whether the reviewer is another agent than the change's authors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Independent {
    Yes,
    No,
    /// No commit of the change names the agent that wrote it.
    Unknown,
}

/// A person let a blocking review cross. Who cannot be known, so the reason
/// and the time are what is kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Overruled {
    pub(crate) why: String,
    pub(crate) at: u64,
}

/// `sykli-review.v1`: what one review of one tree against one base found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Record {
    pub(crate) schema: String,
    pub(crate) tree: String,
    pub(crate) head: String,
    pub(crate) base: String,
    pub(crate) level: Level,
    pub(crate) findings: Vec<Finding>,
    pub(crate) reviewer: Agent,
    pub(crate) authors: Vec<Agent>,
    pub(crate) independent: Independent,
    pub(crate) at: u64,
    pub(crate) overruled: Option<Overruled>,
}

/// What the reviewer's reply says. The verdict is its one `REVIEW:` line; a
/// reply without one, with several, or with a level that is not one of the
/// three, is no review. A verdict milder than its own worst finding is
/// raised to it.
pub(crate) fn parse_verdict(reply: &str) -> Result<Verdict, String> {
    let mut stated = Vec::new();
    let mut findings = Vec::new();
    for line in reply.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("REVIEW:") {
            let word = rest
                .trim()
                .trim_matches(|c| c == '*' || c == '`' || c == '.');
            stated.push(match word.to_ascii_lowercase().as_str() {
                "fine" => Level::Fine,
                "look" => Level::Look,
                "blocking" => Level::Blocking,
                other => {
                    return Err(format!(
                        "the reviewer's verdict `{other}` is not fine, look or blocking"
                    ));
                }
            });
        } else if let Some(text) = line.strip_prefix("- [blocking]") {
            findings.push(finding(Level::Blocking, text));
        } else if let Some(text) = line.strip_prefix("- [look]") {
            findings.push(finding(Level::Look, text));
        }
    }
    let stated = match stated[..] {
        [] => return Err("the reviewer gave no `REVIEW:` verdict line".into()),
        [stated] => stated,
        _ => {
            return Err(format!(
                "the reviewer gave {} `REVIEW:` verdict lines, not one",
                stated.len()
            ));
        }
    };
    let worst = findings
        .iter()
        .map(|finding| finding.level)
        .max()
        .unwrap_or(Level::Fine);
    Ok(Verdict {
        level: stated.max(worst),
        findings,
    })
}

fn finding(level: Level, text: &str) -> Finding {
    Finding {
        level,
        text: text.trim().to_string(),
    }
}

/// The agents the change's commits name as co-authors. Only a trailer says
/// so: a commit without one has an author no one can name.
pub(crate) fn authors(trailers: &str) -> BTreeSet<Agent> {
    let lower = trailers.to_ascii_lowercase();
    let mut found = BTreeSet::new();
    if lower.contains("claude") || lower.contains("anthropic") {
        found.insert(Agent::Claude);
    }
    if lower.contains("codex") || lower.contains("openai") {
        found.insert(Agent::Codex);
    }
    found
}

/// The reviewer: the first installed agent that did not write the change,
/// else the first installed one, in a fresh session.
pub(crate) fn choose(
    authors: &BTreeSet<Agent>,
    installed: &[Agent],
) -> Option<(Agent, Independent)> {
    if authors.is_empty() {
        return installed
            .first()
            .map(|agent| (*agent, Independent::Unknown));
    }
    installed
        .iter()
        .find(|agent| !authors.contains(agent))
        .map(|agent| (*agent, Independent::Yes))
        .or_else(|| installed.first().map(|agent| (*agent, Independent::No)))
}

/// 0 when the change may cross, 1 when a blocking review stands.
pub(crate) fn exit_code(record: &Record) -> u8 {
    if record.level == Level::Blocking && record.overruled.is_none() {
        BLOCKING
    } else {
        FINE
    }
}

/// The most of a diff a reviewer is handed, in characters; past it the
/// reviewer reads the files.
const DIFF_LIMIT: usize = 300_000;

pub(crate) fn prompt(base: &str, head: &str, log: &str, diff: &str) -> String {
    let (shown, cut) = match diff.char_indices().nth(DIFF_LIMIT) {
        Some((at, _)) => (
            &diff[..at],
            "\n[The diff is cut here: read the changed files for the rest.]\n",
        ),
        None => (diff, ""),
    };
    format!(
        "You are reviewing a change in this repository. You did not write it.\n\
         \n\
         The change is every commit in `{base}..{head}`. Its commits and its diff are \
         at the end of this message. Read the files it touches as far as you need to \
         judge it. Change nothing: no edits, no commits, no pushes.\n\
         \n\
         Judge only what would matter to the person who owns this repository:\n\
         - blocking: it is wrong or harmful as written. A bug the change introduces, \
         lost or corrupted data, a check loosened to pass, a secret, a broken rule of \
         this repository's own instructions for contributors (an AGENTS.md or the \
         like), where it has them.\n\
         - look: it works, but a person should know about it before it lands. A risky \
         design choice, a missing test for new behaviour, text that no longer matches \
         the code.\n\
         Style, naming and things you would merely have done differently are not \
         findings.\n\
         \n\
         Reply with one line per finding, each starting with `- [blocking]` or \
         `- [look]`, then the file and line, then one sentence. End with exactly one \
         line: `REVIEW: fine`, `REVIEW: look` or `REVIEW: blocking`. With no findings, \
         reply with only `REVIEW: fine`.\n\
         \n\
         Commits:\n{log}\n\
         Diff:\n{shown}{cut}"
    )
}

/// Runs the reviewer on a prompt and returns its reply.
pub(crate) type Reviewer<'a> = &'a dyn Fn(Agent, &str, &Path) -> Result<String, String>;

/// Reviews the commit checked out at `root` against `base_ref`, or reuses
/// what this clone keeps for its tree. Returns `None` when the commit is
/// already in the base.
pub(crate) fn review(
    root: &Path,
    base_ref: &str,
    installed: &[Agent],
    reviewer: Reviewer<'_>,
) -> Result<Option<Record>, String> {
    if uncommitted(root)? {
        return Err(
            "uncommitted or untracked files here: a review is of a commit, so commit first".into(),
        );
    }
    let head = git(root, &["rev-parse", "HEAD"])?;
    let tree = git(root, &["rev-parse", "HEAD^{tree}"])?;
    let base = git(root, &["merge-base", base_ref, "HEAD"])
        .map_err(|error| format!("no common ancestor with {base_ref} ({error}): fetch it first"))?;
    if base == head {
        return Ok(None);
    }
    // A kept review answers only for the change it read: the same tree on
    // another base is another diff.
    if let Some(kept) = read_record(root, &tree)?.filter(|kept| kept.base == base) {
        return Ok(Some(kept));
    }
    let range = format!("{base}..{head}");
    let trailers = git(
        root,
        &[
            "log",
            "--format=%(trailers:key=Co-Authored-By,valueonly)",
            &range,
        ],
    )?;
    let authors = authors(&trailers);
    let Some((agent, independent)) = choose(&authors, installed) else {
        return Err("no reviewer is installed: neither `codex` nor `claude` is on PATH".into());
    };
    let log = git(root, &["log", "--format=%h %s", &range])?;
    let diff = git(
        root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            &base,
            &head,
        ],
    )?;
    let reply = reviewer(agent, &prompt(&base, &head, &log, &diff), root)?;
    let verdict = parse_verdict(&reply)?;
    if git(root, &["rev-parse", "HEAD^{tree}"])? != tree || uncommitted(root)? {
        return Err(format!(
            "the worktree changed while {agent} reviewed it: nothing was recorded"
        ));
    }
    let record = Record {
        schema: SCHEMA.to_string(),
        tree,
        head,
        base,
        level: verdict.level,
        findings: verdict.findings,
        reviewer: agent,
        authors: authors.into_iter().collect(),
        independent,
        at: now(),
        overruled: None,
    };
    write_record(root, &record)?;
    Ok(Some(record))
}

/// A person lets a blocking review of this commit's tree cross.
pub(crate) fn overrule(root: &Path, base_ref: &str, why: &str) -> Result<Record, String> {
    let tree = git(root, &["rev-parse", "HEAD^{tree}"])?;
    let base = git(root, &["merge-base", base_ref, "HEAD"])?;
    let Some(mut record) = read_record(root, &tree)?.filter(|kept| kept.base == base) else {
        return Err("no review is kept for this commit: run `sykli review` first".into());
    };
    if record.level != Level::Blocking {
        return Err(format!(
            "the review of this commit is `{}`: nothing to overrule",
            record.level
        ));
    }
    record.overruled = Some(Overruled {
        why: why.to_string(),
        at: now(),
    });
    write_record(root, &record)?;
    Ok(record)
}

/// What an agent session sets in its environment. None of them reaches the
/// reviewer, and none may be set where a review is overruled.
const AGENT_MARKERS: [&str; 10] = [
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_THREAD_ID",
    "CODEX_SESSION_ID",
    "PI_SESSION_ID",
    "PI_CODING_AGENT",
    "OPENCODE",
    "OPENCODE_SESSION_ID",
    "AI_AGENT",
];

/// Only a person at a terminal overrules: an agent session is refused.
pub(crate) fn person_at_terminal(env: &dyn Fn(&str) -> bool, terminal: bool) -> Result<(), String> {
    if let Some(marker) = AGENT_MARKERS.iter().find(|name| env(name)) {
        return Err(format!(
            "only a person overrules a review: this is an agent session ({marker} is set)"
        ));
    }
    if !terminal {
        return Err("only a person overrules a review: run this at a terminal".into());
    }
    Ok(())
}

pub(crate) fn summary(record: &Record) -> String {
    let count = |level| {
        record
            .findings
            .iter()
            .filter(|finding| finding.level == level)
            .count()
    };
    let mut line = format!(
        "review: {} ({} blocking, {} to look at) by {}",
        record.level,
        count(Level::Blocking),
        count(Level::Look),
        record.reviewer
    );
    line.push_str(match record.independent {
        Independent::Yes => ", another agent than the author",
        Independent::No => ", the same agent as the author in a fresh session",
        Independent::Unknown => ", author agent unknown",
    });
    if let Some(overruled) = &record.overruled {
        line.push_str(&format!("; overruled by a person: {}", overruled.why));
    }
    line
}

/// The review this clone keeps for a tree. A note that is not a review of
/// that tree is an error, never "no review".
pub(crate) fn read_record(root: &Path, tree: &str) -> Result<Option<Record>, String> {
    let Some(bytes) = read_note(root, NOTES, tree)? else {
        return Ok(None);
    };
    let record: Record = serde_json::from_slice(&bytes)
        .map_err(|error| format!("the note on {tree} in {NOTES} is not a review: {error}"))?;
    if record.schema != SCHEMA {
        return Err(format!(
            "the review of {tree} has schema `{}`, not {SCHEMA}",
            record.schema
        ));
    }
    if record.tree != tree {
        return Err(format!(
            "the review kept on {tree} is of another tree, {}",
            record.tree
        ));
    }
    Ok(Some(record))
}

fn write_record(root: &Path, record: &Record) -> Result<(), String> {
    let text = serde_json::to_string_pretty(record).map_err(|error| error.to_string())? + "\n";
    write_note(root, NOTES, &record.tree, text.as_bytes())
}

/// The note `notes` holds on `object`, byte for byte, or None when there is
/// none. Any other failure is an error: unknown is not "no note".
pub(crate) fn read_note(root: &Path, notes: &str, object: &str) -> Result<Option<Vec<u8>>, String> {
    let output = git_command(root)
        .args(["notes", "--ref", notes, "show", object])
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(Some(output.stdout));
    }
    // git says "no note found" with exit 1; anything else is an error.
    let said = String::from_utf8_lossy(&output.stderr);
    if said.to_ascii_lowercase().contains("no note found") {
        return Ok(None);
    }
    Err(format!("git notes show {object}: {}", said.trim()))
}

/// Put `bytes` on `object` as its note in `notes`, replacing the one there.
/// The bytes are stored as a blob first and named, so the note holds exactly
/// them: a message given to `git notes` directly is tidied on the way in.
pub(crate) fn write_note(
    root: &Path,
    notes: &str,
    object: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let blob = git_with_input(root, &["hash-object", "-w", "--stdin"], bytes)?;
    git(
        root,
        &["notes", "--ref", notes, "add", "-f", "-C", &blob, object],
    )
    .map(|_| ())
}

fn git_with_input(root: &Path, args: &[&str], input: &[u8]) -> Result<String, String> {
    let mut child = git_command(root)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    // Written beside the wait: git may answer before it has read everything.
    let mut stdin = child.stdin.take().ok_or("git's stdin is piped")?;
    let bytes = input.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&bytes));
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    let written = writer.join().map_err(|_| "writing to git panicked")?;
    if !output.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    written.map_err(|error| format!("git {}: {error}", args.join(" ")))?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// A git command run at `root`, saying what it says in one language: its
/// "no note found" is read.
fn git_command(root: &Path) -> Command {
    let mut command = repo::git_command();
    command.current_dir(root).env("LC_ALL", "C");
    command
}

pub(crate) fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = git_command(root)
        .args(args)
        .output()
        .map_err(|error| format!("could not start git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Whether anything at `root` is uncommitted or untracked, sykli's own
/// `.sykli/` aside (a run writes its receipts there, and receipt trees leave
/// it out too).
pub(crate) fn uncommitted(root: &Path) -> Result<bool, String> {
    let output = git_command(root)
        .args(["status", "--porcelain", "-z"])
        .output()
        .map_err(|error| format!("could not start git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git status: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .any(|entry| !entry.is_empty() && !entry.starts_with(b"?? .sykli/")))
}

/// Whether `name` is a commit this clone has.
pub(crate) fn commit_exists(root: &Path, name: &str) -> bool {
    git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{name}^{{commit}}"),
        ],
    )
    .is_ok()
}

/// The remote's default branch, by the rule the pre-push hook gates with:
/// the branch named with `--branch`, else the one `refs/remotes/REMOTE/HEAD`
/// names. Where that is unset or names a ref that is gone, the hook gates
/// both `main` and `master`; a review is against one branch, so it is
/// whichever of the two this clone has for the remote, and with both or
/// neither it cannot be told.
pub(crate) fn default_branch(
    root: &Path,
    remote: &str,
    named: Option<&str>,
) -> Result<String, String> {
    // A leading `-` is a valid ref name's start, and an option to git.
    if remote.starts_with('-') || !repo::remote_name(remote) {
        return Err(format!("--remote {remote:?} is not a remote name"));
    }
    if let Some(branch) = named {
        if !repo::branch_name(branch) {
            return Err(format!(
                "--branch {branch:?} is not a branch name; name it as `git push REMOTE NAME` \
                 would (main, not refs/heads/main)"
            ));
        }
        return Ok(branch.to_string());
    }
    let head = format!("refs/remotes/{remote}/HEAD");
    let target = git(root, &["symbolic-ref", "--quiet", &head]).ok();
    let exists = target
        .as_deref()
        .is_some_and(|target| commit_exists(root, target));
    let (candidates, _) = hook::default_branches(Some(remote), target.as_deref(), exists);
    let known = candidates
        .iter()
        .filter_map(|name| name.strip_prefix("refs/heads/"))
        .filter(|branch| {
            candidates.len() == 1 || commit_exists(root, &format!("refs/remotes/{remote}/{branch}"))
        })
        .collect::<Vec<_>>();
    match known[..] {
        [branch] => Ok(branch.to_string()),
        _ => Err(format!(
            "{remote}'s default branch is unknown here: name it with `git remote set-head \
             {remote} --auto`, or with --branch"
        )),
    }
}

/// The agents on PATH, in the order reviewers are tried.
pub(crate) fn installed() -> Vec<Agent> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    Agent::ALL
        .into_iter()
        .filter(|agent| std::env::split_paths(&path).any(|dir| dir.join(agent.program()).is_file()))
        .collect()
}

/// A directory of this process's own under the temporary directory, named
/// `PREFIX-<unguessable>` and removed, with what it holds, when dropped.
pub(crate) struct Scratch(PathBuf);

impl Scratch {
    pub(crate) fn new(prefix: &str) -> Result<Self, String> {
        crate::fresh_directory(
            &std::env::temp_dir(),
            (0..4).map(|_| format!("{prefix}-{}", crate::private_name())),
        )
        .map(Scratch)
        .map_err(|error| format!("no temporary directory could be made: {error}"))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Ours alone, and under the temporary directory: a failure to remove
        // it changes nothing about the review.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Starts the agent with reading tools only, gives it the prompt on stdin
/// and returns what it said last.
pub(crate) fn run_agent(agent: Agent, prompt: &str, root: &Path) -> Result<String, String> {
    let scratch = Scratch::new("sykli-review")?;
    let last = scratch.path().join("last-message.txt");
    let mut command = reviewer_command(agent, &last);
    command.current_dir(root);
    let printed = converse(command, &agent.to_string(), prompt, TIME_LIMIT)?;
    match agent {
        Agent::Claude => Ok(printed),
        Agent::Codex => fs::read_to_string(&last)
            .map_err(|error| format!("{agent} wrote no last message: {error}")),
    }
}

/// Starts the command, hands it the prompt and returns what it printed, or
/// fails once it has run longer than `limit`. The one limit covers handing
/// over the prompt, the run, and reading what it printed.
fn converse(
    mut command: Command,
    who: &str,
    prompt: &str,
    limit: Duration,
) -> Result<String, String> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not start {who}: {error}"))?;
    // The prompt is written beside the wait, so a reviewer that never reads
    // it runs into the time limit instead of holding this command forever.
    let mut stdin = child.stdin.take().ok_or("the reviewer's stdin is piped")?;
    let text = prompt.to_string();
    let writer = beside(move || stdin.write_all(text.as_bytes()));
    let reader = drain(
        child
            .stdout
            .take()
            .ok_or("the reviewer's stdout is piped")?,
    );
    let said = drain(
        child
            .stderr
            .take()
            .ok_or("the reviewer's stderr is piped")?,
    );
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => return Err(format!("could not wait for {who}: {error}")),
        }
        if started.elapsed() > limit {
            // Killed, its pipes are closed: the writer ends by itself. A
            // kill that fails leaves a process that already ended.
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "{who} did not finish its review in {} seconds",
                limit.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // The same limit covers reading what it printed: a process it started
    // may hold the pipes open after it ended.
    let deadline = started + limit;
    let printed = by(&reader, deadline, who)?;
    let said = by(&said, deadline, who)?;
    let handed = by(&writer, deadline, who);
    if !status.success() {
        // What the reviewer said while working is shown only when it failed.
        eprint!("{said}");
        return Err(format!("{who} ended with {status} before giving a review"));
    }
    handed.map_err(|error| format!("{who} did not take its whole prompt: {error}"))?;
    Ok(printed)
}

type Arriving<T> = std::sync::mpsc::Receiver<std::io::Result<T>>;

/// Runs `work` on a thread of its own and hands back where its result
/// arrives, so a wait for it can be given up.
fn beside<T: Send + 'static>(
    work: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Arriving<T> {
    let (done, result) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // The receiver is gone when the wait was given up: nothing to tell.
        let _ = done.send(work());
    });
    result
}

fn drain(mut from: impl Read + Send + 'static) -> Arriving<String> {
    beside(move || {
        let mut bytes = Vec::new();
        from.read_to_end(&mut bytes)
            .map(|_| String::from_utf8_lossy(&bytes).into_owned())
    })
}

/// What `beside` work produced, unless it is still going at the deadline.
fn by<T>(result: &Arriving<T>, deadline: Instant, who: &str) -> Result<T, String> {
    let left = deadline.saturating_duration_since(Instant::now());
    match result.recv_timeout(left) {
        Ok(done) => done.map_err(|error| error.to_string()),
        Err(_) => Err(format!(
            "{who} ended, but something it started still holds its input or output at the \
             time limit"
        )),
    }
}

/// The reviewer's command line. It starts as a session of its own: no agent
/// marker of the session that asked for the review reaches it.
fn reviewer_command(agent: Agent, last: &Path) -> Command {
    let mut command = Command::new(agent.program());
    for marker in AGENT_MARKERS {
        command.env_remove(marker);
    }
    match agent {
        Agent::Codex => {
            command
                .args([
                    "exec",
                    "--sandbox",
                    "read-only",
                    // No configured server or connector reaches the reviewer.
                    "--ignore-user-config",
                    "--ephemeral",
                    "-o",
                ])
                .arg(last)
                .arg("-");
        }
        Agent::Claude => {
            // Reading tools only: no shell, no editing tool, no configured
            // server, and none of the person's or the project's settings, so
            // no hook of theirs runs a command either.
            command.args([
                "-p",
                "--tools",
                "Read,Grep,Glob",
                "--strict-mcp-config",
                "--setting-sources",
                "",
                "--settings",
                r#"{"disableAllHooks":true}"#,
            ]);
        }
    }
    command
}

/// `sykli review [--remote NAME] [--branch NAME] [--overrule WHY]`.
pub(crate) fn command(remote: &str, branch: Option<&str>, overrule_why: Option<&str>) -> ExitCode {
    let outcome = repo::repository()
        .map_err(|_| "not inside a git repository".to_string())
        .and_then(|repository| {
            let root = repository.root.as_path();
            let branch = default_branch(root, remote, branch)?;
            let base_ref = format!("refs/remotes/{remote}/{branch}");
            match overrule_why {
                None => reviewed(root, &base_ref, &format!("{remote}/{branch}")),
                Some(why) if why.trim().is_empty() => {
                    Err("--overrule takes the reason, in words".to_string())
                }
                Some(why) => person_at_terminal(
                    &|name| std::env::var_os(name).is_some(),
                    std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
                )
                .and_then(|()| overrule(root, &base_ref, why))
                .map(|record| {
                    println!("{}", summary(&record));
                    FINE
                }),
            }
        });
    ExitCode::from(outcome.unwrap_or_else(|error| {
        eprintln!("review: no review happened: {error}");
        NO_REVIEW
    }))
}

/// Review, and say what was found: one line, then the findings.
fn reviewed(root: &Path, base_ref: &str, shown: &str) -> Result<u8, String> {
    let Some(record) = review(root, base_ref, &installed(), &run_agent)? else {
        println!("review: nothing to review: this commit is already in {shown}");
        return Ok(FINE);
    };
    println!("{}", summary(&record));
    for finding in &record.findings {
        println!("  [{}] {}", finding.level, finding.text);
    }
    let code = exit_code(&record);
    if code == BLOCKING {
        eprintln!(
            "review: blocking stops the landing. Fix it and commit, or a person runs \
             `sykli review --overrule \"<why>\"` at a terminal."
        );
    }
    Ok(code)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    const BASE: &str = "refs/remotes/origin/main";

    #[test]
    fn a_reply_without_a_verdict_line_is_no_review() {
        let error = parse_verdict("Looks good to me overall.\n").unwrap_err();
        assert!(error.contains("no `REVIEW:`"), "{error}");
        assert!(parse_verdict("").is_err());
    }

    #[test]
    fn a_verdict_that_is_not_one_of_the_three_is_no_review() {
        let error = parse_verdict("REVIEW: approved\n").unwrap_err();
        assert!(error.contains("approved"), "{error}");
    }

    #[test]
    fn the_verdict_is_a_line_of_its_own_and_findings_are_kept() {
        let verdict = parse_verdict(
            "I first thought REVIEW: blocking but checked.\n\
             - [look] src/cli.rs:10: the help text names a removed flag.\n\
             REVIEW: look\n",
        )
        .unwrap();
        assert_eq!(verdict.level, Level::Look);
        assert_eq!(verdict.findings.len(), 1);
        assert_eq!(
            verdict.findings[0].text,
            "src/cli.rs:10: the help text names a removed flag."
        );
    }

    #[test]
    fn two_verdict_lines_are_no_review() {
        let error = parse_verdict("REVIEW: blocking\nREVIEW: fine\n").unwrap_err();
        assert!(error.contains("2 `REVIEW:`"), "{error}");
        assert!(parse_verdict("REVIEW: fine\nREVIEW: fine\n").is_err());
    }

    #[test]
    fn a_verdict_milder_than_its_worst_finding_is_raised() {
        let verdict =
            parse_verdict("- [blocking] src/run.rs:3: drops the exit code.\nREVIEW: fine\n")
                .unwrap();
        assert_eq!(verdict.level, Level::Blocking);
        // Wherever the finding stands, after the verdict too.
        let verdict = parse_verdict("REVIEW: fine\n- [look] a.rs:1: untested.\n").unwrap();
        assert_eq!(verdict.level, Level::Look);
    }

    #[test]
    fn markdown_around_the_verdict_word_is_read() {
        assert_eq!(
            parse_verdict("REVIEW: **Fine**").unwrap().level,
            Level::Fine
        );
    }

    #[test]
    fn the_reviewer_is_another_agent_than_the_author_when_one_is_installed() {
        let claude = authors("Claude <noreply@anthropic.com>\n");
        assert_eq!(
            choose(&claude, &[Agent::Codex, Agent::Claude]),
            Some((Agent::Codex, Independent::Yes))
        );
        assert_eq!(
            choose(&claude, &[Agent::Claude]),
            Some((Agent::Claude, Independent::No))
        );
        let codex = authors("Codex <noreply@openai.com>\n");
        assert_eq!(
            choose(&codex, &[Agent::Codex, Agent::Claude]),
            Some((Agent::Claude, Independent::Yes))
        );
        assert_eq!(
            choose(&authors(""), &[Agent::Codex, Agent::Claude]),
            Some((Agent::Codex, Independent::Unknown))
        );
        assert_eq!(choose(&claude, &[]), None);
    }

    #[test]
    fn an_agent_or_a_pipe_cannot_overrule() {
        let error = person_at_terminal(&|name| name == "CLAUDECODE", true).unwrap_err();
        assert!(error.contains("CLAUDECODE"), "{error}");
        assert!(person_at_terminal(&|_| false, false).is_err());
        assert!(person_at_terminal(&|_| false, true).is_ok());
    }

    #[test]
    fn a_blocking_review_fails_until_a_person_overrules_it() {
        let root = branch_repo("review-blocking");
        let calls = Cell::new(0);
        let blocking = |_: Agent, _: &str, _: &Path| {
            calls.set(calls.get() + 1);
            Ok("- [blocking] new.txt:1: wrong.\nREVIEW: blocking\n".to_string())
        };

        let record = review(&root, BASE, &[Agent::Codex], &blocking)
            .unwrap()
            .unwrap();
        assert_eq!(record.level, Level::Blocking);
        assert_eq!(record.schema, "sykli-review.v1");
        assert_eq!(exit_code(&record), 1);

        // The same change is not reviewed twice: the kept record answers.
        let again = review(&root, BASE, &[Agent::Codex], &blocking)
            .unwrap()
            .unwrap();
        assert_eq!(again, record);
        assert_eq!(calls.get(), 1);

        let overruled = overrule(&root, BASE, "known and accepted").unwrap();
        assert_eq!(exit_code(&overruled), 0);
        let kept = review(&root, BASE, &[Agent::Codex], &blocking)
            .unwrap()
            .unwrap();
        assert_eq!(exit_code(&kept), 0);
        assert_eq!(kept.overruled.unwrap().why, "known and accepted");
    }

    #[test]
    fn the_record_is_a_note_on_the_tree_with_every_field() {
        let root = branch_repo("review-note");
        run_git(
            &root,
            &[
                "commit",
                "-q",
                "--amend",
                "-m",
                "change\n\nCo-Authored-By: Claude <noreply@anthropic.com>",
            ],
        );
        let look = |_: Agent, _: &str, _: &Path| {
            Ok("- [look] new.txt:1: untested.\nREVIEW: look\n".to_string())
        };
        review(&root, BASE, &[Agent::Codex, Agent::Claude], &look)
            .unwrap()
            .unwrap();
        let tree = git(&root, &["rev-parse", "HEAD^{tree}"]).unwrap();
        let note = git(&root, &["notes", "--ref", NOTES, "show", &tree]).unwrap();
        let note: serde_json::Value = serde_json::from_str(&note).unwrap();
        assert_eq!(note["schema"], "sykli-review.v1");
        assert_eq!(note["tree"], tree.as_str());
        assert_eq!(
            note["head"],
            git(&root, &["rev-parse", "HEAD"]).unwrap().as_str()
        );
        assert_eq!(
            note["base"],
            git(&root, &["rev-parse", BASE]).unwrap().as_str()
        );
        assert_eq!(note["level"], "look");
        assert_eq!(note["findings"][0]["level"], "look");
        assert_eq!(note["findings"][0]["text"], "new.txt:1: untested.");
        assert_eq!(note["reviewer"], "codex");
        assert_eq!(note["authors"], serde_json::json!(["claude"]));
        assert_eq!(note["independent"], "yes");
        assert!(note["at"].as_u64().unwrap() > 0);
        assert!(note["overruled"].is_null());
    }

    #[test]
    fn a_note_that_is_not_a_review_of_the_tree_is_an_error_not_no_review() {
        let root = branch_repo("review-foreign-note");
        let tree = git(&root, &["rev-parse", "HEAD^{tree}"]).unwrap();
        write_note(&root, NOTES, &tree, b"not json\n").unwrap();
        let never = |_: Agent, _: &str, _: &Path| -> Result<String, String> {
            panic!("an unreadable kept review is not replaced by a new one")
        };
        let error = review(&root, BASE, &[Agent::Codex], &never).unwrap_err();
        assert!(error.contains("is not a review"), "{error}");

        // A review of another tree, copied here, is not this tree's review,
        // and neither is one written by a later schema.
        let record = |tree: &str, schema: &str| {
            serde_json::to_vec(&Record {
                schema: schema.into(),
                tree: tree.into(),
                head: "head".into(),
                base: "base".into(),
                level: Level::Fine,
                findings: Vec::new(),
                reviewer: Agent::Codex,
                authors: Vec::new(),
                independent: Independent::Unknown,
                at: 1,
                overruled: None,
            })
            .unwrap()
        };
        write_note(&root, NOTES, &tree, &record("another-tree", SCHEMA)).unwrap();
        let error = read_record(&root, &tree).unwrap_err();
        assert!(error.contains("of another tree"), "{error}");
        write_note(&root, NOTES, &tree, &record(&tree, "sykli-review.v2")).unwrap();
        let error = read_record(&root, &tree).unwrap_err();
        assert!(error.contains("sykli-review.v2"), "{error}");
        write_note(&root, NOTES, &tree, &record(&tree, SCHEMA)).unwrap();
        assert_eq!(read_record(&root, &tree).unwrap().unwrap().at, 1);
    }

    #[test]
    fn a_new_commit_is_reviewed_again() {
        let root = branch_repo("review-again");
        let calls = Cell::new(0);
        let fine = |_: Agent, _: &str, _: &Path| {
            calls.set(calls.get() + 1);
            Ok("REVIEW: fine\n".to_string())
        };
        review(&root, BASE, &[Agent::Claude], &fine)
            .unwrap()
            .unwrap();
        fs::write(root.join("new.txt"), "changed\n").unwrap();
        run_git(&root, &["commit", "-q", "-am", "second"]);
        let second = review(&root, BASE, &[Agent::Claude], &fine)
            .unwrap()
            .unwrap();
        assert_eq!(calls.get(), 2);
        assert_eq!(second.level, Level::Fine);
        assert!(
            overrule(&root, BASE, "why").is_err(),
            "fine has nothing to overrule"
        );
    }

    #[test]
    fn uncommitted_work_a_missing_reviewer_and_no_verdict_are_no_review() {
        let root = branch_repo("review-none");
        let silent = |_: Agent, _: &str, _: &Path| Ok("I could not decide.\n".to_string());

        let error = review(&root, BASE, &[], &silent).unwrap_err();
        assert!(error.contains("no reviewer"), "{error}");
        let error = review(&root, BASE, &[Agent::Codex], &silent).unwrap_err();
        assert!(error.contains("no `REVIEW:`"), "{error}");
        let failed = |_: Agent, _: &str, _: &Path| Err("codex ended with exit 1".to_string());
        assert!(review(&root, BASE, &[Agent::Codex], &failed).is_err());
        // None of them left a record that a later run would take for a review.
        let tree = git(&root, &["rev-parse", "HEAD^{tree}"]).unwrap();
        assert_eq!(read_record(&root, &tree).unwrap(), None);

        let fine = |_: Agent, _: &str, _: &Path| Ok("REVIEW: fine\n".to_string());
        fs::write(root.join("stray.txt"), "x\n").unwrap();
        let error = review(&root, BASE, &[Agent::Codex], &fine).unwrap_err();
        assert!(error.contains("commit first"), "{error}");
        fs::remove_file(root.join("stray.txt")).unwrap();
        fs::write(root.join("new.txt"), "edited\n").unwrap();
        let error = review(&root, BASE, &[Agent::Codex], &fine).unwrap_err();
        assert!(error.contains("commit first"), "{error}");
    }

    #[test]
    fn what_a_run_left_in_dot_sykli_is_not_uncommitted_work() {
        let root = branch_repo("review-dot-sykli");
        fs::create_dir_all(root.join(".sykli/receipts")).unwrap();
        fs::write(root.join(".sykli/receipts/rcpt_x.json"), "{}\n").unwrap();
        assert!(!uncommitted(&root).unwrap());
        fs::write(root.join(".sykli-other"), "x\n").unwrap();
        assert!(uncommitted(&root).unwrap());
    }

    #[test]
    fn a_worktree_that_changes_under_the_reviewer_records_nothing() {
        let root = branch_repo("review-moved");
        let edits = |_: Agent, _: &str, root: &Path| {
            fs::write(root.join("new.txt"), "edited meanwhile\n").unwrap();
            Ok("REVIEW: fine\n".to_string())
        };
        let error = review(&root, BASE, &[Agent::Codex], &edits).unwrap_err();
        assert!(error.contains("changed while"), "{error}");
        let tree = git(&root, &["rev-parse", "HEAD^{tree}"]).unwrap();
        assert_eq!(read_record(&root, &tree).unwrap(), None);
    }

    #[test]
    fn a_commit_already_in_the_base_has_nothing_to_review() {
        let root = branch_repo("review-landed");
        run_git(&root, &["update-ref", BASE, "HEAD"]);
        let never = |_: Agent, _: &str, _: &Path| -> Result<String, String> {
            panic!("nothing to review, so no reviewer starts")
        };
        assert_eq!(review(&root, BASE, &[Agent::Codex], &never).unwrap(), None);
        let tree = git(&root, &["rev-parse", "HEAD^{tree}"]).unwrap();
        assert_eq!(read_record(&root, &tree).unwrap(), None);
    }

    #[test]
    fn the_same_tree_on_another_base_is_reviewed_again() {
        let root = branch_repo("review-rebased");
        fs::write(root.join("more.txt"), "more\n").unwrap();
        run_git(&root, &["add", "-A"]);
        run_git(&root, &["commit", "-q", "-m", "more"]);
        let calls = Cell::new(0);
        let blocking = |_: Agent, _: &str, _: &Path| {
            calls.set(calls.get() + 1);
            Ok("REVIEW: blocking\n".to_string())
        };
        review(&root, BASE, &[Agent::Codex], &blocking)
            .unwrap()
            .unwrap();
        overrule(&root, BASE, "accepted").unwrap();

        // The base moves under the same tree: the change is now another diff.
        run_git(&root, &["update-ref", BASE, "HEAD~1"]);
        let error = overrule(&root, BASE, "accepted again").unwrap_err();
        assert!(error.contains("no review is kept"), "{error}");
        let again = review(&root, BASE, &[Agent::Codex], &blocking)
            .unwrap()
            .unwrap();
        assert_eq!(calls.get(), 2);
        assert_eq!(
            again.overruled, None,
            "an overrule does not follow the tree"
        );
        assert_eq!(exit_code(&again), 1);
    }

    #[test]
    fn the_default_branch_is_the_hooks_or_the_one_named() {
        let root = branch_repo("review-default-branch");
        // No refs/remotes/origin/HEAD: main, since only it is there.
        assert_eq!(default_branch(&root, "origin", None).unwrap(), "main");
        run_git(&root, &["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
        run_git(
            &root,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/trunk",
            ],
        );
        assert_eq!(default_branch(&root, "origin", None).unwrap(), "trunk");
        assert_eq!(
            default_branch(&root, "origin", Some("release")).unwrap(),
            "release"
        );
        assert!(default_branch(&root, "origin", Some("refs/heads/x..y")).is_err());
        for name in ["-origin", "--upload-pack=x", "a b", ""] {
            let error = default_branch(&root, name, None).unwrap_err();
            assert!(error.contains("is not a remote name"), "{name}: {error}");
        }
        // It names a ref that is gone, and both fallbacks exist: unknown.
        run_git(&root, &["update-ref", "-d", "refs/remotes/origin/trunk"]);
        run_git(&root, &["update-ref", "refs/remotes/origin/master", "HEAD"]);
        let error = default_branch(&root, "origin", None).unwrap_err();
        assert!(error.contains("--branch"), "{error}");
        // A remote this clone has nothing of: unknown, never a guess.
        assert!(default_branch(&root, "elsewhere", None).is_err());
    }

    #[test]
    fn the_codex_reviewer_is_sandboxed_and_loads_no_user_configuration() {
        let command = reviewer_command(Agent::Codex, Path::new("/tmp/last.txt"));
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "exec",
                "--sandbox",
                "read-only",
                "--ignore-user-config",
                "--ephemeral",
                "-o",
                "/tmp/last.txt",
                "-"
            ]
        );
    }

    #[test]
    fn the_claude_reviewer_has_reading_tools_only() {
        let command = reviewer_command(Agent::Claude, Path::new("/tmp/last.txt"));
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "-p",
                "--tools",
                "Read,Grep,Glob",
                "--strict-mcp-config",
                "--setting-sources",
                "",
                "--settings",
                r#"{"disableAllHooks":true}"#
            ]
        );
    }

    #[test]
    fn a_reviewer_whose_child_keeps_its_output_open_runs_into_the_time_limit() {
        let mut leaves_child = Command::new("sh");
        leaves_child.args(["-c", "sleep 4 & exit 0"]);
        let started = Instant::now();
        let error = converse(leaves_child, "sh", "", Duration::from_millis(500)).unwrap_err();
        assert!(error.contains("still holds"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn a_reviewer_that_ends_without_taking_its_whole_prompt_gave_no_review() {
        // It exits 0 and closes its output, but what it started holds stdin
        // and reads nothing: the verdict would be of a part of the change.
        let mut partial = Command::new("sh");
        partial.args(["-c", "sleep 4 <&0 >/dev/null 2>&1 & echo 'REVIEW: fine'"]);
        let prompt = "x".repeat(2_000_000);
        let error = converse(partial, "sh", &prompt, Duration::from_millis(500)).unwrap_err();
        assert!(error.contains("whole prompt"), "{error}");
    }

    #[test]
    fn a_reviewer_that_never_reads_its_prompt_runs_into_the_time_limit() {
        let mut stalled = Command::new("sh");
        stalled.args(["-c", "sleep 4"]);
        let prompt = "x".repeat(2_000_000);
        let started = Instant::now();
        let error = converse(stalled, "sh", &prompt, Duration::from_millis(300)).unwrap_err();
        assert!(error.contains("did not finish"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn a_reviewer_that_exits_non_zero_gave_no_review_whatever_it_printed() {
        let mut failing = Command::new("sh");
        failing.args(["-c", "cat >/dev/null; echo 'REVIEW: fine'; exit 3"]);
        let error = converse(failing, "sh", "prompt", Duration::from_secs(5)).unwrap_err();
        assert!(error.contains("before giving a review"), "{error}");
    }

    #[test]
    fn a_reviewer_that_takes_its_prompt_is_heard() {
        let mut echo = Command::new("sh");
        echo.args(["-c", "wc -c | tr -d ' '"]);
        let prompt = "x".repeat(2_000_000);
        let printed = converse(echo, "sh", &prompt, Duration::from_secs(10)).unwrap();
        assert_eq!(printed.trim(), "2000000");
    }

    #[test]
    fn the_reviewer_is_handed_the_commits_and_the_diff() {
        let root = branch_repo("review-prompt");
        let seen = RefCell::new(String::new());
        let fine = |_: Agent, prompt: &str, _: &Path| {
            *seen.borrow_mut() = prompt.to_string();
            Ok("REVIEW: fine\n".to_string())
        };
        review(&root, BASE, &[Agent::Claude], &fine)
            .unwrap()
            .unwrap();
        let prompt = seen.borrow();
        assert!(prompt.contains("+new"), "the diff: {prompt}");
        assert!(prompt.contains(" change\n"), "the commit subject: {prompt}");

        let long = "x".repeat(DIFF_LIMIT + 10);
        let cut = super::prompt("a", "b", "", &long);
        assert!(cut.contains("The diff is cut here"));
        assert!(cut.len() < long.len() + 3_000);
        assert!(!cut.contains(&long), "the whole diff was handed over");
        let whole = super::prompt("a", "b", "", "short");
        assert!(!whole.contains("The diff is cut here"));
    }

    #[test]
    fn the_reviewer_starts_without_the_asking_sessions_agent_markers() {
        for agent in Agent::ALL {
            let command = reviewer_command(agent, Path::new("/tmp/last.txt"));
            let removed: Vec<_> = command
                .get_envs()
                .filter(|(_, value)| value.is_none())
                .map(|(name, _)| name.to_string_lossy().into_owned())
                .collect();
            for marker in ["CLAUDECODE", "CODEX_THREAD_ID", "AI_AGENT"] {
                assert!(removed.contains(&marker.to_string()), "{removed:?}");
            }
        }
    }

    /// A repository whose `origin/main` is its first commit, with one commit
    /// on top of it.
    pub(crate) fn branch_repo(name: &str) -> Fixture {
        let root = Fixture::new(name);
        fs::create_dir_all(&root).unwrap();
        run_git(&root, &["init", "-q", "-b", "main"]);
        identify(&root);
        fs::write(root.join("README.md"), "# Demo\n").unwrap();
        run_git(&root, &["add", "-A"]);
        run_git(&root, &["commit", "-q", "-m", "initial"]);
        run_git(&root, &["update-ref", BASE, "HEAD"]);
        fs::write(root.join("new.txt"), "new\n").unwrap();
        run_git(&root, &["add", "-A"]);
        run_git(&root, &["commit", "-q", "-m", "change"]);
        root
    }

    /// A directory of a test's own, removed when the test ends.
    pub(crate) struct Fixture(PathBuf);

    impl Fixture {
        pub(crate) fn new(name: &str) -> Self {
            Fixture(std::env::temp_dir().join(format!("sykli-{name}-{}", crate::private_name())))
        }
    }

    impl std::ops::Deref for Fixture {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl AsRef<Path> for Fixture {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Who commits in a test repository, without the person's own settings
    /// deciding anything.
    pub(crate) fn identify(root: &Path) {
        run_git(root, &["config", "user.email", "widget@example.invalid"]);
        run_git(root, &["config", "user.name", "Widget Test"]);
        run_git(root, &["config", "commit.gpgsign", "false"]);
        run_git(root, &["config", "core.excludesFile", "/dev/null"]);
    }

    pub(crate) fn run_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
