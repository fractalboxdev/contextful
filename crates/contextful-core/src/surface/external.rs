//! `[[external]]` declarations: work around the store that Admin draws on the workflow
//! canvas and the engine never runs (`surface.visualize.external-step`). A block holds a
//! name, a label and typed references alone, so it carries no kind, command or schedule
//! (`surface.visualize.external-shape`).

use super::SurfaceError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// One typed reference an external step reads or feeds.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ref {
    Table(String),
    Pipeline(String),
    Job(String),
}

impl Ref {
    /// Read `table:<name>`, `pipeline:<id>` or `job:<name>`
    /// (`surface.visualize.external-unbound`).
    pub fn parse(text: &str) -> Result<Ref, SurfaceError> {
        let (kind, name) = text.split_once(':').filter(|(_, n)| !n.trim().is_empty()).ok_or_else(|| {
            SurfaceError::ExternalStepUnbound(format!("`{text}` is no typed reference; write `table:<name>`, `pipeline:<id>` or `job:<name>`"))
        })?;
        match kind {
            "table" => Ok(Ref::Table(name.to_string())),
            "pipeline" => Ok(Ref::Pipeline(name.to_string())),
            "job" => Ok(Ref::Job(name.to_string())),
            _ => Err(SurfaceError::ExternalStepUnbound(format!("`{text}` names kind `{kind}`; a reference is `table:`, `pipeline:` or `job:`"))),
        }
    }

    /// The reference as written.
    pub fn spelled(&self) -> String {
        match self {
            Ref::Table(n) => format!("table:{n}"),
            Ref::Pipeline(n) => format!("pipeline:{n}"),
            Ref::Job(n) => format!("job:{n}"),
        }
    }
}

/// One `[[external]]` block as written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct External {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub outputs: Vec<String>,
}

impl External {
    /// The typed inputs; every entry parsed by [`Ref::parse`].
    pub fn input_refs(&self) -> Result<Vec<Ref>, SurfaceError> {
        self.inputs.iter().map(|r| Ref::parse(r)).collect()
    }

    /// The typed outputs; every entry parsed by [`Ref::parse`].
    pub fn output_refs(&self) -> Result<Vec<Ref>, SurfaceError> {
        self.outputs.iter().map(|r| Ref::parse(r)).collect()
    }
}

/// What a manifest declares that an external step may name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Known {
    /// Produced and standalone tables, and the tables models build.
    pub tables: BTreeSet<String>,
    pub pipelines: BTreeSet<String>,
    pub jobs: BTreeSet<String>,
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    external: Vec<toml::Value>,
}

/// Parse a manifest's `[[external]]` blocks; a key outside the shape refuses
/// (`surface.visualize.external-shape`).
pub fn parse_externals(manifest: &str) -> Result<Vec<External>, SurfaceError> {
    let parsed: Manifest = toml::from_str(manifest).map_err(|e| SurfaceError::Invalid(format!("the manifest does not parse: {e}")))?;
    parsed
        .external
        .into_iter()
        .map(|value| {
            let name = value.get("name").and_then(|v| v.as_str()).unwrap_or("(unnamed)").to_string();
            value.try_into::<External>().map_err(|e| SurfaceError::Invalid(format!("external `{name}`: {e}")))
        })
        .collect()
}

/// Bind every external step against what the manifest declares: each reference names a
/// known table, pipeline or job (`surface.visualize.external-unbound`), and each name
/// stands alone among externals, jobs and pipelines (`surface.visualize.external-collision`).
pub fn bind_externals(externals: &[External], known: &Known) -> Result<(), SurfaceError> {
    let mut names = BTreeSet::new();
    for external in externals {
        let name = &external.name;
        if name.trim().is_empty() {
            return Err(SurfaceError::Invalid("an external step declares an empty `name`".into()));
        }
        if !names.insert(name.as_str()) {
            return Err(SurfaceError::ExternalStepCollision(format!("external `{name}` is declared more than once")));
        }
        if known.jobs.contains(name) {
            return Err(SurfaceError::ExternalStepCollision(format!("external `{name}` shares its name with job `{name}`")));
        }
        if known.pipelines.contains(name) {
            return Err(SurfaceError::ExternalStepCollision(format!("external `{name}` shares its name with pipeline `{name}`")));
        }
        for reference in external.input_refs()?.into_iter().chain(external.output_refs()?) {
            let (set, what) = match &reference {
                Ref::Table(n) => (&known.tables, n),
                Ref::Pipeline(n) => (&known.pipelines, n),
                Ref::Job(n) => (&known.jobs, n),
            };
            if !set.contains(what) {
                return Err(SurfaceError::ExternalStepUnbound(format!("external `{name}` names `{}`, which the manifest does not declare", reference.spelled())));
            }
        }
    }
    Ok(())
}
