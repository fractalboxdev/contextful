use clap::Parser;
use contextful_acceptance::conformance::{self, Options};
use std::path::PathBuf;

/// Black-box consumer compatibility over a supplied Contextful artifact.
#[derive(Parser)]
#[command(version)]
struct Args {
    #[arg(long)]
    binary: PathBuf,
    #[arg(long, default_value = "local")]
    storage_profile: String,
    /// Required scenario; repeated options select a subset. The default requires all.
    #[arg(long = "require")]
    required: Vec<String>,
    #[arg(long, default_value_t = 60)]
    timeout_seconds: u64,
    #[arg(long)]
    output: PathBuf,
}

fn main() {
    let args = Args::parse();
    let result = conformance::run(&Options {
        binary: args.binary,
        storage_profile: args.storage_profile,
        required: args.required,
        timeout_seconds: args.timeout_seconds,
    });
    match result {
        Ok(report) => {
            let serialized = serde_json::to_vec_pretty(&report).expect("report serializes");
            if std::fs::write(&args.output, serialized).is_err() {
                eprintln!("ConformanceReportUnavailable");
                std::process::exit(2);
            }
            if !report.passed() {
                eprintln!("ConformanceRequiredScenarioFailed");
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
}
