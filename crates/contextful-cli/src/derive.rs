//! `contextful derive` — one derive engine outside a pipeline.

use anyhow::{Context, Result};
use clap::Subcommand;
use contextful_connectors::derive::{run_chain, Chain};
use contextful_core::run::derive::config::bindings;
use contextful_core::run::derive::cues::{parse, passages};
use contextful_core::run::RunError;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum DeriveCmd {
    /// Build one engine from its `[derive.<name>]` block and run it against one file,
    /// printing the passages it yields as JSON lines.
    TestEngine {
        name: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long, default_value = "contextful.toml")]
        declaration: PathBuf,
    },
}

struct Never;

impl contextful_core::run::ports::Cancellation for Never {
    fn requested(&self) -> bool {
        false
    }
}

pub fn run(cmd: DeriveCmd) -> Result<()> {
    match cmd {
        DeriveCmd::TestEngine { name, file, declaration } => {
            let text = std::fs::read_to_string(&declaration).with_context(|| format!("reading `{}`", declaration.display()))?;
            let all = bindings(&text)?;
            let binding = all.get(&name).ok_or_else(|| {
                RunError::DeriveEngineUnbound(format!(
                    "`{}` defines no `[derive.{name}]` block; bound engines: {}",
                    declaration.display(),
                    all.keys().cloned().collect::<Vec<_>>().join(", ")
                ))
            })?;
            if binding.driver == "fetch" {
                return Err(RunError::DeriveTestEngineUnsupported(format!(
                    "engine `{name}` runs the `fetch` driver; the verb drives a transcriber over one media file"
                ))
                .into());
            }
            let cwd = std::env::current_dir()?;
            let chain = Chain::resolve(&name, binding, &cwd)?;
            let vars: BTreeMap<String, String> = std::env::vars().collect();
            let resolver = contextful_runtime::assemble(&vars, std::sync::Arc::new(crate::run::SystemClock))?;
            let mut env = Vec::new();
            for (k, t) in &chain.env {
                env.push((k.clone(), resolver.render(t)?.reveal().to_string()));
            }
            let scratch = tempfile_dir()?;
            let doc = run_chain(&chain, &cwd.join(&file), &env, &scratch, &Never);
            let _ = std::fs::remove_dir_all(&scratch);
            let parsed = parse(&doc?);
            for d in &parsed.defects {
                eprintln!("{d}");
            }
            eprintln!("{}", chain.id);
            for p in passages(&parsed.cues) {
                println!("{}", serde_json::json!({ "start_ms": p.start_ms, "end_ms": p.end_ms, "text": p.text }));
            }
            Ok(())
        }
    }
}

fn tempfile_dir() -> Result<PathBuf> {
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).map_err(|e| anyhow::anyhow!("{e}"))?;
    let dir = std::env::temp_dir().join(format!("contextful-test-engine-{}", nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
