//! The snapshot a receipt records, checked against the way sykli always
//! built it: an index seeded from HEAD, `git add --all`, `.sykli` removed,
//! the tree written. That oracle runs here with plain git; sykli builds the
//! same tree with fewer processes, and must agree in every case, whatever
//! pathspec settings git hands down in the environment.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
};

/// Git exports these to hooks (`git --literal-pathspecs commit`, and the
/// like); none may change what the snapshot leaves out.
const PATHSPEC_SETTINGS: [&str; 4] = [
    "GIT_LITERAL_PATHSPECS",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
];

const CONTRACT: &str = r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t","run":"test -f file.txt","inputs":["file.txt"]}]}"#;

struct Scratch {
    base: PathBuf,
    repo: PathBuf,
    contract: PathBuf,
    indexes: AtomicUsize,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("sykli-snapshot-{name}-{nonce}"));
        let repo = base.join("repo");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(base.join("cfg")).unwrap();
        let contract = base.join("cfg/sykli.json");
        fs::write(&contract, CONTRACT).unwrap();
        let scratch = Scratch {
            base,
            repo,
            contract,
            indexes: AtomicUsize::new(0),
        };
        scratch.git(&["init", "--quiet", "--initial-branch=main"], None);
        fs::write(scratch.repo.join("file.txt"), "one\n").unwrap();
        scratch.git(&["add", "--all"], None);
        scratch.git(&["commit", "--quiet", "-m", "fixture"], None);
        scratch
    }

    /// Plain git in the repository, without any pathspec setting; with
    /// `index`, on that index file instead of the repository's.
    fn git(&self, args: &[&str], index: Option<&Path>) -> String {
        let mut command = Command::new("git");
        command
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(&self.repo);
        for setting in PATHSPEC_SETTINGS {
            command.env_remove(setting);
        }
        if let Some(index) = index {
            command.env("GIT_INDEX_FILE", index);
        }
        let out = command.output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// The working tree's and HEAD's trees as sykli always computed them.
    fn oracle(&self) -> (String, String) {
        let index = |n: usize| self.base.join(format!("oracle-index-{n}"));
        let working = index(self.indexes.fetch_add(1, Ordering::SeqCst));
        let head = index(self.indexes.fetch_add(1, Ordering::SeqCst));
        let remove = ["rm", "--cached", "-r", "-q", "--ignore-unmatch", ".sykli"];
        self.git(&["read-tree", "HEAD"], Some(&working));
        self.git(&["add", "--all"], Some(&working));
        self.git(&remove, Some(&working));
        let tree = self.git(&["write-tree"], Some(&working));
        self.git(&["read-tree", "HEAD"], Some(&head));
        self.git(&remove, Some(&head));
        (tree, self.git(&["write-tree"], Some(&head)))
    }

    fn sykli(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_sykli"))
            .args(args)
            .envs(env.iter().copied())
            .current_dir(&self.repo)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    /// A run's receipt trees: the working tree's and HEAD's.
    fn run(&self, env: &[(&str, &str)]) -> (String, String) {
        let out = self.sykli(&["run", self.contract.to_str().unwrap(), "--json"], env);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{env:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let receipt: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let subject = &receipt["subject"];
        (
            subject["tree_oid"].as_str().unwrap().to_owned(),
            subject["head_tree_oid"].as_str().unwrap().to_owned(),
        )
    }

    /// The oracle, then a run; they must agree.
    fn agree(&self, case: &str, env: &[(&str, &str)]) {
        let expected = self.oracle();
        assert_eq!(self.run(env), expected, "{case} {env:?}");
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[test]
fn the_snapshot_leaves_out_sykli_as_it_always_did() {
    let scratch = Scratch::new("cases");
    let repo = &scratch.repo;
    let lock = scratch.sykli(&["lock", scratch.contract.to_str().unwrap()], &[]);
    assert!(lock.status.success());

    // Untracked and not ignored: a run's receipts are left out.
    fs::create_dir_all(repo.join(".sykli/receipts")).unwrap();
    fs::write(repo.join(".sykli/receipts/rcpt_x.json"), "{}").unwrap();
    scratch.agree("untracked, not ignored", &[]);

    // Ignored, the usual case.
    fs::write(repo.join(".gitignore"), ".sykli/\n").unwrap();
    scratch.git(&["add", ".gitignore"], None);
    scratch.git(&["commit", "--quiet", "-m", "ignore"], None);
    scratch.agree("ignored", &[]);

    // A folder named alike, nested or beside it, is ordinary content.
    fs::create_dir_all(repo.join("sub/.sykli")).unwrap();
    fs::write(repo.join("sub/.sykli/x"), "x").unwrap();
    fs::create_dir_all(repo.join(".sykli-notes")).unwrap();
    fs::write(repo.join(".sykli-notes/x"), "x").unwrap();
    scratch.agree("look-alikes", &[]);

    // HEAD tracks a file under .sykli: it is left out of both trees, so the
    // checkout is not dirty, and HEAD's tree is not the commit's own.
    fs::write(repo.join(".sykli/kept.txt"), "kept").unwrap();
    scratch.git(&["add", "--force", ".sykli/kept.txt"], None);
    scratch.git(&["add", "--all"], None);
    scratch.git(&["commit", "--quiet", "-m", "tracks .sykli"], None);
    let (tree, head) = scratch.oracle();
    assert_ne!(head, scratch.git(&["rev-parse", "HEAD^{tree}"], None));
    assert_eq!(tree, head);
    scratch.agree("tracked .sykli", &[]);

    // An upper-case .SYKLI, tracked. On a case-insensitive file system it is
    // the folder receipts are written to; either way, the oracle decides.
    scratch.git(&["rm", "--quiet", "--cached", "-r", ".sykli"], None);
    fs::remove_dir_all(repo.join(".sykli")).unwrap();
    fs::create_dir_all(repo.join(".SYKLI")).unwrap();
    fs::write(repo.join(".SYKLI/note.txt"), "note").unwrap();
    scratch.git(&["add", "--force", ".SYKLI/note.txt"], None);
    scratch.git(&["commit", "--quiet", "-m", "upper case"], None);
    scratch.agree("tracked .SYKLI", &[]);
    fs::create_dir_all(repo.join(".sykli/receipts")).unwrap();
    fs::write(repo.join(".sykli/receipts/rcpt_y.json"), "{}").unwrap();
    scratch.agree("tracked .SYKLI with receipts", &[]);
}

/// Git exports GIT_LITERAL_PATHSPECS to hooks when it is given
/// `--literal-pathspecs`, and the like for the other settings. The
/// snapshot's own pathspecs must mean what they say whatever is set.
#[test]
fn pathspec_settings_in_the_environment_do_not_change_the_snapshot() {
    let scratch = Scratch::new("settings");
    let repo = &scratch.repo;
    let lock = scratch.sykli(&["lock", scratch.contract.to_str().unwrap()], &[]);
    assert!(lock.status.success());
    fs::create_dir_all(repo.join(".sykli/receipts")).unwrap();
    fs::write(repo.join(".sykli/receipts/rcpt_x.json"), "{}").unwrap();
    for ignored in [false, true] {
        if ignored {
            fs::write(repo.join(".gitignore"), ".sykli/\n").unwrap();
            scratch.git(&["add", ".gitignore"], None);
            scratch.git(&["commit", "--quiet", "-m", "ignore"], None);
        }
        for setting in PATHSPEC_SETTINGS {
            scratch.agree(&format!("ignored {ignored}"), &[(setting, "1")]);
        }
    }

    // The pre-commit hook, as `git --literal-pathspecs commit` runs it.
    let in_repo = repo.join("sykli.json");
    fs::write(&in_repo, CONTRACT).unwrap();
    assert!(scratch.sykli(&["lock", "sykli.json"], &[]).status.success());
    scratch.git(&["add", "--all"], None);
    scratch.git(&["commit", "--quiet", "-m", "contract"], None);
    fs::write(repo.join("file.txt"), "two\n").unwrap();
    scratch.git(&["add", "file.txt"], None);
    for setting in PATHSPEC_SETTINGS {
        let out = scratch.sykli(&["hook", "pre-commit"], &[(setting, "1")]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{setting}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
