//! Applied executable declarations, separate from live runtime and writer authority.

use anyhow::{Context, Result};
use contextful_context::{read::Face, Store};
use contextful_core::memory::declare::MemoryDeclarations;
use contextful_core::pipeline::declare::{collect, ManifestFile, PipelineSpec};
use contextful_core::pipeline::model::{collect_models, ModelSpec};
use contextful_core::run::journal::sha256_hex;
use contextful_core::store::declare::TableDecl;
use contextful_core::surface::SurfaceError;
use contextful_policy::enforce::mask::Pepper;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// Private snapshot field; public declarations retain their existing table syntax.
pub(crate) fn snapshot_tables(text: &str) -> Result<Option<Vec<TableDecl>>> {
    #[derive(Deserialize)]
    struct Snapshot { standalone_tables: Option<Vec<TableDecl>> }
    Ok(toml::from_str::<Snapshot>(text)?.standalone_tables)
}

/// Adapt an internal snapshot for the strict public manifest parser. Only the
/// typed snapshot extension is removed; unknown public blocks still refuse.
pub(crate) fn public_manifest(mut file: ManifestFile) -> Result<ManifestFile> {
    snapshot_tables(&file.text)?;
    let mut document: toml::Value = toml::from_str(&file.text)?;
    if document.as_table_mut().context("snapshot is a table")?.remove("standalone_tables").is_some() {
        file.text = toml::to_string(&document)?;
    }
    Ok(file)
}

/// Standalone legacy declarations are independent of named source specifications.
pub(crate) fn standalone_tables(files: &[ManifestFile]) -> Result<Vec<TableDecl>> {
    let mut tables = Vec::new();
    for file in files.iter().filter(|f| !f.path.ends_with(".json")) {
        let document: toml::Value = toml::from_str(&file.text)?;
        if matches!(document.get("pipeline"), Some(toml::Value::Table(_))) {
            tables.extend(TableDecl::parse_pipeline(&file.text)?);
        }
    }
    unique(&mut tables)?;
    Ok(tables)
}

fn unique(tables: &mut [TableDecl]) -> Result<()> {
    tables.sort_by(|a, b| a.name.cmp(&b.name));
    for pair in tables.windows(2) {
        if pair[0].name == pair[1].name {
            return Err(SurfaceError::ApplyValidationRefused(format!("table `{}` is declared more than once", pair[0].name)).into());
        }
    }
    Ok(())
}

/// Rehydrate the two existing public declaration shapes without mixing their TOML types.
pub(crate) fn snapshot_parts(local: &str, snapshot: &str) -> Result<(String, Vec<ManifestFile>)> {
    let tables = snapshot_tables(snapshot)?.ok_or_else(|| SurfaceError::ApplyValidationRefused("applied snapshot lacks standalone table contracts; run a full pipeline apply before firing jobs".into()))?;
    let snapshot: toml::Value = toml::from_str(snapshot)?;
    let mut main: toml::Value = toml::from_str(local)?;
    let main_table = main.as_table_mut().context("main declaration is a table")?;
    for key in ["pipeline", "model", "job", "standalone_tables"] { main_table.remove(key); }
    main_table.insert("pipeline".into(), toml::Value::Table(toml::map::Map::from_iter([("tables".into(), toml::Value::try_from(&tables)?)])));
    for key in ["model", "job"] {
        if let Some(value) = snapshot.get(key) { main_table.insert(key.into(), value.clone()); }
    }
    let pipelines = toml::Value::Table(toml::map::Map::from_iter([("pipeline".into(), snapshot.get("pipeline").cloned().unwrap_or_else(|| toml::Value::Array(Vec::new())))]));
    Ok((toml::to_string(&main)?, vec![ManifestFile { path: "applied-pipelines.toml".into(), text: toml::to_string(&pipelines)? }]))
}

pub(crate) fn validate_snapshot_tables(snapshot: &str) -> Result<()> {
    if let Some(mut standalone) = snapshot_tables(snapshot)? {
        unique(&mut standalone)?;
        let (main, files) = snapshot_parts("", snapshot)?;
        let mut tables = TableDecl::parse_declaration_set(&main, &files)?;
        let jobs = contextful_core::job::parse_jobs(snapshot, &|_| true)?;
        if jobs.iter().any(|job| matches!(job.kind_name(), "build" | "store-driven")) {
            unique(&mut tables)?;
        }
    }
    Ok(())
}

pub(crate) struct Effective {
    pub main: String,
    files: Vec<ManifestFile>,
    pub specs: Vec<PipelineSpec>,
    pub models: Vec<ModelSpec>,
    pub tables: Vec<TableDecl>,
    applied: bool,
}

impl Effective {
    pub fn resolve(located: &crate::project::Located, local: &str, applied: Option<u64>) -> Result<Self> {
        let (main, files, specs, models) = if let Some(version) = applied {
            let snapshot = crate::cadence::snapshot_manifest(&located.project, local, version)?;
            let public = public_manifest(snapshot.clone())?;
            let specs = collect(std::slice::from_ref(&public))?.into_iter().map(|p| p.spec).collect();
            let models = crate::model_source::applied(&snapshot)?;
            let (main, files) = snapshot_parts(local, &snapshot.text)?;
            (main, files, specs, models)
        } else {
            let all = crate::project::manifests(&located.declaration)?;
            let specs = collect(&all)?.into_iter().map(|p| p.spec).collect();
            let models = crate::model_source::collect(&all)?;
            (local.to_string(), crate::project::pipeline_files(&located.declaration)?, specs, models)
        };
        let mut tables = TableDecl::parse_declaration_set(&main, &files)?;
        unique(&mut tables)?;
        Ok(Self { main, files, specs, models, tables, applied: applied.is_some() })
    }

    /// Static declaration identity; newly landed data does not change a pending owner's pin.
    pub fn structural_identity(&self, outputs: &[String]) -> Result<String> {
        let memory = MemoryDeclarations::parse(&self.main)?;
        let mut shaped: Vec<_> = memory.tables.iter().map(|t| (&t.name, t.shape.name(), &t.columns)).collect();
        shaped.sort_by(|a, b| a.0.cmp(b.0));
        Ok(sha256_hex(&serde_json::to_vec(&("store-structure-v1", &self.tables, &self.models, outputs, shaped, memory.declared_relations))?))
    }

    /// Current authority must agree before an applied structure can serve a new attempt.
    pub fn face(&self, located: &crate::project::Located, local: &str) -> Result<Face> {
        let pepper = || Pepper::resolve(|k| std::env::var(k).ok());
        let store = Store::open(&located.project.dir, &located.project.name)?;
        let face = Face::open_declared(store.clone(), &self.main, &self.files, pepper())?;
        if self.applied {
            let live_files = crate::project::pipeline_files(&located.declaration)?;
            let live = Face::open_declared(store, local, &live_files, pepper())?;
            let all = crate::project::manifests(&located.declaration)?;
            let current_models = collect_models(&all, &collect(&all)?)?;
            let mut names: BTreeSet<_> = live.tables()?.into_iter().chain(face.tables()?).collect();
            names.extend(self.models.iter().map(|m| m.id.clone()));
            names.extend(current_models.iter().map(|m| m.spec.id.clone()));
            for name in names {
                if authority(&face.decl(&name))? != authority(&live.decl(&name))? {
                    return Err(SurfaceError::ApplyValidationRefused(format!("current table `{name}` authority differs from the applied snapshot; reapply before starting a new execution")).into());
                }
            }
            let expected: BTreeMap<_, _> = self.models.iter().map(|m| (m.id.as_str(), (&m.disclosure, &m.disclosure_opt_out))).collect();
            let current: BTreeMap<_, _> = current_models.iter().map(|m| (m.spec.id.as_str(), (&m.spec.disclosure, &m.spec.disclosure_opt_out))).collect();
            for id in expected.keys().chain(current.keys()).collect::<BTreeSet<_>>() {
                let empty = (&None, &None);
                if expected.get(id).unwrap_or(&empty) != current.get(id).unwrap_or(&empty) {
                    return Err(SurfaceError::ApplyValidationRefused(format!("current model `{id}` disclosure authority differs from the applied snapshot; reapply before starting a new execution")).into());
                }
            }
        }
        Ok(face)
    }
}

/// Unknown future fields remain checked; only these executable structure fields may drift.
fn authority(declaration: &TableDecl) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(declaration)?;
    let table = value.as_object_mut().context("table declaration serializes as an object")?;
    for key in ["primary_key", "order_by", "write_mode", "columns"] { table.remove(key); }
    Ok(value)
}
