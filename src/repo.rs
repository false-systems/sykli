//! The git repository around sykli, found out once per process.
//!
//! A hook runs on every push, so every git process counts. One
//! `git rev-parse` answers where the repository is, and the answer is kept
//! for the rest of the process; a caller that already knows states it
//! (`--repo-root` and `--git-common-dir`), and then no git runs to find the
//! repository at all. Questions about objects go to one `git cat-file
//! --batch` for all the names a command needs.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// Where the repository is.
pub(crate) struct Repository {
    /// The worktree's top level, as git (or the caller) spells it.
    pub(crate) root: PathBuf,
    /// The common git directory: the refs and objects every worktree shares.
    pub(crate) common_dir: PathBuf,
    /// `root` with symbolic links resolved, for comparing locations.
    resolved: OnceLock<Option<PathBuf>>,
}

static REPOSITORY: OnceLock<Result<Repository, String>> = OnceLock::new();

/// Take the caller's answers instead of asking git. Both must be absolute
/// directories, and the files on disk must bear them out: the root's `.git`
/// must lead to the common directory. A root that is only a subdirectory
/// would leave changes outside it out of every snapshot, so a receipt could
/// pass for content that never ran.
pub(crate) fn give(root: PathBuf, common_dir: PathBuf) -> Result<(), String> {
    for (flag, path) in [("--repo-root", &root), ("--git-common-dir", &common_dir)] {
        if !path.is_absolute() || !path.is_dir() {
            return Err(format!(
                "{flag} {} is not an absolute path to a directory",
                path.display()
            ));
        }
    }
    if !leads_to(&root, &common_dir) {
        return Err(format!(
            "--repo-root {} is not the top level of the repository whose common git directory \
             is --git-common-dir {}: its .git does not lead there; pass both as `git rev-parse \
             --path-format=absolute --show-toplevel --git-common-dir` prints them",
            root.display(),
            common_dir.display()
        ));
    }
    let _ = REPOSITORY.set(Ok(Repository::at(root, common_dir)));
    Ok(())
}

/// Whether `root/.git` leads to `common_dir`, read from files as git reads
/// them: the directory itself (a main checkout), or a `gitdir: PATH` file (a
/// linked worktree, a submodule) whose directory is the common one, or names
/// it in a `commondir` file. Relative paths are relative to the file that
/// holds them. Compared where both are on disk, so `/tmp` and
/// `/private/tmp`, or a trailing slash, spell the same directory.
fn leads_to(root: &Path, common_dir: &Path) -> bool {
    let Ok(common) = std::fs::canonicalize(common_dir) else {
        return false;
    };
    let dot_git = root.join(".git");
    let target = if dot_git.is_dir() {
        dot_git
    } else {
        let Some(gitdir) = std::fs::read_to_string(&dot_git).ok().and_then(|text| {
            text.strip_prefix("gitdir: ")
                .map(|rest| root.join(rest.trim_end()))
        }) else {
            return false;
        };
        match std::fs::read_to_string(gitdir.join("commondir")) {
            Ok(text) => gitdir.join(text.trim_end()),
            Err(_) => gitdir,
        }
    };
    std::fs::canonicalize(target).is_ok_and(|target| target == common)
}

/// The repository around the current directory, found once per process,
/// or git's reason why there is none.
pub(crate) fn repository() -> Result<&'static Repository, String> {
    REPOSITORY
        .get_or_init(discover)
        .as_ref()
        .map_err(Clone::clone)
}

fn discover() -> Result<Repository, String> {
    let output = Command::new("git")
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-common-dir",
        ])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().into());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();
    match (lines.next(), lines.next()) {
        (Some(root), Some(common_dir)) => Ok(Repository::at(
            PathBuf::from(root),
            PathBuf::from(common_dir),
        )),
        _ => Err(format!("unexpected answer from git rev-parse: {text:?}")),
    }
}

impl Repository {
    /// A repository known without asking git, as `--repo-root` and
    /// `--git-common-dir` give it.
    pub(crate) fn at(root: PathBuf, common_dir: PathBuf) -> Self {
        Repository {
            root,
            common_dir,
            resolved: OnceLock::new(),
        }
    }

    /// `root` with symbolic links resolved, when it can be.
    pub(crate) fn resolved(&self) -> Option<&Path> {
        self.resolved
            .get_or_init(|| std::fs::canonicalize(&self.root).ok())
            .as_deref()
    }

    /// The top level spelled from the current directory, as declared paths
    /// are compared: Git's own spelling can expand Windows short names. The
    /// root as given when the current directory is not inside it.
    pub(crate) fn root_here(&self) -> Result<PathBuf, String> {
        let here = std::env::current_dir().map_err(|error| error.to_string())?;
        let below = std::fs::canonicalize(&here)
            .ok()
            .zip(self.resolved())
            .and_then(|(resolved, root)| {
                resolved
                    .strip_prefix(root)
                    .ok()
                    .map(|below| below.components().count())
            });
        Ok(match below {
            Some(depth) => {
                let mut root = here;
                for _ in 0..depth {
                    root.pop();
                }
                root
            }
            None => self.root.clone(),
        })
    }

    /// Where objects are written: `GIT_OBJECT_DIRECTORY` when set, as git
    /// honours it, else the common directory's `objects`.
    pub(crate) fn object_directory(&self) -> PathBuf {
        match std::env::var_os("GIT_OBJECT_DIRECTORY") {
            Some(directory) => {
                let directory = PathBuf::from(directory);
                if directory.is_absolute() {
                    directory
                } else {
                    std::env::current_dir()
                        .map(|here| here.join(&directory))
                        .unwrap_or(directory)
                }
            }
            None => self.common_dir.join("objects"),
        }
    }
}

/// An object as `git cat-file --batch` reports it.
pub(crate) struct Object {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) bytes: Vec<u8>,
}

impl Repository {
    /// Ask one `git cat-file --batch`, run at the top level, about every
    /// name in order: each object's id and contents, or None when the name
    /// names no object (missing or ambiguous). Names are revision expressions
    /// such as `REV^{tree}` or `REV:path`, paths from the top level; read from
    /// stdin, they can never be options.
    pub(crate) fn cat(&self, names: &[String]) -> Result<Vec<Option<Object>>, String> {
        cat(&self.root, names)
    }
}

fn cat(root: &Path, names: &[String]) -> Result<Vec<Option<Object>>, String> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(name) = names.iter().find(|name| name.contains(['\n', '\r'])) {
        return Err(format!("{name:?} cannot be asked of git cat-file"));
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let mut stdin = child.stdin.take().ok_or("git cat-file has no stdin")?;
    let mut input = names.join("\n");
    input.push('\n');
    // Written from another thread, so a long answer can never block it.
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    writer
        .join()
        .map_err(|_| "writing to git cat-file panicked".to_string())?
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().into());
    }
    let mut rest = &output.stdout[..];
    let mut answers = Vec::with_capacity(names.len());
    for name in names {
        let end = rest
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or("git cat-file ended early")?;
        let header = String::from_utf8_lossy(&rest[..end]).into_owned();
        rest = &rest[end + 1..];
        if header == format!("{name} missing") || header == format!("{name} ambiguous") {
            answers.push(None);
            continue;
        }
        let fields = header.split(' ').collect::<Vec<_>>();
        let [id, kind, size] = fields[..] else {
            return Err(format!("unexpected git cat-file answer {header:?}"));
        };
        let size: usize = size
            .parse()
            .map_err(|_| format!("unexpected git cat-file answer {header:?}"))?;
        if rest.len() < size + 1 {
            return Err("git cat-file ended early".into());
        }
        answers.push(Some(Object {
            id: id.to_string(),
            kind: kind.to_string(),
            bytes: rest[..size].to_vec(),
        }));
        rest = &rest[size + 1..];
    }
    Ok(answers)
}

/// Whether `name` is a branch name a push can update, by git's documented
/// rules for ref names (git-check-ref-format) applied to `refs/heads/NAME`,
/// plus the two `git branch` refuses: `HEAD` and a leading `-`. No git runs.
pub(crate) fn branch_name(name: &str) -> bool {
    if name.is_empty() || name == "HEAD" || name.starts_with('-') || name.starts_with("refs/") {
        return false;
    }
    if name == "@"
        || name.contains("..")
        || name.contains("@{")
        || name.contains("//")
        || name.starts_with('/')
        || name.ends_with('/')
        || name.ends_with('.')
    {
        return false;
    }
    let forbidden = |c: char| {
        c.is_ascii_control() || matches!(c, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\')
    };
    if name.chars().any(forbidden) {
        return false;
    }
    name.split('/')
        .all(|component| !component.starts_with('.') && !component.ends_with(".lock"))
}

#[cfg(test)]
mod tests {
    use super::branch_name;

    #[test]
    fn branch_names_follow_gits_rules() {
        for good in [
            "main",
            "release/2.0",
            "feature/a-b_c",
            "v1",
            "ünïcode",
            "a.b",
        ] {
            assert!(branch_name(good), "{good}");
        }
        for bad in [
            "",
            "HEAD",
            "-dash",
            "refs/heads/main",
            "two words",
            "main..x",
            "a//b",
            "/a",
            "a/",
            "a.",
            ".hidden",
            "a/.hidden",
            "a.lock",
            "a/b.lock",
            "@",
            "a@{b",
            "a~b",
            "a^b",
            "a:b",
            "a?b",
            "a*b",
            "a[b",
            "a\\b",
            "tab\there",
            "del\u{7f}",
        ] {
            assert!(!branch_name(bad), "{bad:?}");
        }
    }
}
