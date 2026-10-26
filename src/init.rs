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
}

/// One ecosystem's contribution: a name to prefix with when several coexist.
struct Ecosystem {
    name: &'static str,
    tasks: Vec<Task>,
}

/// What `root` implies. `contract` is the name of the contract being written
/// when it lies at the top level of `root`, which no task declares.
pub fn detect(root: &Path, contract: Option<&str>) -> Result<Detected, String> {
    let looked_for = vec!["Cargo.toml", "package.json", "go.mod"];
    let mut notes = Vec::new();
    let cargo = cargo(root, contract, &mut notes)?;
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
    }
}

/// A Cargo build reads more than its Rust sources: a vendored or path
/// dependency, a file a source includes, data its tests read (in `tests/`,
/// a top-level `fixtures/`, beside a pin, in a workflow file), and nothing
/// can tell all of them apart from the files nothing reads. So every cargo
/// task declares every file git lists here (`listing`), except what
/// `left_out` names at the top level, and except untracked files that the
/// completeness rule does not count. A file that is declared and read by
/// nothing costs cache hits when it changes; one that is read and not
/// declared keeps a cached pass it does not deserve. Every file the
/// completeness rule requires (`cargo_inputs`) is declared, so `run`
/// accepts the contract `init` writes.
fn cargo(
    root: &Path,
    contract: Option<&str>,
    notes: &mut Vec<String>,
) -> Result<Option<Ecosystem>, String> {
    if !root.join("Cargo.toml").is_file() {
        return Ok(None);
    }
    let (tracked, untracked) = listing(root)?;
    let source = |path: &str| Path::new(path).extension().is_some_and(|ext| ext == "rs");
    let mut included = BTreeSet::new();
    for path in tracked.iter().chain(&untracked) {
        let file = root.join(path);
        if source(path) && file.is_file() {
            let text = fs::read(&file).map_err(|error| format!("cannot read {path}: {error}"))?;
            included.extend(
                cargo_inputs::included(Path::new(path), &String::from_utf8_lossy(&text))
                    .into_iter()
                    .map(|named| named.to_string_lossy().replace('\\', "/")),
            );
        }
    }
    let mut inputs = BTreeSet::new();
    let mut build = 0;
    let listed = tracked
        .iter()
        .map(|path| (path, true))
        .chain(untracked.iter().map(|path| (path, false)));
    for (path, tracked) in listed {
        if path.starts_with(".sykli/") {
            continue;
        }
        // Cargo's build directory, at the top level only: a module deeper
        // down named `target` is source.
        if path.starts_with("target/") {
            build += 1;
            continue;
        }
        let wanted = included.contains(path)
            || if tracked {
                !left_out(path, contract)
            } else {
                cargo_inputs::by_name(Path::new(path))
            };
        if wanted && declarable(root, path, notes) {
            inputs.insert(path.clone());
        }
    }
    if build > 0 {
        notes.push(format!(
            "{build} files under target/, Cargo's build directory, are not declared; ignore it in .gitignore"
        ));
    }
    Ok(Some(Ecosystem {
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
    }))
}

/// The files under `root`, as paths from it, split into tracked and
/// untracked: git's index and the untracked files it does not ignore, from
/// the `git ls-files` the completeness rule reads (`repo::files`). Outside a
/// git repository nothing says what is ignored, so every file counts as
/// tracked but hidden entries, symbolic links, `node_modules` and Cargo's
/// top-level `target`; Cargo's configuration is hidden and counts too.
fn listing(root: &Path) -> Result<(Vec<String>, Vec<String>), String> {
    if crate::repo::repository().is_err() {
        let mut tracked = walk(root, Path::new(""), &[], &["node_modules"], &["target"]);
        for file in [".cargo/config.toml", ".cargo/config"] {
            if root.join(file).is_file() {
                tracked.push(file.into());
            }
        }
        return Ok((tracked, Vec::new()));
    }
    let here = std::env::current_dir()
        .map_err(|error| error.to_string())?
        .join(root);
    let files = crate::repo::files(&here, true)?;
    Ok((files.cached, files.others))
}

/// The files at the top level that no build or test is expected to read,
/// left out so that editing them keeps cached passes: prose (`*.md`),
/// licences and notices, and sykli's own contracts and lock. Only the top
/// level, which the undeclared-sibling check exempts, so leaving them out
/// never refuses a run; a file a source includes (a crate documented by its
/// README) is declared all the same. Nothing else is left out: a test that
/// checks a workflow file or a document reads it.
fn left_out(path: &str, contract: Option<&str>) -> bool {
    if path.contains('/') {
        return false;
    }
    let upper = path.to_ascii_uppercase();
    upper.ends_with(".MD")
        || ["LICENSE", "LICENCE", "COPYING", "NOTICE"]
            .iter()
            .any(|prefix| upper.starts_with(prefix))
        || matches!(path, "sykli.json" | "sykli.lock")
        || contract == Some(path)
}

/// Whether `path` can be declared: a regular file, or a symbolic link to
/// one inside `root`, since a run hashes what the link leads to. A link out
/// of `root` or to no file is not (with a note); a file gone from the work
/// tree, a directory or a submodule is not either.
fn declarable(root: &Path, path: &str, notes: &mut Vec<String>) -> bool {
    let file = root.join(path);
    match fs::symlink_metadata(&file) {
        Ok(meta) if meta.is_file() => true,
        Ok(meta) if meta.file_type().is_symlink() => {
            let inside = file.is_file()
                && fs::canonicalize(&file)
                    .ok()
                    .zip(fs::canonicalize(root).ok())
                    .is_some_and(|(target, root)| target.starts_with(root));
            if !inside {
                notes.push(format!(
                    "{path} is a symbolic link to no file in the repository; not declared"
                ));
            }
            inside
        }
        _ => false,
    }
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

pub fn render(tasks: Vec<Task>) -> Result<String, String> {
    let contract = Contract {
        schema: "sykli-contract.v1".into(),
        tasks,
    };
    let mut text = serde_json::to_string_pretty(&contract).map_err(|error| error.to_string())?;
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
    let detected = match detect(&root, contract) {
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
    let text = match render(detected.tasks) {
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
