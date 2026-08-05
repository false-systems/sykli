use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() != Some("gate") {
        eprintln!("usage: cargo xtask gate");
        return ExitCode::from(2);
    }

    for args in [
        &["fmt", "--check"][..],
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings",
        ],
        &["test", "--workspace", "--locked"],
    ] {
        let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args(args)
            .status();
        if !status.is_ok_and(|status| status.success()) {
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
