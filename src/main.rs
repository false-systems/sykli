//! Sykli executes declared graphs and proves what ran.
//!
//! A receipt claims exactly what ran — never what it meant.

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "sykli",
    version,
    about = "Executes declared graphs and proves what ran"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate an emitted contract without executing it
    Validate {
        /// Path to a sykli-contract.v1 JSON file
        contract: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Validate { contract } => {
            eprintln!("unimplemented: validate {}", contract.display());
            ExitCode::from(2)
        }
    }
}
