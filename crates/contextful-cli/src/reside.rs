//! `surface.reside`: the project's `[residency] regions` allow-set, and the regions its
//! configured resources resolve to, held against it before anything serves.

use crate::project::{manifests, Located};
use anyhow::{Context, Result};
use contextful_core::pipeline::declare::{collect, SourceBlock};
use contextful_core::store::sync::Endpoint;
use contextful_core::surface::reside::{Residency, Resource};

/// The project manifest's `[residency] regions`, `None` when it declares no `[residency]`.
pub(crate) fn declared(text: &str) -> Result<Option<Residency>> {
    let value: toml::Value = if text.trim().is_empty() { toml::Value::Table(Default::default()) } else { toml::from_str(text)? };
    let Some(block) = value.get("residency") else { return Ok(None) };
    let regions = block.get("regions").context("`[residency]` declares `regions`, a list of regions")?;
    let regions: Vec<String> = regions
        .as_array()
        .context("`[residency] regions` is a list of strings")?
        .iter()
        .map(|v| v.as_str().map(str::to_string).context("`[residency] regions` is a list of strings"))
        .collect::<Result<_>>()?;
    Ok(Some(Residency::parse(&regions)?))
}

/// The region a source or destination block's config resolves to: its `region`, or the
/// region an `s3://<region>` endpoint names.
fn block_region(block: &SourceBlock) -> Option<String> {
    let config = block.config.as_object()?;
    if let Some(r) = config.get("region").and_then(|v| v.as_str()) {
        return Some(r.to_string());
    }
    config.get("endpoint").and_then(|v| v.as_str()).and_then(|e| e.strip_prefix("s3://")).map(|r| r.trim_end_matches('/').to_string())
}

/// Every configured resource of the project that resolves to a region: the store's
/// `[sync]` bucket and each pipeline source or destination naming one.
fn resources(l: &Located) -> Result<Vec<Resource>> {
    let mut out = Vec::new();
    if let (Some(sync), _) = crate::sync::sync_config(l)? {
        if let Endpoint::S3 { region, .. } = sync.resolve_endpoint()? {
            out.push(Resource { name: "the `[sync]` bucket".into(), region });
        }
    }
    for d in collect(&manifests(&l.declaration)?)? {
        let spec = d.spec;
        if let Some(region) = block_region(&spec.source) {
            out.push(Resource { name: format!("pipeline `{}` source", spec.id), region });
        }
        if let Some(region) = spec.destination.as_ref().and_then(block_region) {
            out.push(Resource { name: format!("pipeline `{}` destination", spec.id), region });
        }
    }
    Ok(out)
}

/// Refuse the start when a configured resource resolves outside the allow-set
/// (`surface.reside.region-mismatch`). A project declaring no `[residency]` places anywhere.
pub(crate) fn enforce(l: &Located, text: &str) -> Result<()> {
    let Some(residency) = declared(text)? else { return Ok(()) };
    residency.enforce(&resources(l)?)?;
    Ok(())
}
