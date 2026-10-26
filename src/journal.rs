//! The append-only journal and the leases of durable graph runs (Unix only).
//!
//! A journal is a directory of immutable records, `records/<sequence>.json`,
//! each published durably and exclusively (`canonical::publish_with`: a synced
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
/// envelope), the name of the thing it records (a run), and its facts. Each
/// kind keeps its own record type, so the bytes of existing records never
/// change.
pub(crate) trait Record: Serialize + DeserializeOwned {
    type Fact;
    const SCHEMA: &'static str;
    /// What the owner is called in errors ("run").
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
        let named = |e: std::io::Error| format!("{}: {e}", records.display());
        for entry in fs::read_dir(records).map_err(named)? {
            let path = entry.map_err(named)?.path();
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
        // Every refusal names the record it is about.
        let named = |e: String| format!("{}: {e}", path.display());
        let bytes = fs::read(&path).map_err(|e| named(err(e)))?;
        let envelope: Envelope<R> = decode(&bytes).map_err(named)?;
        let record = &envelope.record;
        if record.schema() != R::SCHEMA
            || record.owner() != owner
            || record.sequence() != loaded.len() as u64 + 1
            || record.previous() != loaded.last().map(|r| r.id.as_str())
            || envelope.id != identity(R::SCHEMA, record)?
            || path.file_name().and_then(|s| s.to_str())
                != Some(format!("{:020}.json", record.sequence()).as_str())
        {
            return Err(named(format!(
                "record sequence, digest or {} reference mismatch",
                R::OWNER
            )));
        }
        apply(record).map_err(named)?;
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
    let path = record_path(records, envelope.record.sequence());
    publish_with(&path, &canonical(envelope)?, durability)
        .map_err(|e| format!("{}: {e}", path.display()))
}

// Native advisory locks: the open file description survives controller exit
// only in the bounded executor, avoiding PID reuse and stale lock-file deletion.
unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
    fn fcntl(fd: i32, command: i32, ...) -> i32;
}

// Process control for runs and their tasks, declared once here.
unsafe extern "C" {
    pub(crate) fn setsid() -> i32;
    pub(crate) fn setpgid(pid: i32, pgid: i32) -> i32;
    pub(crate) fn killpg(pgid: i32, signal: i32) -> i32;
    pub(crate) fn kill(pid: i32, signal: i32) -> i32;
    pub(crate) fn signal(signal: i32, handler: usize) -> usize;
    fn getdtablesize() -> i32;
}

/// Whether any process is still in process group `group`, by signal 0 (no
/// signal is sent). A group sykli may not signal (EPERM) still exists.
pub(crate) fn group_exists(group: i32) -> bool {
    if group <= 1 {
        return false;
    }
    // SAFETY: signal 0 only checks; a negative pid names the group.
    let found = unsafe { kill(-group, 0) } == 0;
    found || std::io::Error::last_os_error().raw_os_error() == Some(1)
}

/// Mark every descriptor from 3 up close-on-exec, so a process this one
/// starts inherits none of them (a caller's pipe, say) unless it is handed
/// one explicitly, as a lease is (`Lease::inherit`). The open descriptors are
/// listed from `/dev/fd`; where that cannot be read, every number up to the
/// descriptor limit is tried.
pub(crate) fn close_inherited_on_exec() {
    let listed = fs::read_dir("/dev/fd").ok().map(|entries| {
        entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
            .collect::<Vec<_>>()
    });
    // SAFETY: getdtablesize only reads the process's limit.
    let all = || (0..unsafe { getdtablesize() }.clamp(0, 1 << 16)).collect::<Vec<_>>();
    for fd in listed.unwrap_or_else(all).into_iter().filter(|fd| *fd >= 3) {
        // SAFETY: F_SETFD (2) with FD_CLOEXEC (1) changes only this
        // process's descriptor flags; EBADF for a closed one is harmless.
        unsafe {
            fcntl(fd, 2, 1);
        }
    }
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
                format!("lease-busy: {error}")
            } else {
                format!("lease unavailable on this file system: {error}")
            });
        }
        Ok(Self(file))
    }
    /// Take an existing lease, never creating one: None when there is no
    /// `lease` in `directory` (it is still being set up, or already gone),
    /// an error when another holds it. For a remover, which must never
    /// re-create a lease inside a directory another is deleting.
    pub fn acquire_existing(directory: &Path) -> Result<Option<Self>, String> {
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.join("lease"))
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(err(error)),
        };
        // SAFETY: the descriptor is owned by file; LOCK_EX | LOCK_NB on Unix.
        if unsafe { flock(file.as_raw_fd(), 2 | 4) } != 0 {
            return Err(format!("lease-busy: {}", std::io::Error::last_os_error()));
        }
        Ok(Some(Self(file)))
    }

    /// Hold the lease at `path` shared, beside any other shared holder,
    /// waiting while one holds it alone (a remover, briefly). None when,
    /// once it is held, `path` no longer names it, or its directory is
    /// gone: a remover moved the directory aside meanwhile, and the caller
    /// starts again from a directory that is there.
    pub fn share(path: &Path) -> Result<Option<Self>, String> {
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        let lease = Self::lock(file, 1)?; // LOCK_SH
        let held = lease.0.metadata().map_err(err)?;
        Ok(fs::metadata(path)
            .ok()
            .filter(|now| (now.dev(), now.ino()) == (held.dev(), held.ino()))
            .map(|_| lease))
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

    /// Journal critical sections are short. A stopped writer must not hang
    /// inspection or another operation indefinitely; never steal its lock.
    fn journal_lock(file: File, mode: i32, path: &Path) -> Result<Self, String> {
        use std::time::{Duration, Instant};
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            // SAFETY: owned descriptor; LOCK_NB never waits in the kernel.
            if unsafe { flock(file.as_raw_fd(), mode | 4) } == 0 {
                return Ok(Self(file));
            }
            let error = std::io::Error::last_os_error();
            if !matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) {
                return Err(format!("{}: {error}", path.display()));
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "journal-busy: {} remained locked for 2 seconds; retry when its holder finishes",
                    path.display()
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
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
                .open(&path)
                .map_err(err)?,
            Err(error) => return Err(err(error)),
        };
        Self::journal_lock(file, 2, &path) // LOCK_EX; contents are never written.
    }

    pub fn journal_read(directory: &Path) -> Result<Option<Self>, String> {
        let path = directory.join("journal-lock");
        match File::open(&path) {
            Ok(file) => Self::journal_lock(file, 1, &path).map(Some), // LOCK_SH
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
    fn journal_contention_is_bounded_without_stealing_the_lock() {
        let directory = std::env::temp_dir().join(format!(
            "sykli-journal-busy-{}-{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        for read in [true, false] {
            let held = Lease::journal(&directory).unwrap();
            // A finite holder also bounds this regression on the old implementation.
            let release = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(3));
                drop(held);
            });
            let result = if read {
                Lease::journal_read(&directory).map(|_| ())
            } else {
                Lease::journal(&directory).map(|_| ())
            };
            release.join().unwrap();
            assert!(result.is_err_and(|error| error.contains("journal-busy")));
            assert!(directory.join("journal-lock").is_file());
            drop(Lease::journal(&directory).unwrap());
        }
        fs::remove_dir_all(directory).unwrap();
    }

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

    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "sykli-{name}-{}-{}-{}",
            std::process::id(),
            now(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        directory
    }

    /// A process that another thread forks has a copy of every descriptor
    /// until it execs, and a lock belongs to the open file description that
    /// copy shares. Dropping a lease in that window still releases it: the
    /// next holder is never refused for a child that only had not exec'd yet.
    #[test]
    fn a_dropped_lease_is_free_while_a_forked_child_still_has_its_descriptor() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        let directory = scratch("forked");
        let lease = Lease::acquire(&directory).unwrap();
        // The child says it has forked, then waits to be told to exec.
        let (forked, mut has_forked) = UnixStream::pair().unwrap();
        let (go_on, mut tell) = UnixStream::pair().unwrap();
        let forker = std::thread::spawn(move || {
            let mut command = ProcessCommand::new("true");
            // SAFETY: one write and one read on sockets; nothing allocates.
            unsafe {
                command.pre_exec(move || {
                    (&forked).write_all(&[1])?;
                    (&go_on).read_exact(&mut [0])
                });
            }
            command.status()
        });
        has_forked.read_exact(&mut [0]).unwrap();
        drop(lease);
        let next = Lease::acquire_existing(&directory);
        tell.write_all(&[1]).unwrap();
        assert!(forker.join().unwrap().unwrap().success());
        assert!(matches!(next, Ok(Some(_))), "{:?}", next.err());
        drop(next);
        fs::remove_dir_all(directory).unwrap();
    }

    /// A lease handed to a child stays held by it once this process drops
    /// its own copy: a task that outlives its coordinator reads as running.
    #[test]
    fn a_lease_handed_to_a_child_stays_held_once_this_copy_is_dropped() {
        let directory = scratch("handed");
        let lease = Lease::acquire(&directory).unwrap();
        let mut command = ProcessCommand::new("sh");
        lease.inherit(&mut command);
        // The child holds it until its stdin closes.
        let mut child = command
            .args(["-c", "read line || true"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        drop(lease);
        let refused = Lease::acquire_existing(&directory).map(|lease| lease.is_some());
        drop(child.stdin.take());
        assert!(child.wait().unwrap().success());
        assert!(
            refused
                .as_ref()
                .is_err_and(|error| error.starts_with("lease-busy")),
            "{refused:?}"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    /// Shared holders exclude a remover and not each other; a holder that
    /// waited while a remover moved the directory aside gets nothing, and
    /// starts again from a directory that is there.
    #[test]
    fn a_shared_lease_is_refused_once_its_directory_is_moved_aside() {
        let base = std::env::temp_dir().join(format!(
            "sykli-share-{}-{}-{}",
            std::process::id(),
            now(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        let directory = base.join("held");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("lease");
        let first = Lease::share(&path).unwrap().expect("held");
        let second = Lease::share(&path).unwrap().expect("held beside the first");
        assert!(Lease::acquire_existing(&directory).is_err());
        drop((first, second));

        // A remover holds it alone, a holder waits, the remover moves the
        // directory aside and lets go: the holder gets nothing.
        let remover = Lease::acquire_existing(&directory).unwrap().unwrap();
        let waiting = {
            let path = path.clone();
            std::thread::spawn(move || Lease::share(&path).map(|lease| lease.is_some()))
        };
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::rename(&directory, base.join("aside")).unwrap();
        drop(remover);
        assert_eq!(waiting.join().unwrap(), Ok(false));
        assert!(
            Lease::share(&path).unwrap().is_none(),
            "its directory is gone"
        );
        fs::remove_dir_all(base).unwrap();
    }
}
