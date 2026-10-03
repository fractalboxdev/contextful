//! Local policy diagnosis for published models (`disclosure.set-mode.check-verb`).

use anyhow::{bail, Result};
use clap::Subcommand;
use contextful_core::disclosure::error::DisclosureError;
use contextful_core::disclosure::suppress::SuppressPolicy;
use contextful_core::pipeline::model::collect_models;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum DisclosureCmd {
    /// Read the local declaration and report policy refusals by model.
    Check {
        #[arg(long, default_value = "contextful.toml")]
        config: PathBuf,
    },
}

fn column_shaped(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn run(cmd: DisclosureCmd) -> Result<()> {
    match cmd {
        DisclosureCmd::Check { config } => {
            if !config.is_file() {
                bail!("disclosure check: manifest `{}` does not read", config.display());
            }
            let files = crate::project::manifests(&config)?;
            let models = collect_models(&files, &[])?;
            let count = models.iter().filter(|m| m.spec.publishes()).count();
            let mut refusals = Vec::new();
            for model in models.into_iter().filter(|m| m.spec.publishes()) {
                let Some(policy) = model.spec.disclosure else { continue };
                if policy.grouping_allowlist.is_empty() || policy.grouping_allowlist.iter().any(|name| !column_shaped(name)) {
                    refusals.push(format!("{}: {}", model.spec.id, DisclosureError::GroupingAllowlistEmpty("permitted grouping columns must be nonempty column names".into())));
                }
                if let Err(error) = SuppressPolicy::new(policy.min_group_size, policy.max_contributor_share) {
                    refusals.push(format!("{}: {error}", model.spec.id));
                }
            }
            if !refusals.is_empty() {
                bail!("{}", refusals.join("\n"));
            }
            println!("disclosure check: {count} published models inspected");
            Ok(())
        }
    }
}
