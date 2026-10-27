//! `corpus.registry.prose-report`: the advisory Vale report over the guides and
//! `references/`, under the `WriteTechnically` style in `.vale/`. It reports and refuses
//! nothing.

use anyhow::Result;
use std::path::Path;
use std::process::Command;

/// The Vale release the report is written against.
pub const VERSION: &str = "3.25.1";
const CONFIG: &str = ".vale/vale.ini";
const PATHS: [&str; 2] = ["spec/guide", "references"];

/// Run Vale over [`PATHS`] and print each alert; every outcome exits 0.
pub fn report(root: &Path) -> Result<()> {
    let installed = Command::new("vale").arg("--version").current_dir(root).output();
    let Some(version) = installed.ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()) else {
        println!("prose: vale is absent from PATH; the report is written against Vale {VERSION}; no alerts read");
        return Ok(());
    };
    if !version.trim_start_matches("vale version ").trim_start_matches('v').starts_with(VERSION) {
        println!("prose: `{version}` differs from the pinned Vale {VERSION}; alerts may differ");
    }
    let out = Command::new("vale").args(["--config", CONFIG, "--output", "line", "--no-exit"]).args(PATHS).current_dir(root).output()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout.lines().filter(|l| !l.trim().is_empty()).for_each(|l| println!("{l}"));
    let alerts: Vec<&str> = stdout.lines().filter(|l| is_alert(l)).collect();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stderr.trim().is_empty() {
        println!("prose: vale reported: {}", stderr.trim());
    }
    println!("prose: {} alert(s) over {}; advisory, nothing refused", alerts.len(), PATHS.join(" and "));
    Ok(())
}

/// A `--output line` alert: `<path>:<line>:<column>:<check>:<message>`.
fn is_alert(line: &str) -> bool {
    let parts: Vec<&str> = line.splitn(5, ':').collect();
    parts.len() == 5 && [parts[1], parts[2]].iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}
