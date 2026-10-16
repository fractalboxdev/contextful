//! Local policy diagnosis for published models (`disclosure.set-mode.check-verb`), and
//! the release of a governed model into one derived table (`disclosure.release.release-verb`).

use anyhow::{bail, Result};
use clap::Subcommand;
use contextful_core::disclosure::error::DisclosureError;
use contextful_core::disclosure::suppress::SuppressPolicy;
use contextful_core::pipeline::model::collect_models;
use crate::model_source::StatementFault;
use crate::run::ProjectArgs;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum DisclosureCmd {
    /// Read the local declaration and report policy refusals by model.
    Check {
        #[arg(long, default_value = "contextful.toml")]
        config: PathBuf,
    },
    /// Build a governed model under its disclosure policy and publish one derived table.
    Release {
        /// The declared model carrying `[model.disclosure]`.
        model: String,
        #[command(flatten)]
        project: ProjectArgs,
        /// The project manifest; absent, the project's `contextful.toml`.
        #[arg(long)]
        declaration: Option<PathBuf>,
        /// The site id the published rows carry; replaces the manifest's declaration.
        #[arg(long)]
        site_id: Option<String>,
        /// The environment variable holding the site id; replaces the manifest's declaration.
        #[arg(long)]
        site_id_env: Option<String>,
        /// What the release publishes; an activation handle and a lookalike segment refuse.
        #[arg(long, value_enum, default_value = "table")]
        output: Output,
    },
}

/// The `--output` a release requests (`disclosure.release.lookalike-refusal`).
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Output {
    Table,
    ActivationHandle,
    LookalikeSegment,
}

impl From<Output> for contextful_core::disclosure::release::ReleaseOutput {
    fn from(o: Output) -> Self {
        use contextful_core::disclosure::release::ReleaseOutput;
        match o {
            Output::Table => ReleaseOutput::Table,
            Output::ActivationHandle => ReleaseOutput::ActivationHandle,
            Output::LookalikeSegment => ReleaseOutput::LookalikeSegment,
        }
    }
}

fn column_shaped(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn run(cmd: DisclosureCmd) -> Result<()> {
    match cmd {
        DisclosureCmd::Release { model, project, declaration, site_id, site_id_env, output } => {
            release::run(&model, &project, declaration, site_id, site_id_env, output.into())
        }
        DisclosureCmd::Check { config } => {
            if !config.is_file() {
                bail!("disclosure check: manifest `{}` does not read", config.display());
            }
            let files = crate::project::manifests(&config)?;
            let models = collect_models(&files, &[])?;
            let count = models.iter().filter(|m| m.spec.publishes()).count();
            let mut refusals = Vec::new();
            for model in models.into_iter().filter(|m| m.spec.publishes()) {
                let mut spec = model.spec;
                if let Err(error) = crate::model_source::resolve(&mut spec, &model.file) {
                    let refusal = match error {
                        StatementFault::Unreadable { .. } => DisclosureError::ModelUnreadable(error.to_string()).to_string(),
                        StatementFault::Declaration(_) => error.to_string(),
                    };
                    refusals.push(format!("{}: {refusal}", spec.id));
                    continue;
                }
                let aggregate = match contextful_context::read::aggregate_shape(&spec.sql) {
                    Ok(aggregate) => aggregate,
                    Err(error) => {
                        refusals.push(format!("{}: {error}", spec.id));
                        continue;
                    }
                };
                let Some(policy) = spec.disclosure else {
                    if aggregate && spec.disclosure_opt_out.is_none() {
                        refusals.push(format!("{}: {}", spec.id, DisclosureError::PolicyAbsent("aggregate-shaped model declares no `[model.disclosure]` or `disclosure_opt_out`".into())));
                    }
                    continue;
                };
                if policy.grouping_allowlist.is_empty() || policy.grouping_allowlist.iter().any(|name| !column_shaped(name)) {
                    refusals.push(format!("{}: {}", spec.id, DisclosureError::GroupingAllowlistEmpty("permitted grouping columns must be nonempty column names".into())));
                }
                if let Err(error) = SuppressPolicy::new(policy.min_group_size, policy.max_contributor_share) {
                    refusals.push(format!("{}: {error}", spec.id));
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

mod release {
    //! `contextful disclosure release`: a thin adapter. It resolves the model, its spend and
    //! the project's cap, opens the project ledger, and runs the model build with the core
    //! release as its step. No disclosure rule lives here.

    use crate::project::open_face;
    use crate::run::{site_id_for, ProjectArgs};
    use anyhow::{Context, Result};
    use contextful_context::build::{build_released, BuildRequest, OsEntropy};
    use contextful_core::disclosure::error::DisclosureError;
    use contextful_core::disclosure::noise::{Entropy, Spend};
    use contextful_core::disclosure::release::{release, spend_of, Breakdown, BudgetLedger, ReleaseError, ReleaseOutput, ReleaseRequest};
    use contextful_core::pipeline::model::project_disclosure;
    use contextful_policy::enforce::mask::Pepper;
    use contextful_sqlite::SqliteBudgetLedger;
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    /// The project ledger under the declared cap: each unit is recorded under its key's
    /// keyed digest under the project's pepper and granted the cap before its spend is
    /// reserved (`disclosure.release.lifetime-cap`, `disclosure.release.budget-ledger`).
    struct DeclaredLedger {
        ledger: SqliteBudgetLedger,
        cap: u64,
        pepper: Pepper,
    }

    impl BudgetLedger for DeclaredLedger {
        fn reserve(&self, release_id: &str, units: &BTreeSet<String>, spend: Spend) -> std::result::Result<(), ReleaseError> {
            let digests: BTreeSet<String> = units.iter().map(|u| self.pepper.digest(u)).collect();
            for digest in &digests {
                self.ledger.grant(digest, self.cap).map_err(ReleaseError::Ledger)?;
            }
            self.ledger.reserve(release_id, &digests, spend)
        }
    }

    /// `.contextful/disclosure/<project>/budget.sqlite`, outside the synced store root.
    fn ledger_path(dir: &Path, project: &str) -> PathBuf {
        dir.join(".contextful/disclosure").join(project).join("budget.sqlite")
    }

    pub fn run(id: &str, project: &ProjectArgs, declaration: Option<PathBuf>, site_id: Option<String>, site_id_env: Option<String>, output: ReleaseOutput) -> Result<()> {
        output.admit()?;
        let l = project.locate(declaration)?;
        let spec = crate::build::buildable(&l.declaration, id)?;
        let Some(policy) = spec.disclosure.clone() else {
            return Err(DisclosureError::PolicyAbsent(format!("model `{id}` declares no `[model.disclosure]` to release under")).into());
        };
        let spend = spend_of(&policy)?;
        let files = crate::pipeline::manifests(&l.declaration)?;
        let cap = project_disclosure(&files)?.cap_micros();
        let text = if l.declaration.exists() { std::fs::read_to_string(&l.declaration)? } else { String::new() };
        let grouped = crate::export::publication::validate_job(&text, id, &[id.to_string()])?;
        let _publication = if grouped { Some(crate::export::publication::lock(&l)?) } else { None };
        let site_id = site_id_for(&text, &l.declaration, site_id, site_id_env)?;
        let started_at = crate::build::now(&project.now)?;
        let completed_at = match &project.now {
            Some(_) => started_at,
            None => crate::build::now(&None)?.max(started_at),
        };
        let face = open_face(&l.project, &l.declaration, &text, Pepper::resolve(|k| std::env::var(k).ok()))
            .with_context(|| format!("opening the read face over `{}`", l.declaration.display()))?;
        let ledger = DeclaredLedger {
            ledger: SqliteBudgetLedger::open(&ledger_path(&l.project.dir, &l.project.name))?,
            cap,
            pepper: Pepper::resolve(|k| std::env::var(k).ok()),
        };
        let entropy = RefCell::new(OsEntropy::new()?);
        let outcome = RefCell::new(None);
        // The ledger is project-wide and a build id is unique only within one model's chain,
        // so each invocation reserves under a fresh nonce: no release replays another's.
        let step = |build_id: &str, breakdown: Breakdown| {
            let nonce = entropy.borrow_mut().next_u64();
            let request = ReleaseRequest { release_id: format!("{}/{build_id}/{nonce:016x}", spec.id), table: spec.id.clone(), spend };
            let released = release(&policy, &breakdown, &request, &ledger, &mut *entropy.borrow_mut())?;
            let table = released.table.clone();
            *outcome.borrow_mut() = Some(released);
            Ok(table)
        };
        let built = build_released(&face, &BuildRequest { model: &spec, site_id: &site_id, started_at, completed_at, available_as_of: None }, &step)?;
        if grouped {
            crate::export::publication::record_completion(face.store(), &text, id, &built.build_id, serde_json::to_value(&built.watermark)?)?;
        }
        let released = outcome.into_inner().context("the release step ran")?;
        println!(
            "{}",
            serde_json::json!({
                "table": released.table.name,
                "columns": released.table.columns,
                "rows": released.table.rows,
                "envelope": released.envelope,
            })
        );
        Ok(())
    }
}
