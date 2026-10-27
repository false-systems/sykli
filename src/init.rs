//! `sykli init`: look at the repository, write the graph it implies.
//!
//! Detection, not a DSL: a Cargo, npm, or Go manifest yields the tasks its
//! ecosystem already has names for, with every input declared as a real
//! file in the tree so `plan --changed` can answer honestly. A Cargo task
//! declares every file it may read (`cargo`), not only its sources. Nothing here
//! guesses commands that do not exist — an npm script is a task only if the
//! manifest defines it.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sykli::{Contract, Task};

use crate::cargo_inputs;

/// What detection found, where it looked so "nothing" can be explained, and
/// anything it saw but could not turn into a task.
pub struct Detected {
    pub tasks: Vec<Task>,
    pub looked_for: Vec<&'static str>,
    pub notes: Vec<String>,
    /// What a Cargo build reads, from the listing given to `detect`.
    reads: Option<cargo_inputs::Reads>,
}

/// One ecosystem's contribution: a name to prefix with when several coexist.
struct Ecosystem {
    name: &'static str,
    tasks: Vec<Task>,
}

/// What `root`, the current directory, implies. `contract` is the name of
/// the contract being written when it lies at the top level of `root`, which
/// no task declares. `listed` is what git lists in the repository around it
/// (`Listed::of_repository`), None outside one.
pub fn detect(
    root: &Path,
    contract: Option<&str>,
    listed: Option<&crate::Listed>,
) -> Result<Detected, String> {
    let looked_for = vec!["Cargo.toml", "package.json", "go.mod"];
    let mut notes = Vec::new();
    let (cargo, reads) = match cargo(root, contract, listed, &mut notes)? {
        Some((ecosystem, reads)) => (Some(ecosystem), reads),
        None => (None, None),
    };
    let found: Vec<Ecosystem> = [cargo, npm(root, &mut notes), go(root)]
        .into_iter()
        .flatten()
        .collect();
    let prefix = found.len() > 1;
    let mut tasks = Vec::new();
    for ecosystem in found {
        for mut task in ecosystem.tasks {
            if prefix {
                task.name = format!("{}-{}", ecosystem.name, task.name);
                task.after = task
                    .after
                    .into_iter()
                    .map(|after| format!("{}-{}", ecosystem.name, after))
                    .collect();
            }
            tasks.push(task);
        }
    }
    Ok(Detected {
        tasks,
        looked_for,
        notes,
        reads,
    })
}

fn task(name: &str, run: &str, after: &[&str], inputs: BTreeSet<String>) -> Task {
    Task {
        name: name.into(),
        run: run.into(),
        workdir: None,
        env: Default::default(),
        after: after.iter().map(|after| after.to_string()).collect(),
        inputs: inputs.into_iter().collect(),
        outputs: Vec::new(),
        runtime: None,
        inherit: Vec::new(),
        contract_inputs: Vec::new(),
    }
}

/// A Cargo build reads more than its Rust sources: a vendored or path
/// dependency, a file a source includes, data its tests read (in `tests/`,
/// a top-level `fixtures/`, beside a pin), and nothing can tell all of them
/// apart from the files nothing reads. So every cargo task declares every
/// file git tracks here, documents included (a test may read a folder of
/// Markdown cases), and every symbolic link but one that leads out of the
/// repository, which cannot be bound; all it leaves out is `.sykli/`,
/// `target/`, and sykli's own contract and lock at the top level. An
/// untracked file is declared only when the completeness rule counts it
/// (Cargo's files by name, and every file the build reads otherwise:
/// `cargo_inputs::Reads`), so `run` accepts the contract `init` writes, and
/// an include whose path cannot be known refuses here as it does in `run`.
/// In a git repository, what the build reads is found from the whole
/// repository's listing, `listed`, the one the completeness rule reads, and
/// returned with the tasks, so `write` judges the contract from it too. A
/// file that is declared and read by nothing costs cache hits when it
/// changes; one that is read and not declared keeps a cached pass it does
/// not deserve.
fn cargo(
    root: &Path,
    contract: Option<&str>,
    listed: Option<&crate::Listed>,
    notes: &mut Vec<String>,
) -> Result<Option<(Ecosystem, Option<cargo_inputs::Reads>)>, String> {
    if !root.join("Cargo.toml").is_file() {
        return Ok(None);
    }
    // Every file listed, absolute, as `Reads` and the completeness rule spell
    // them, from where shown paths start; and the files under `root`, as
    // paths from it, tracked and untracked, with how each is spelled.
    let here = crate::absolute(root)?;
    let (files, top, tracked, untracked) = match listed {
        Some(listed) => {
            let under = |paths: &[PathBuf]| {
                paths
                    .iter()
                    .filter_map(|path| path.strip_prefix(&here).ok())
                    .map(|path| path.to_string_lossy().replace('\\', "/"))
                    .collect::<Vec<_>>()
            };
            (
                listed.files().into_iter().cloned().collect::<Vec<_>>(),
                listed.root.clone(),
                under(&listed.cached),
                under(&listed.others),
            )
        }
        None => {
            let tracked = walked(root);
            let files = tracked
                .iter()
                .map(|path| here.join(path))
                .collect::<Vec<_>>();
            (files, here.clone(), tracked, Vec::new())
        }
    };
    let spelled = |path: &str| here.join(path);
    let reads = cargo_inputs::Reads::of(&top, &files)?;
    if let Some(refusal) = reads.sources().refusal() {
        return Err(refusal);
    }
    let mut inputs = BTreeSet::new();
    let mut build = 0;
    let paths = tracked
        .iter()
        .map(|path| (path, true))
        .chain(untracked.iter().map(|path| (path, false)));
    for (path, tracked) in paths {
        if path.starts_with(".sykli/") {
            continue;
        }
        // Cargo's build directory, at the top level only: a module deeper
        // down named `target` is source.
        if path.starts_with("target/") {
            build += 1;
            continue;
        }
        let full = spelled(path);
        // By name from the repository's top level, as the rule asks.
        let relative = full.strip_prefix(&top).unwrap_or(&full);
        let required = cargo_inputs::by_name(relative) || reads.reads(&full);
        let wanted = required || (tracked && !metadata(path, contract));
        if wanted && declarable(root, path, notes) {
            inputs.insert(path.clone());
        }
    }
    // The file a declared link to a file in the repository leads to, which
    // `verify --commit` judges the link by.
    let all = tracked.iter().chain(&untracked).collect::<BTreeSet<_>>();
    let behind = inputs
        .iter()
        .filter_map(|path| file_behind(root, path))
        .filter(|file| all.contains(file))
        .collect::<Vec<_>>();
    inputs.extend(behind);
    if build > 0 {
        notes.push(format!(
            "{build} files under target/, Cargo's build directory, are not declared; ignore it in .gitignore"
        ));
    }
    let ecosystem = Ecosystem {
        name: "cargo",
        tasks: vec![
            task("fmt", "cargo fmt --check", &[], inputs.clone()),
            task(
                "clippy",
                "cargo clippy --workspace --all-targets -- -D warnings",
                &["fmt"],
                inputs.clone(),
            ),
            task("test", "cargo test --workspace", &["fmt"], inputs),
        ],
    };
    Ok(Some((ecosystem, listed.map(|_| reads))))
}

/// Outside a git repository nothing says what is ignored, so every file
/// under `root`, as paths from it, counts as tracked but hidden entries,
/// symbolic links, `node_modules` and Cargo's top-level `target`; Cargo's
/// configuration is hidden and counts too, and so does the
/// `.cargo-checksum.json` that `cargo vendor` writes beside a manifest.
fn walked(root: &Path) -> Vec<String> {
    let mut tracked = walk(root, Path::new(""), &[], &["node_modules"], &["target"]);
    let checksums = tracked
        .iter()
        .filter_map(|path| path.strip_suffix("Cargo.toml"))
        .filter(|directory| directory.is_empty() || directory.ends_with('/'))
        .map(|directory| format!("{directory}.cargo-checksum.json"))
        .filter(|checksum| root.join(checksum).is_file())
        .collect::<Vec<_>>();
    tracked.extend(checksums);
    for file in [".cargo/config.toml", ".cargo/config"] {
        if root.join(file).is_file() {
            tracked.push(file.into());
        }
    }
    tracked.sort();
    tracked
}

/// sykli's own contract and lock at the top level, and the contract being
/// written: evaluation metadata, never a task's input.
fn metadata(path: &str, contract: Option<&str>) -> bool {
    matches!(path, "sykli.json" | "sykli.lock") || contract == Some(path)
}

/// Whether `path` can be declared: a regular file, or a symbolic link (read
/// through to a file in the repository; recorded by its text when it leads
/// to a directory in the repository or to nothing there). A link that leads
/// out of the repository, to a file, a directory or nothing, cannot be,
/// since no commit holds what it leads to (with a note); nor can a file gone
/// from the work tree, a directory or a submodule.
fn declarable(root: &Path, path: &str, notes: &mut Vec<String>) -> bool {
    let file = root.join(path);
    let Ok(meta) = fs::symlink_metadata(&file) else {
        return false;
    };
    if meta.is_file() {
        return true;
    }
    if !meta.file_type().is_symlink() {
        return false;
    }
    if crate::leads_out_of_the_repository(&file) {
        notes.push(format!(
            "{path} is a symbolic link out of the repository; not declared, since no commit \
             holds what it leads to"
        ));
        return false;
    }
    true
}

/// The regular file inside `root` that the symbolic link at `path` leads
/// to, as a path from `root`; None for anything else.
fn file_behind(root: &Path, path: &str) -> Option<String> {
    let link = root.join(path);
    if !fs::symlink_metadata(&link).ok()?.file_type().is_symlink() {
        return None;
    }
    let file = fs::canonicalize(&link).ok().filter(|file| file.is_file())?;
    let root = fs::canonicalize(root).ok()?;
    let relative = file.strip_prefix(root).ok()?;
    Some(relative.to_string_lossy().replace('\\', "/"))
}

fn npm(root: &Path, notes: &mut Vec<String>) -> Option<Ecosystem> {
    let manifest = root.join("package.json");
    if !manifest.is_file() {
        return None;
    }
    let package: serde_json::Value = match fs::read(&manifest)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    {
        Some(package) => package,
        None => {
            notes.push("package.json is not valid JSON; skipped".into());
            return None;
        }
    };
    let scripts = package
        .get("scripts")
        .and_then(|scripts| scripts.as_object());
    let (runner, lockfile) = if root.join("pnpm-lock.yaml").is_file() {
        ("pnpm run", Some("pnpm-lock.yaml"))
    } else if root.join("yarn.lock").is_file() {
        ("yarn run", Some("yarn.lock"))
    } else if root.join("package-lock.json").is_file() {
        ("npm run", Some("package-lock.json"))
    } else {
        ("npm run", None)
    };
    let mut inputs = BTreeSet::from(["package.json".to_string()]);
    if let Some(lockfile) = lockfile {
        inputs.insert(lockfile.into());
    }
    for dir in ["src", "lib", "test", "tests"] {
        inputs.extend(walk(root, Path::new(dir), &[], &["node_modules"], &[]));
    }
    let tasks: Vec<Task> = ["lint", "test", "build"]
        .into_iter()
        .filter(|script| scripts.is_some_and(|scripts| scripts.contains_key(*script)))
        .map(|script| task(script, &format!("{runner} {script}"), &[], inputs.clone()))
        .collect();
    if tasks.is_empty() {
        notes.push("package.json defines none of the scripts lint, test, build; skipped".into());
        return None;
    }
    Some(Ecosystem { name: "npm", tasks })
}

fn go(root: &Path) -> Option<Ecosystem> {
    if !root.join("go.mod").is_file() {
        return None;
    }
    let mut inputs = BTreeSet::from(["go.mod".to_string()]);
    if root.join("go.sum").is_file() {
        inputs.insert("go.sum".into());
    }
    inputs.extend(walk(root, Path::new(""), &["go"], &["vendor"], &[]));
    Some(Ecosystem {
        name: "go",
        tasks: vec![
            task("vet", "go vet ./...", &[], inputs.clone()),
            task("test", "go test ./...", &["vet"], inputs),
        ],
    })
}

/// Every regular file under `dir` (relative to `root`), as sorted
/// repository-relative paths. Hidden entries, symlinks (a link out of the
/// tree or back into it is not a declared input), directories named in
/// `skip_anywhere` at any depth, and directories named in `skip_at_root`
/// directly under `root` are left out; with an empty `extensions` list every
/// file counts.
fn walk(
    root: &Path,
    dir: &Path,
    extensions: &[&str],
    skip_anywhere: &[&str],
    skip_at_root: &[&str],
) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![root.join(dir)];
    let root_children = |path: &Path| path.parent() == Some(root);
    while let Some(current) = pending.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if name.starts_with('.') || kind.is_symlink() || skip_anywhere.contains(&name.as_str())
            {
                continue;
            }
            if kind.is_dir() {
                if root_children(&path) && skip_at_root.contains(&name.as_str()) {
                    continue;
                }
                pending.push(path);
            } else if kind.is_file() {
                let keep = extensions.is_empty()
                    || path
                        .extension()
                        .is_some_and(|ext| extensions.iter().any(|want| ext == *want));
                if keep {
                    if let Ok(relative) = path.strip_prefix(root) {
                        found.push(relative.to_string_lossy().replace('\\', "/"));
                    }
                }
            }
        }
    }
    found.sort();
    found
}

pub fn render(contract: &Contract) -> Result<String, String> {
    let mut text = serde_json::to_string_pretty(contract).map_err(|error| error.to_string())?;
    text.push('\n');
    Ok(text)
}

/// The command: detect, refuse to clobber, write, lock.
pub fn run(
    path: &Path,
    force: bool,
    lock: bool,
    write_lock: &dyn Fn(&Path) -> Result<PathBuf, String>,
) -> ExitCode {
    // Declared inputs and task commands resolve against the directory sykli
    // runs in, not the contract's, so a contract written elsewhere would
    // declare paths that only work from there. Refuse rather than mislead.
    let here = std::env::current_dir()
        .ok()
        .and_then(|d| fs::canonicalize(d).ok());
    let root = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty() && *parent != Path::new("."))
        .filter(|parent| fs::canonicalize(parent).ok() != here)
        .map(Path::to_path_buf);
    if let Some(elsewhere) = root {
        eprintln!(
            "{} is in {}, not the current directory; inputs are relative to where sykli runs, so run `sykli init` from there",
            path.display(),
            elsewhere.display()
        );
        return ExitCode::from(1);
    }
    write(path, force, lock, write_lock, Place::Repository)
}

/// `sykli init --contract PATH` for a contract outside the repository: the
/// current directory is already the repository root, the tasks are inferred
/// from it as ever, and the contract and lock go to PATH's directory, made if
/// it is missing. Nothing in the repository is touched, so no `.sykli/` and
/// no `.gitignore` entry: whoever keeps their contract outside the
/// repository keeps sykli's state out of its history some other way, such as
/// a global git ignore.
pub fn run_outside(
    path: &Path,
    force: bool,
    lock: bool,
    write_lock: &dyn Fn(&Path) -> Result<PathBuf, String>,
) -> ExitCode {
    write(path, force, lock, write_lock, Place::Outside)
}

/// Where the contract goes, relative to the repository it is inferred from.
#[derive(PartialEq)]
enum Place {
    Repository,
    Outside,
}

/// Detect, refuse to clobber, write, lock.
fn write(
    path: &Path,
    force: bool,
    lock: bool,
    write_lock: &dyn Fn(&Path) -> Result<PathBuf, String>,
    place: Place,
) -> ExitCode {
    let root = PathBuf::from(".");
    let contract = match place {
        Place::Repository => path.file_name().and_then(|name| name.to_str()),
        Place::Outside => None,
    };
    // One listing of the whole repository, for what the tasks declare and
    // for the judgement of the contract they make.
    let listed = match crate::repo::repository() {
        Ok(_) => match crate::Listed::of_repository(true) {
            Ok(listed) => Some(listed),
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        },
        Err(_) => None,
    };
    let detected = match detect(&root, contract, listed.as_ref()) {
        Ok(detected) => detected,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
    };
    if detected.tasks.is_empty() {
        eprintln!(
            "nothing to declare: looked for {} in {}",
            detected.looked_for.join(", "),
            match place {
                Place::Repository => "the current directory",
                Place::Outside => "the repository root",
            }
        );
        for note in &detected.notes {
            eprintln!("  {note}");
        }
        return ExitCode::from(1);
    }
    for note in &detected.notes {
        eprintln!("note: {note}");
    }
    let task_count = detected.tasks.len();
    let input_count = detected
        .tasks
        .iter()
        .flat_map(|task| task.inputs.iter())
        .collect::<BTreeSet<_>>()
        .len();
    let written = Contract {
        schema: "sykli-contract.v1".into(),
        tasks: detected.tasks,
    };
    // What `validate` and `run` refuse before any task, judged from the same
    // listing: `init` writes no contract they would refuse. In a
    // subdirectory, the Cargo rule still reads the whole repository, and a
    // contract's paths cannot leave where sykli runs, so the refusal names
    // the top level to run in.
    if let Some(listed) = listed {
        let judged = crate::Inventory::given(&written, path, listed, detected.reads)
            .and_then(|inventory| crate::undeclared_refusal_of(&written, &inventory));
        match judged {
            Ok(None) => {}
            Ok(Some(refusal)) | Err(refusal) => {
                eprintln!(
                    "error: validate and run would refuse the contract init would write here, so \
                     it writes none: {refusal}"
                );
                return ExitCode::from(1);
            }
        }
    }
    let text = match render(&written) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
    };
    if path.exists() && !force {
        eprintln!(
            "{} exists; pass --force to overwrite it. This would have been written:",
            path.display()
        );
        print!("{text}");
        return ExitCode::from(2);
    }
    if place == Place::Outside {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        if let Err(error) = fs::create_dir_all(parent) {
            eprintln!("error: cannot create {}: {error}", parent.display());
            return ExitCode::from(1);
        }
    }
    if let Err(error) = fs::write(path, &text) {
        eprintln!("error: cannot write {}: {error}", path.display());
        return ExitCode::from(1);
    }
    if place == Place::Repository {
        keep_state_out_of_git();
    }
    if lock {
        match write_lock(path) {
            Ok(lock_path) => println!(
                "wrote {} ({task_count} tasks, {input_count} inputs) and {}",
                path.display(),
                lock_path.display()
            ),
            Err(error) => {
                eprintln!("wrote {} but could not lock it: {error}", path.display());
                return ExitCode::from(1);
            }
        }
    } else {
        println!(
            "wrote {} ({task_count} tasks, {input_count} inputs)",
            path.display()
        );
    }
    ExitCode::SUCCESS
}

fn keep_state_out_of_git() {
    // Receipts and caches live under .sykli/, which never counts as part of
    // the tree a receipt describes; make the directory and keep it out of git
    // so `sykli run --json > .sykli/receipt.json` verifies clean.
    let _ = fs::create_dir_all(".sykli");
    let ignore = fs::read_to_string(".gitignore").unwrap_or_default();
    if !ignore
        .lines()
        .any(|line| matches!(line.trim(), ".sykli" | ".sykli/" | "/.sykli" | "/.sykli/"))
    {
        let mut ignore = ignore;
        if !ignore.is_empty() && !ignore.ends_with('\n') {
            ignore.push('\n');
        }
        ignore.push_str(".sykli/\n");
        if let Err(error) = fs::write(".gitignore", ignore) {
            eprintln!("note: could not add .sykli/ to .gitignore: {error}");
        }
    }
}
