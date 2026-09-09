use super::*;
use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;

pub use crate::canonical::{digest, now, publish};

pub fn checked_path(root: &Path, relative_path: &str) -> Result<PathBuf, String> {
    relative(relative_path)?;
    let mut path = root.to_path_buf();
    for component in relative_path.split('/') {
        path.push(component);
        let metadata =
            fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
            return Err(format!(
                "symlink/special file unsupported: {}",
                path.display()
            ));
        }
        if metadata.is_dir() && path.join(".git").exists() {
            return Err(format!(
                "nested repository/submodule unsupported: {}",
                path.display()
            ));
        }
    }
    Ok(path)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    content: Option<String>,
    executable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tree {
    schema: String,
    entries: BTreeMap<String, Entry>,
}

pub struct Store(pub PathBuf);
impl Store {
    pub fn new(path: &Path) -> Result<Self, String> {
        Ok(Self(super::super::absolute(path)?))
    }
    pub fn production(&self, id: &str) -> Result<PathBuf, String> {
        digest(id)?;
        Ok(self.0.join("requests").join(id))
    }
    pub fn blob_path(&self, id: &str) -> Result<PathBuf, String> {
        digest(id)?;
        Ok(self.0.join("blobs").join(id))
    }
    pub fn blob(&self, bytes: &[u8]) -> Result<String, String> {
        let id = super::super::sha256(bytes);
        let path = self.blob_path(&id)?;
        publish(&path, bytes)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o555)).map_err(err)?;
        Ok(id)
    }
    pub fn read_blob(&self, id: &str) -> Result<Vec<u8>, String> {
        let path = self.blob_path(id)?;
        if !fs::symlink_metadata(&path).map_err(err)?.is_file() {
            return Err("artifact is not a regular file".into());
        }
        let bytes = fs::read(&path).map_err(|e| format!("artifact-unavailable {id}: {e}"))?;
        if super::super::sha256(&bytes) != id {
            return Err(format!("artifact digest mismatch {id}"));
        }
        Ok(bytes)
    }
    fn entry(&self, path: &Path) -> Result<Entry, String> {
        let metadata = fs::symlink_metadata(path).map_err(err)?;
        if !metadata.is_file() {
            return Err(format!("expected regular file: {}", path.display()));
        }
        // ponytail: whole-file collection; stream blobs if large artifacts justify it.
        Ok(Entry {
            content: Some(self.blob(&fs::read(path).map_err(err)?)?),
            executable: metadata.mode() & 0o111 != 0,
        })
    }
    pub fn capture(&self, root: &Path, source: &Source) -> Result<Artifact, String> {
        snapshot(root, source, |bytes| self.blob(bytes))
    }
    fn walk(
        &self,
        root: &Path,
        directory: &Path,
        entries: &mut BTreeMap<String, Entry>,
    ) -> Result<(), String> {
        for child in fs::read_dir(directory).map_err(err)? {
            let path = child.map_err(err)?.path();
            let relative_path = path
                .strip_prefix(root)
                .map_err(err)?
                .to_str()
                .ok_or("non-UTF8 artifact path")?;
            let checked = checked_path(root, relative_path)?;
            if checked.is_dir() {
                entries.insert(
                    relative_path.into(),
                    Entry {
                        content: None,
                        executable: false,
                    },
                );
                self.walk(root, &checked, entries)?;
            } else {
                entries.insert(relative_path.into(), self.entry(&checked)?);
            }
        }
        Ok(())
    }
    pub fn collect(&self, root: &Path, output: &Output) -> Result<Artifact, String> {
        let path = checked_path(root, &output.collect)?;
        let content = match &output.ty {
            ArtifactType::Directory => {
                if !path.is_dir() {
                    return Err("expected directory output".into());
                }
                let mut entries = BTreeMap::new();
                self.walk(&path, &path, &mut entries)?;
                self.blob(&canonical(&Tree {
                    schema: "sykli-tree.v1".into(),
                    entries,
                })?)?
            }
            ty => {
                let entry = self.entry(&path)?;
                let id = entry.content.unwrap();
                validate_bytes(ty, &self.read_blob(&id)?)?;
                if matches!(ty, ArtifactType::Executable { .. }) && !entry.executable {
                    return Err("executable output has no executable mode".into());
                }
                id
            }
        };
        Ok(Artifact {
            content,
            ty: output.ty.clone(),
        })
    }
    fn tree(&self, artifact: &Artifact) -> Result<Tree, String> {
        let tree: Tree = decode(&self.read_blob(&artifact.content)?)?;
        if tree.schema != "sykli-tree.v1" {
            return Err("unsupported tree schema".into());
        }
        for (path, entry) in &tree.entries {
            relative(path)?;
            for parent in Path::new(path).ancestors().skip(1) {
                if tree
                    .entries
                    .get(parent.to_str().ok_or("non-UTF8 tree")?)
                    .is_some_and(|e| e.content.is_some())
                {
                    return Err("tree file/directory collision".into());
                }
            }
            if let Some(id) = &entry.content {
                digest(id)?;
            } else if entry.executable {
                return Err("invalid directory entry".into());
            }
        }
        Ok(tree)
    }
    pub fn available(&self, artifact: &Artifact) -> Result<(), String> {
        match artifact.ty {
            ArtifactType::Directory | ArtifactType::SourceTree => {
                for entry in self.tree(artifact)?.entries.values() {
                    if let Some(id) = &entry.content {
                        self.read_blob(id)?;
                    }
                }
                Ok(())
            }
            _ => {
                validate_bytes(&artifact.ty, &self.read_blob(&artifact.content)?)?;
                if matches!(artifact.ty, ArtifactType::Executable { .. })
                    && fs::metadata(self.blob_path(&artifact.content)?)
                        .map_err(err)?
                        .mode()
                        & 0o111
                        == 0
                {
                    return Err(
                        "artifact-unavailable: executable location has no executable mode".into(),
                    );
                }
                Ok(())
            }
        }
    }
    pub fn materialize(&self, artifact: &Artifact, destination: &Path) -> Result<(), String> {
        match artifact.ty {
            ArtifactType::Directory | ArtifactType::SourceTree => {
                fs::create_dir(destination).map_err(err)?;
                for (p, entry) in self.tree(artifact)?.entries {
                    let path = destination.join(p);
                    if let Some(id) = entry.content {
                        fs::create_dir_all(path.parent().unwrap()).map_err(err)?;
                        fs::write(&path, self.read_blob(&id)?).map_err(err)?;
                        fs::set_permissions(
                            path,
                            fs::Permissions::from_mode(if entry.executable {
                                0o755
                            } else {
                                0o644
                            }),
                        )
                        .map_err(err)?;
                    } else {
                        fs::create_dir_all(path).map_err(err)?;
                    }
                }
            }
            _ => {
                let bytes = self.read_blob(&artifact.content)?;
                validate_bytes(&artifact.ty, &bytes)?;
                fs::write(destination, bytes).map_err(err)?;
                fs::set_permissions(
                    destination,
                    fs::Permissions::from_mode(
                        if matches!(artifact.ty, ArtifactType::Executable { .. }) {
                            0o755
                        } else {
                            0o644
                        },
                    ),
                )
                .map_err(err)?;
            }
        }
        Ok(())
    }
}

pub fn validate_bytes(ty: &ArtifactType, bytes: &[u8]) -> Result<(), String> {
    let ArtifactType::Executable {
        format,
        architecture,
    } = ty
    else {
        return Ok(());
    };
    let detected = if bytes.len() >= 64
        && &bytes[..4] == b"\x7fELF"
        && bytes[4] == 2
        && bytes[5] == 1
        && bytes[6] == 1
        && matches!(u16::from_le_bytes([bytes[16], bytes[17]]), 2 | 3)
    {
        (
            "elf",
            match u16::from_le_bytes([bytes[18], bytes[19]]) {
                62 => "x86_64",
                183 => "aarch64",
                _ => "unsupported",
            },
        )
    } else if bytes.len() >= 32
        && bytes[..4] == [0xcf, 0xfa, 0xed, 0xfe]
        && u32::from_le_bytes(bytes[12..16].try_into().unwrap()) == 2
    {
        (
            "macho",
            match u32::from_le_bytes(bytes[4..8].try_into().unwrap()) {
                0x01000007 => "x86_64",
                0x0100000c => "aarch64",
                _ => "unsupported",
            },
        )
    } else {
        return Err("unsupported or invalid executable header".into());
    };
    if detected != (format.as_str(), architecture.as_str()) {
        return Err(format!(
            "executable type mismatch: expected {format}/{architecture}, found {}/{}",
            detected.0, detected.1
        ));
    }
    Ok(())
}

// Native advisory locks: the open file description survives controller exit
// only in the bounded executor, avoiding PID reuse and stale lock-file deletion.
unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
    fn fcntl(fd: i32, command: i32, ...) -> i32;
}

pub struct Lease(File);
impl Lease {
    pub fn acquire(directory: &Path) -> Result<Self, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("lease"))
            .map_err(err)?;
        // SAFETY: the descriptor is owned by file; LOCK_EX | LOCK_NB on Unix.
        if unsafe { flock(file.as_raw_fd(), 2 | 4) } != 0 {
            let error = std::io::Error::last_os_error();
            // Only a held lock means another controller; ENOLCK/EOPNOTSUPP
            // (network file systems) are a store problem, not a busy peer.
            return Err(if error.kind() == std::io::ErrorKind::WouldBlock {
                format!("production-busy: {error}")
            } else {
                format!("lease unavailable on this file system: {error}")
            });
        }
        Ok(Self(file))
    }
    fn lock(file: File, mode: i32) -> Result<Self, String> {
        // SAFETY: the descriptor is owned by file. Retry interrupted flock calls.
        loop {
            if unsafe { flock(file.as_raw_fd(), mode) } == 0 {
                return Ok(Self(file));
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(err(error));
            }
        }
    }

    pub fn journal(directory: &Path) -> Result<Self, String> {
        let path = directory.join("journal-lock");
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)
                .map_err(err)?,
            Err(error) => return Err(err(error)),
        };
        Self::lock(file, 2) // LOCK_EX; the lock file's contents are never written.
    }

    pub fn journal_read(directory: &Path) -> Result<Option<Self>, String> {
        match File::open(directory.join("journal-lock")) {
            Ok(file) => Self::lock(file, 1).map(Some), // LOCK_SH
            // Older stores have no journal lock. Read their validated atomic prefix.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(err(error)),
        }
    }

    // Observe an existing lease without creating or writing any file. Release
    // the probe immediately: inspection must not reserve the controller lease.
    pub fn observe(path: &Path) -> Result<bool, String> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(err(error)),
        };
        loop {
            // SAFETY: owned descriptor; LOCK_SH | LOCK_NB does not modify bytes.
            if unsafe { flock(file.as_raw_fd(), 1 | 4) } == 0 {
                return Ok(false);
            }
            let error = std::io::Error::last_os_error();
            match error.kind() {
                std::io::ErrorKind::WouldBlock => return Ok(true),
                std::io::ErrorKind::Interrupted => continue,
                _ => return Err(err(error)),
            }
        }
    }

    pub fn attempt(directory: &Path, attempt: &str) -> Result<Self, String> {
        digest(attempt)?;
        let parent = directory.join("attempt-leases");
        fs::create_dir_all(&parent).map_err(err)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(parent.join(attempt))
            .map_err(err)?;
        // Wait for short-lived inspection probes before claiming this attempt.
        // A duplicate executor checks the recorded terminal outcome after locking.
        Self::lock(file, 2)
    }
    pub fn inherit(&self, command: &mut ProcessCommand) -> RawFd {
        let fd = self.0.as_raw_fd();
        // SAFETY: fcntl is async-signal-safe; no allocation in pre_exec.
        unsafe {
            command.pre_exec(move || {
                if fcntl(fd, 2, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        fd
    }
    pub fn received(directory: &Path, fd: RawFd) -> Result<Self, String> {
        // SAFETY: F_GETFD only inspects whether this descriptor is valid.
        if fd < 3 || unsafe { fcntl(fd, 1) } == -1 {
            return Err("executor requires inherited lease".into());
        }
        // SAFETY: this private child entrypoint receives ownership of one inherited FD.
        let file = unsafe { File::from_raw_fd(fd) };
        let actual = file.metadata().map_err(err)?;
        let expected = fs::metadata(directory.join("lease")).map_err(err)?;
        if (actual.dev(), actual.ino()) != (expected.dev(), expected.ino()) {
            return Err("executor lease mismatch".into());
        }
        // Never pass the controller lease on to arbitrary recipe processes.
        if unsafe { fcntl(fd, 2, 1) } == -1 {
            return Err(err(std::io::Error::last_os_error()));
        }
        Ok(Self(file))
    }
}

pub fn snapshot(
    root: &Path,
    source: &Source,
    mut blob: impl FnMut(&[u8]) -> Result<String, String>,
) -> Result<Artifact, String> {
    let mut entries = BTreeMap::new();
    for path in &source.paths {
        let file = checked_path(root, path)?;
        let metadata = fs::symlink_metadata(&file).map_err(err)?;
        if !metadata.is_file() {
            return Err(format!("expected regular source file {path}"));
        }
        entries.insert(
            path.clone(),
            Entry {
                content: Some(blob(&fs::read(file).map_err(err)?)?),
                executable: metadata.mode() & 0o111 != 0,
            },
        );
    }
    Ok(Artifact {
        content: blob(&canonical(&Tree {
            schema: "sykli-tree.v1".into(),
            entries,
        })?)?,
        ty: source.ty.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::NONCE;
    use std::sync::atomic::Ordering;

    #[test]
    fn observing_a_lease_does_not_reserve_it() {
        let directory = std::env::temp_dir().join(format!(
            "sykli-lease-{}-{}-{}",
            std::process::id(),
            now(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("lease");
        assert!(!Lease::observe(&path).unwrap());
        assert!(!path.exists(), "observation must not create a lease");
        File::create(&path).unwrap();
        let observed = Lease::observe(&path).unwrap();
        // Other unit tests fork commands concurrently. A child can briefly
        // inherit the probe's descriptor until exec closes it (CLOEXEC).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let next_controller = loop {
            match Lease::acquire(&directory) {
                Ok(lease) => break lease,
                Err(error) => {
                    assert!(std::time::Instant::now() < deadline, "{error}");
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        };
        assert!(!observed);
        assert!(Lease::observe(&path).unwrap());
        assert!(Lease::acquire(&directory).is_err());
        drop(next_controller);
        fs::remove_dir_all(directory).unwrap();
    }
}
