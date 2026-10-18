//! Finite test stdin prepared before spawn: no parent writer can race an exit.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Seek, Write},
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Input {
    path: Option<PathBuf>,
    file: Option<File>,
}

impl Input {
    fn remove(&mut self) -> io::Result<()> {
        drop(self.file.take());
        if let Some(path) = &self.path {
            fs::remove_file(path)?;
            self.path = None;
        }
        Ok(())
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        if let Err(error) = self.remove() {
            eprintln!("test stdin cleanup failed for {:?}: {error}", self.path);
        }
    }
}

pub fn output(command: &mut Command, bytes: &[u8]) -> io::Result<Output> {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "sykli-test-stdin-{}-{time}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // Arm cleanup only after create_new succeeds: never remove somebody else's file.
    let file = options.open(&path)?;
    let mut input = Input {
        path: Some(path),
        file: Some(file),
    };
    let file = input.file.as_mut().unwrap();
    file.write_all(bytes)?;
    file.rewind()?;
    let result = command.stdin(input.file.take().unwrap()).output();
    // Command retains its Stdio even after output() returns. Close it before
    // removal, including on spawn failure (required for cleanup on Windows).
    command.stdin(Stdio::null());
    input.remove()?;
    result
}
