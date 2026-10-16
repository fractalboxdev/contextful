//! `contextful schema export`: the JSON Schemas derived from the pipeline specification and
//! the plan types, written where every language binding reads them
//! (`run.declare.serialization`, `run.compile.plan-schema`).

use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::{Path, PathBuf};

/// Where `export` writes absent `--out`, under the nearest directory upward holding
/// `contextful.toml`, else the working directory.
pub const SCHEMA_DIR: &str = ".contextful/schema";

#[derive(Subcommand)]
pub enum SchemaCmd {
    /// Write `pipeline.json` and `plan.json`, each a JSON Schema derived from the type the
    /// manifest reader and the run path deserialize into.
    Export {
        /// The directory receiving the schemas; absent, `.contextful/schema` under the project root.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

pub fn run(cmd: SchemaCmd) -> Result<()> {
    match cmd {
        SchemaCmd::Export { out } => {
            let out = match out {
                Some(out) => out,
                None => crate::root::root(None)?.join(SCHEMA_DIR),
            };
            std::fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
            for (name, schema) in [("pipeline.json", contextful_core::pipeline::declare::pipeline_schema()), ("plan.json", contextful_core::run::nodes::plan_schema())] {
                let path = out.join(name);
                write(&path, &schema)?;
                println!("wrote {}", path.display());
            }
            Ok(())
        }
    }
}

fn write(path: &Path, schema: &serde_json::Value) -> Result<()> {
    let mut text = serde_json::to_string_pretty(schema)?;
    text.push('\n');
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}
