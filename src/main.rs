//! Runs the lab and writes the report. The measurement itself is in the library.
//!
//! `--check` runs the lab and compares the numbers with the committed `reports/latest.json`
//! without touching it: the artifact is a claim about this code, and a claim that stopped being
//! true must fail a build instead of waiting for the next person to rerun `make lab`. It is not
//! entirely write-free — scenario 7 still writes `reports/agent-happy.jsonl` (deterministically,
//! so the tree stays clean) and creates the ignored `reports/agent-denied.jsonl` — but the two
//! committed files this flag is about are read, never written.

use std::fs;
use std::path::Path;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let check = std::env::args().skip(1).any(|arg| arg == "--check");
    let report = llmlab::run_lab().await;
    let directory = Path::new("reports");
    let committed = directory.join("latest.json");

    if check {
        let previous = match fs::read_to_string(&committed) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("llmlab: {}: {error}", committed.display());
                return ExitCode::from(2);
            }
        };
        return match llmlab::report_differences(&previous, &report.json()) {
            Ok(differences) if differences.is_empty() => {
                println!("{} matches this run", committed.display());
                ExitCode::SUCCESS
            }
            Ok(differences) => {
                eprintln!("{} no longer matches this run:", committed.display());
                for difference in &differences {
                    eprintln!("  {difference}");
                }
                ExitCode::from(1)
            }
            Err(message) => {
                eprintln!("llmlab: {message}");
                ExitCode::from(2)
            }
        };
    }

    fs::create_dir_all(directory).expect("create reports/");
    fs::write(directory.join("latest.md"), report.markdown()).expect("write reports/latest.md");
    fs::write(committed, report.json()).expect("write reports/latest.json");
    print!("{}", report.markdown());
    ExitCode::SUCCESS
}
