#[path = "support/stdio.rs"]
mod stdio;

use std::{fs, io::Read, process::Command};

fn payload() -> Vec<u8> {
    b"complete input\nwith multiple lines\n".repeat(65_536)
}

fn input_files() -> Vec<std::path::PathBuf> {
    let prefix = format!("sykli-test-stdin-{}-", std::process::id());
    let mut files: Vec<_> = fs::read_dir(std::env::temp_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&prefix)
        })
        .collect();
    files.sort();
    files
}

#[test]
fn stdin_child() {
    let Ok(mode) = std::env::var("SYKLI_STDIN_TEST_CHILD") else {
        return;
    };
    if mode == "refuse" {
        eprintln!("refused before reading stdin");
        std::process::exit(23);
    }
    assert_eq!(mode, "read");
    let mut bytes = Vec::new();
    std::io::stdin().read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, payload());
    println!("complete input received: {} bytes", bytes.len());
}

#[test]
fn prepared_stdin_preserves_early_exit_full_input_and_spawn_errors() {
    let before = input_files();
    let bytes = payload(); // Larger than a pipe buffer: no blocking producer.
    for mode in ["refuse", "read"] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "stdin_child", "--nocapture"])
            .env("SYKLI_STDIN_TEST_CHILD", mode);
        let out = stdio::output(&mut command, &bytes).unwrap();
        assert_eq!(
            out.status.code(),
            Some(if mode == "refuse" { 23 } else { 0 }),
            "{out:?}"
        );
        if mode == "refuse" {
            assert!(String::from_utf8_lossy(&out.stderr).contains("refused before reading stdin"));
        } else {
            assert!(String::from_utf8_lossy(&out.stdout).contains("complete input received"));
        }
        assert_eq!(
            input_files(),
            before,
            "cleanup while Command is still alive"
        );
    }
    let absent = std::env::temp_dir().join(format!("sykli-missing-command-{}", std::process::id()));
    assert!(!absent.exists());
    let mut command = Command::new(absent);
    assert_eq!(
        stdio::output(&mut command, &bytes).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    assert_eq!(input_files(), before, "spawn failure must clean up too");
}
