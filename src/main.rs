//! Runs the lab and writes the report. The measurement itself is in the library.

use std::fs;
use std::path::Path;

#[tokio::main]
async fn main() {
    let report = llmlab::run_lab().await;
    let directory = Path::new("reports");
    fs::create_dir_all(directory).expect("create reports/");
    fs::write(directory.join("latest.md"), report.markdown()).expect("write reports/latest.md");
    fs::write(directory.join("latest.json"), report.json()).expect("write reports/latest.json");
    print!("{}", report.markdown());
}
