//! `sykli init`: look at the repository, write the graph it implies.
//!
//! Detection, not a DSL: a Cargo, npm, or Go manifest yields the tasks its
//! ecosystem already has names for, with every input declared as a real
//! file in the tree so `plan --changed` can answer honestly. Nothing here
//! guesses commands that do not exist — an npm script is a task only if the
//! manifest defines it.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sykli::{Contract, Task};

/// What detection found, and where it looked so "nothing" can be explained.
pub struct Detected {
    pub tasks: Vec<Task>,
    pub looked_for: Vec<&'static str>,
}

/// One ecosystem's contribution: a name to prefix with when several coexist.
struct Ecosystem {
    name: &'static str,
    tasks: Vec<Task>,
}

pub fn detect(root: &Path) -> Detected {
    let looked_for = vec!["Cargo.toml", "package.json", "go.mod"];
    let found: Vec<Ecosystem> = [cargo(root), npm(root), go(root)]
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
    Detected { tasks, looked_for }
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
    }
}

fn cargo(root: &Path) -> Option<Ecosystem> {
    let manifest = root.join("Cargo.toml");
    if !manifest.is_file() {
        return None;
    }
    let mut inputs = BTreeSet::from(["Cargo.toml".to_string()]);
    if root.join("Cargo.lock").is_file() {
        inputs.insert("Cargo.lock".into());
    }
    for dir in ["src", "tests", "benches", "examples"] {
        inputs.extend(walk(root, Path::new(dir), &["rs"], &["target"]));
    }
    for member in workspace_members(&fs::read_to_string(&manifest).unwrap_or_default()) {
        let member_root = root.join(&member);
        if member_root.join("Cargo.toml").is_file() {
            inputs.insert(format!("{member}/Cargo.toml"));
            for dir in ["src", "tests"] {
                inputs.extend(walk(
                    root,
                    &Path::new(&member).join(dir),
                    &["rs"],
                    &["target"],
                ));
            }
        }
    }
    Some(Ecosystem {
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
    })
}

/// The `members = [...]` list of a `[workspace]` table, read line by line.
/// Entries with globs are skipped: the contract declares files, not patterns.
fn workspace_members(manifest: &str) -> Vec<String> {
    let mut members = Vec::new();
    let mut in_workspace = false;
    let mut in_members = false;
    for raw in manifest.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            in_workspace = line == "[workspace]";
            in_members = false;
            continue;
        }
        if !in_workspace {
            continue;
        }
        let list = if let Some(rest) = line.strip_prefix("members") {
            let rest = rest.trim_start();
            match rest.strip_prefix('=') {
                Some(rest) => {
                    in_members = true;
                    rest
                }
                None => continue,
            }
        } else if in_members {
            line
        } else {
            continue;
        };
        for entry in list
            .split([',', '[', ']'])
            .map(|entry| entry.trim().trim_matches('"').trim_matches('\''))
            .filter(|entry| !entry.is_empty())
        {
            if !entry.contains('*') {
                members.push(entry.trim_end_matches('/').to_string());
            }
        }
        if list.contains(']') {
            in_members = false;
        }
    }
    members
}

fn npm(root: &Path) -> Option<Ecosystem> {
    let manifest = root.join("package.json");
    if !manifest.is_file() {
        return None;
    }
    let package: serde_json::Value = serde_json::from_slice(&fs::read(&manifest).ok()?).ok()?;
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
        inputs.extend(walk(root, Path::new(dir), &[], &["node_modules"]));
    }
    let tasks: Vec<Task> = ["lint", "test", "build"]
        .into_iter()
        .filter(|script| scripts.is_some_and(|scripts| scripts.contains_key(*script)))
        .map(|script| task(script, &format!("{runner} {script}"), &[], inputs.clone()))
        .collect();
    if tasks.is_empty() {
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
    inputs.extend(walk(root, Path::new(""), &["go"], &["vendor"]));
    Some(Ecosystem {
        name: "go",
        tasks: vec![
            task("vet", "go vet ./...", &[], inputs.clone()),
            task("test", "go test ./...", &["vet"], inputs),
        ],
    })
}

/// Every regular file under `dir` (relative to `root`), as sorted
/// repository-relative paths. Hidden entries and `skip` directories are
/// left out; with an empty `extensions` list every file counts.
fn walk(root: &Path, dir: &Path, extensions: &[&str], skip: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![root.join(dir)];
    while let Some(current) = pending.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || skip.contains(&name.as_str()) {
                continue;
            }
            if path.is_dir() {
                pending.push(path);
            } else if path.is_file() {
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
    let root = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let detected = detect(&root);
    if detected.tasks.is_empty() {
        eprintln!(
            "nothing to declare: looked for {} in {}",
            detected.looked_for.join(", "),
            root.display()
        );
        return ExitCode::from(1);
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
    if let Err(error) = fs::write(path, &text) {
        eprintln!("error: cannot write {}: {error}", path.display());
        return ExitCode::from(1);
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
