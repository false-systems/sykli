//! The append-only journal and the leases that typed production and durable
//! graph runs share (Unix only).
//!
//! A journal is a directory of immutable records, `records/<sequence>.json`,
//! each published durably and exclusively (`canonical::publish`: a synced
//! temporary, an exclusive hard link, a synced directory), each an envelope
//! whose id is the domain-separated identity of its record, and each naming
//! the id of the one before it. Loading validates the whole chain, so a torn,
//! reordered, rewritten or foreign record fails loudly instead of being read.
//! Appends are serialized by an exclusive `journal-lock`; readers take it
//! shared. Who may append at all is a lease: an advisory `flock` that the
//! kernel releases when the last descriptor of its holder closes, so a dead
//! holder never leaves a stale lock and a PID is never trusted.
use crate::canonical::{Durability, canonical, decode, digest, identity, publish_with};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// One kind of journal record: its schema (also the identity domain of its
/// envelope), the name of the thing it records (a production, a run), and
/// its facts. Each kind keeps its own record type, so the bytes of existing
/// records never change.
pub(crate) trait Record: Serialize + DeserializeOwned {
    type Fact;
    const SCHEMA: &'static str;
    /// What the owner is called in errors ("production", "run").
    const OWNER: &'static str;
    fn new(
        owner: String,
        sequence: u64,
        previous: Option<String>,
        recorded_at: u64,
        fact: Self::Fact,
    ) -> Self;
    fn schema(&self) -> &str;
    fn owner(&self) -> &str;
    fn sequence(&self) -> u64;
    fn previous(&self) -> Option<&str>;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Envelope<R> {
    pub(crate) id: String,
    pub(crate) record: R,
}

/// Where record `sequence` lives.
pub(crate) fn record_path(records: &Path, sequence: u64) -> PathBuf {
    records.join(format!("{sequence:020}.json"))
}

/// Every record in `records`, in order, with the chain validated and each
/// record handed to `apply` as it is read; `apply` refuses a fact that does
/// not follow from the ones before it. Temporaries and anything a file
/// browser drops (`.DS_Store`) are not records; a real foreign record still
/// fails loudly.
pub(crate) fn load<R: Record>(
    records: &Path,
    owner: &str,
    mut apply: impl FnMut(&R) -> Result<(), String>,
) -> Result<Vec<Envelope<R>>, String> {
    let mut paths = Vec::new();
    if records.exists() {
        for entry in fs::read_dir(records).map_err(err)? {
            let path = entry.map_err(err)?.path();
            if path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with('.'))
            {
                continue;
            }
            paths.push(path);
        }
    }
    paths.sort();
    let mut loaded: Vec<Envelope<R>> = Vec::new();
    for path in paths {
        let envelope: Envelope<R> = decode(&fs::read(&path).map_err(err)?)?;
        let record = &envelope.record;
        if record.schema() != R::SCHEMA
            || record.owner() != owner
            || record.sequence() != loaded.len() as u64 + 1
            || record.previous() != loaded.last().map(|r| r.id.as_str())
            || envelope.id != identity(R::SCHEMA, record)?
            || path.file_name().and_then(|s| s.to_str())
                != Some(format!("{:020}.json", record.sequence()).as_str())
        {
            return Err(format!(
                "record sequence, digest or {} reference mismatch",
                R::OWNER
            ));
        }
        apply(record)?;
        loaded.push(envelope);
    }
    Ok(loaded)
}

/// The next record after `records`, sealed: its sequence, the id of the one
/// before it, the time, and its own id.
pub(crate) fn seal<R: Record>(
    owner: String,
    records: &[Envelope<R>],
    fact: R::Fact,
) -> Result<Envelope<R>, String> {
    let record = R::new(
        owner,
        records.len() as u64 + 1,
        records.last().map(|r| r.id.clone()),
        crate::canonical::now(),
        fact,
    );
    Ok(Envelope {
        id: identity(R::SCHEMA, &record)?,
        record,
    })
}

/// Publish a sealed record. An identical record already there is accepted;
/// a different one at the same sequence is a conflict, never an overwrite.
pub(crate) fn write<R: Record>(
    records: &Path,
    envelope: &Envelope<R>,
    durability: Durability,
) -> Result<(), String> {
    publish_with(
        &record_path(records, envelope.record.sequence()),
        &canonical(envelope)?,
        durability,
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::{NONCE, now};
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
