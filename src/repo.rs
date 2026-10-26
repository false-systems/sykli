//! The git repository around sykli, found out once per process.
//!
//! A hook runs on every push, so every git process counts. One
//! `git rev-parse` answers where the repository is, and the answer is kept
//! for the rest of the process; a caller that already knows states it
//! (`--repo-root` and `--git-common-dir`), and then no git runs to find the
//! repository at all. Questions about objects go to one `git cat-file
//! --batch` for all the names a command needs.

use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

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

/// Whether the caller gave the repository (`give`).
static GIVEN: AtomicBool = AtomicBool::new(false);

/// Settings in the environment that point git at a repository or a work
/// tree.
const LOCATION_SETTINGS: [&str; 3] = ["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR"];

/// A git command as sykli starts every one. When the caller gave the
/// repository, `give` checked the pair from files, so nothing in the
/// environment may swap in another repository or work tree: the settings
/// that point git elsewhere are dropped and the command starts at the given
/// root, where git finds the checked repository through the root's own
/// `.git` (a linked worktree's `gitdir:` file included). `GIT_INDEX_FILE`
/// stays, since the pre-commit hook judges the index git is about to commit.
/// Without the facts, git finds the repository as it always did,
/// environment included.
pub(crate) fn git_command() -> Command {
    let mut command = Command::new("git");
    if GIVEN.load(Ordering::SeqCst) {
        for setting in LOCATION_SETTINGS {
            command.env_remove(setting);
        }
        if let Some(Ok(repository)) = REPOSITORY.get() {
            command.current_dir(&repository.root);
        }
    }
    command
}

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
    GIVEN.store(true, Ordering::SeqCst);
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
    /// The contents; empty for a blob larger than the bound it was asked
    /// with (`Session::ask_bounded`), which is hashed as it is read instead.
    pub(crate) bytes: Vec<u8>,
    /// For a blob, the SHA-256 of its contents in hex, whether or not they
    /// were kept.
    pub(crate) sha256: Option<String>,
}

impl Repository {
    /// One `git cat-file --batch` process that can be asked in rounds, where
    /// one round's answers decide the next round's names (a contract read
    /// from a commit names the inputs to read next). It starts on the first
    /// question, so a session never asked starts no process. `env` is set for
    /// it, as for a snapshot whose trees are written to a temporary object
    /// directory: objects written there after it starts are found.
    pub(crate) fn session(&self, env: Vec<(&'static str, std::ffi::OsString)>) -> Session {
        Session {
            root: self.root.clone(),
            env,
            process: None,
        }
    }
}

/// See `Repository::session`.
pub(crate) struct Session {
    root: PathBuf,
    env: Vec<(&'static str, std::ffi::OsString)>,
    process: Option<(Child, Option<ChildStdin>, BufReader<ChildStdout>)>,
}

impl Session {
    /// Ask about every name in order: each object's id, kind and contents,
    /// or None when the name names no object (missing or ambiguous). Names
    /// are revision expressions such as `REV^{tree}` or `REV:path`, paths
    /// from the top level; read from stdin, they can never be options.
    pub(crate) fn ask(&mut self, names: &[String]) -> Result<Vec<Option<Object>>, String> {
        self.ask_bounded(names, usize::MAX)
    }

    /// `ask`, keeping the contents of a blob only up to `keep` bytes: a
    /// larger blob is hashed as it is read and its contents are not held,
    /// so memory stays bounded whatever the size of what is asked.
    pub(crate) fn ask_bounded(
        &mut self,
        names: &[String],
        keep: usize,
    ) -> Result<Vec<Option<Object>>, String> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(name) = names.iter().find(|name| name.contains(['\n', '\r'])) {
            return Err(format!("{name:?} cannot be asked of git cat-file"));
        }
        if self.process.is_none() {
            let mut command = git_command();
            command
                .arg("-C")
                .arg(&self.root)
                .args(["cat-file", "--batch"])
                .envs(self.env.iter().map(|(name, value)| (*name, value)));
            let mut child = command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|error| error.to_string())?;
            let stdin = child.stdin.take().ok_or("git cat-file has no stdin")?;
            let stdout = BufReader::new(child.stdout.take().ok_or("git cat-file has no stdout")?);
            self.process = Some((child, Some(stdin), stdout));
        }
        let Some((_, stdin, stdout)) = self.process.as_mut() else {
            return Err("git cat-file is not running".into());
        };
        let mut writer = stdin.take().ok_or("git cat-file's input is closed")?;
        let mut input = names.join("\n");
        input.push('\n');
        // Written from another thread, so a long answer can never block it;
        // cat-file answers each name as it reads it.
        let writing = std::thread::spawn(move || {
            writer.write_all(input.as_bytes())?;
            writer.flush()?;
            Ok::<_, std::io::Error>(writer)
        });
        let answers = read_answers(stdout, names, keep);
        let written = writing
            .join()
            .map_err(|_| "writing to git cat-file panicked".to_string())?;
        match (answers, written) {
            (Ok(answers), Ok(writer)) => {
                if let Some((_, stdin, _)) = self.process.as_mut() {
                    *stdin = Some(writer);
                }
                Ok(answers)
            }
            (Ok(_), Err(error)) => Err(self.failure(error.to_string())),
            // Close the input first: cat-file waits on it, so waiting for
            // cat-file with it open would never end.
            (Err(error), written) => {
                drop(written);
                Err(self.failure(error))
            }
        }
    }

    /// Close the process and check it ended well.
    pub(crate) fn finish(mut self) -> Result<(), String> {
        let Some((mut child, stdin, _)) = self.process.take() else {
            return Ok(());
        };
        drop(stdin);
        let status = child.wait().map_err(|error| error.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(stderr_of(&mut child))
        }
    }

    /// Git's own reason when it stopped answering, else `error`.
    fn failure(&mut self, error: String) -> String {
        let Some((mut child, stdin, _)) = self.process.take() else {
            return error;
        };
        drop(stdin);
        let _ = child.wait();
        let reason = stderr_of(&mut child);
        if reason.is_empty() { error } else { reason }
    }
}

fn stderr_of(child: &mut Child) -> String {
    let mut reason = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut reason);
    }
    reason.trim().to_string()
}

/// One answer per name: `<id> <type> <size>` and the contents, or
/// `<name> missing` (or `ambiguous`). A blob's contents are hashed as they
/// are read, in pieces, and kept only when they are at most `keep` bytes.
fn read_answers(
    stdout: &mut BufReader<ChildStdout>,
    names: &[String],
    keep: usize,
) -> Result<Vec<Option<Object>>, String> {
    let ended = |_| "git cat-file ended early".to_string();
    let mut answers = Vec::with_capacity(names.len());
    for name in names {
        let mut line = Vec::new();
        let read = stdout
            .read_until(b'\n', &mut line)
            .map_err(|error| error.to_string())?;
        if read == 0 || line.last() != Some(&b'\n') {
            return Err("git cat-file ended early".into());
        }
        line.pop();
        let header = String::from_utf8_lossy(&line).into_owned();
        if header == format!("{name} missing") || header == format!("{name} ambiguous") {
            answers.push(None);
            continue;
        }
        let fields = header.split(' ').collect::<Vec<_>>();
        // A submodule's commit is named, never shown: `<id> submodule`.
        if let [id, "submodule"] = fields[..] {
            answers.push(Some(Object {
                id: id.to_string(),
                kind: "submodule".into(),
                bytes: Vec::new(),
                sha256: None,
            }));
            continue;
        }
        let [id, kind, size] = fields[..] else {
            return Err(format!("unexpected git cat-file answer {header:?}"));
        };
        let size: usize = size
            .parse()
            .map_err(|_| format!("unexpected git cat-file answer {header:?}"))?;
        let blob = kind == "blob";
        let kept = !blob || size <= keep;
        let mut bytes = Vec::with_capacity(if kept { size } else { 0 });
        let mut hasher = Sha256::new();
        let mut piece = [0; 64 * 1024];
        let mut left = size;
        while left > 0 {
            let part = &mut piece[..left.min(64 * 1024)];
            stdout.read_exact(part).map_err(ended)?;
            if blob {
                hasher.update(&*part);
            }
            if kept {
                bytes.extend_from_slice(part);
            }
            left -= part.len();
        }
        let mut newline = [0];
        stdout.read_exact(&mut newline).map_err(ended)?;
        answers.push(Some(Object {
            id: id.to_string(),
            kind: kind.to_string(),
            bytes,
            sha256: blob.then(|| format!("{:x}", hasher.finalize())),
        }));
    }
    Ok(answers)
}

/// The entries of a tree object: each name with its entry as `MODE ID`,
/// the mode as git writes it (`100644`, `100755`, `120000`, `160000`,
/// `40000`) and the object id in hex. `id_bytes` is the length of an object
/// id in this repository (20 for SHA-1, 32 for SHA-256).
pub(crate) fn tree_entries(tree: &[u8], id_bytes: usize) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    let mut rest = tree;
    while let Some(space) = rest.iter().position(|byte| *byte == b' ') {
        let mode = String::from_utf8_lossy(&rest[..space]).into_owned();
        rest = &rest[space + 1..];
        let Some(end) = rest.iter().position(|byte| *byte == 0) else {
            break;
        };
        let name = String::from_utf8_lossy(&rest[..end]).into_owned();
        let Some(id) = rest.get(end + 1..end + 1 + id_bytes) else {
            break;
        };
        let id = id
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        rest = &rest[end + 1 + id_bytes..];
        entries.push((name, format!("{mode} {id}")));
    }
    entries
}

/// What one `git ls-files` lists under `directory`, as paths from it: the
/// files in the index and, with `others`, the untracked files git does not
/// ignore. `--stage` tells the two apart: it prints an index entry with its
/// mode, object id and stage, and an untracked file bare.
pub(crate) fn files(directory: &Path, others: bool) -> Result<Files, String> {
    let mut args = vec!["ls-files", "--stage", "-z"];
    if others {
        args.extend(["--others", "--exclude-standard"]);
    }
    let output = git_command()
        .arg("-C")
        .arg(directory)
        .args(&args)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into());
    }
    let mut files = Files::default();
    for entry in output.stdout.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let entry = std::str::from_utf8(entry)
            .map_err(|_| "input coverage requires UTF-8 Git paths".to_string())?;
        // `<mode> <object> <stage>\t<path>` for the index.
        let staged = entry.split_once('\t').filter(|(stage, _)| {
            let fields = stage.split(' ').collect::<Vec<_>>();
            fields.len() == 3
                && fields[0].len() == 6
                && fields[0].bytes().all(|byte| matches!(byte, b'0'..=b'7'))
                && fields[1].bytes().all(|byte| byte.is_ascii_hexdigit())
                && matches!(fields[2], "0" | "1" | "2" | "3")
        });
        match staged {
            Some((_, path)) => files.cached.push(path.to_string()),
            None => files.others.push(entry.to_string()),
        }
    }
    Ok(files)
}

/// `files`' answer: paths as git spells them, from the directory it listed.
#[derive(Default)]
pub(crate) struct Files {
    /// In the index (once per stage while a merge is unresolved).
    pub(crate) cached: Vec<String>,
    /// Untracked and not ignored.
    pub(crate) others: Vec<String>,
}

/// Whether `name` is a branch name a push can update, by git's documented
/// rules for ref names applied to `refs/heads/NAME`, plus what `git branch`
/// refuses: `HEAD`, `@` and a leading `-`. No git runs.
pub(crate) fn branch_name(name: &str) -> bool {
    !(name.is_empty()
        || name == "HEAD"
        || name == "@"
        || name.starts_with('-')
        || name.starts_with("refs/"))
        && ref_name(&format!("refs/heads/{name}"))
}

/// Whether `name` is a remote name, by git's rule: `refs/remotes/NAME/test`
/// is a valid ref name. `HEAD` and `@` are remote names. No git runs.
pub(crate) fn remote_name(name: &str) -> bool {
    !name.is_empty() && ref_name(&format!("refs/remotes/{name}/test"))
}

/// Git's documented rules for a full ref name (git-check-ref-format).
fn ref_name(name: &str) -> bool {
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
    use super::{Repository, branch_name, remote_name};
    use sha2::{Digest, Sha256};

    /// `verify --commit` hashes every declared input's blob. A blob larger
    /// than the bound is hashed as it is read and never held whole, so a
    /// large input costs time, not memory; a small one is kept (a symbolic
    /// link's target is read from it).
    #[test]
    fn a_large_blob_is_hashed_as_it_is_read_and_not_kept() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("sykli-repo-blobs-{nonce}"));
        std::fs::create_dir_all(&root).unwrap();
        let small = b"small\n".to_vec();
        let big = (0..(1 << 20)).map(|i| (i % 251) as u8).collect::<Vec<_>>();
        std::fs::write(root.join("small.txt"), &small).unwrap();
        std::fs::write(root.join("big.bin"), &big).unwrap();
        for args in [
            &["init", "--quiet"][..],
            &["add", "--all"],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "--quiet",
                "-m",
                "c",
            ],
        ] {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let hex = |bytes: &[u8]| {
            Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        let repository = Repository::at(root.clone(), root.join(".git"));
        let mut session = repository.session(Vec::new());
        let names = ["HEAD:small.txt".to_string(), "HEAD:big.bin".to_string()];
        let answers = session.ask_bounded(&names, 64 * 1024).unwrap();
        session.finish().unwrap();
        let [Some(small_answer), Some(big_answer)] = &answers[..] else {
            panic!("both blobs answered");
        };
        assert_eq!(small_answer.bytes, small);
        assert_eq!(small_answer.sha256.as_deref(), Some(hex(&small).as_str()));
        assert!(big_answer.bytes.is_empty(), "a large blob is not kept");
        assert_eq!(big_answer.sha256.as_deref(), Some(hex(&big).as_str()));
        let _ = std::fs::remove_dir_all(&root);
    }

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

    #[test]
    fn remote_names_follow_gits_rule() {
        for good in ["origin", "HEAD", "@", "team.x/y", "up-stream"] {
            assert!(remote_name(good), "{good}");
        }
        for bad in ["", "a..b", "a b", "a:b", "/x", "x/", ".x", "x.lock"] {
            assert!(!remote_name(bad), "{bad:?}");
        }
    }
}
