//! Sykli executes declared graphs and proves what ran.
//!
//! A receipt claims exactly what ran — never what it meant.

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

use sykli::{Error, load_valid_contract, plan, run};

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

        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },

    /// Show the dependency-level execution plan for a contract
    Plan {
        /// Path to a sykli-contract.v1 JSON file
        contract: PathBuf,

        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },

    /// Execute a contract with the shell runtime
    Run {
        /// Path to a sykli-contract.v1 JSON file
        contract: PathBuf,

        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Validate { contract, json } => validate_cmd(contract, json),
        Command::Plan { contract, json } => plan_cmd(contract, json),
        Command::Run { contract, json } => run_cmd(contract, json),
    };

    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(2)
        }
    }
}

fn validate_cmd(contract: PathBuf, json: bool) -> Result<ExitCode, Error> {
    let valid = load_valid_contract(&contract)?;
    if json {
        print_json(&plan(&valid))?;
    } else {
        println!(
            "valid {} {} task(s), {} level(s)",
            valid.contract_hash,
            valid.contract.tasks.len(),
            valid.levels.len()
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn plan_cmd(contract: PathBuf, json: bool) -> Result<ExitCode, Error> {
    let valid = load_valid_contract(&contract)?;
    let plan = plan(&valid);
    if json {
        print_json(&plan)?;
    } else {
        println!("contract {}", plan.contract_hash);
        for (index, level) in plan.levels.iter().enumerate() {
            println!("level {}: {}", index + 1, level.join(", "));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn run_cmd(contract: PathBuf, json: bool) -> Result<ExitCode, Error> {
    let valid = load_valid_contract(&contract)?;
    let root = contract
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let report = run(&valid, root)?;
    let success = report.outcome == sykli::RunOutcome::Passed;
    if json {
        print_json(&report)?;
    } else {
        for task in &report.tasks {
            println!("{} {:?} {}ms", task.name, task.outcome, task.duration_ms);
        }
        println!("{:?}", report.outcome);
    }

    Ok(if success {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), Error> {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer_pretty(&mut lock, value).map_err(Error::Serialize)?;
    use std::io::Write;
    writeln!(&mut lock).map_err(|source| Error::Io {
        path: PathBuf::from("<stdout>"),
        source,
    })
}
