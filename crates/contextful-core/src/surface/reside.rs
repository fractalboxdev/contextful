//! The residency allow-set (`surface.reside`): the regions a data plane's configured
//! resources may resolve to, checked before anything serves, and the set each site records
//! in the bucket manifest it pushes to.

use super::SurfaceError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Entries a residency allow-set holds at most (`surface.reside.region-entries`).
pub const REGION_ENTRIES: usize = 16;

/// The entry admitting every region.
pub const ANY_REGION: &str = "*";

/// A parsed `[residency] regions` allow-set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Residency {
    regions: BTreeSet<String>,
}

/// One configured resource and the region it resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    /// Where the resource is configured, such as `[sync] endpoint` or `pipeline orders source`.
    pub name: String,
    pub region: String,
}

/// The allow-set a site records beside its pushes: the site id and its sorted entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteRegions {
    pub site_id: String,
    pub regions: Vec<String>,
}

/// One region label: `[a-z0-9-]+`, at most 63 chars.
fn label(s: &str) -> bool {
    !s.is_empty() && s.len() <= 63 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl Residency {
    /// Read an allow-set of at most [`REGION_ENTRIES`] entries, each a region label or `*`.
    pub fn parse(entries: &[String]) -> Result<Residency, SurfaceError> {
        if entries.len() > REGION_ENTRIES {
            return Err(SurfaceError::Invalid(format!(
                "`[residency] regions` holds {} entries; an allow-set holds at most {REGION_ENTRIES}",
                entries.len()
            )));
        }
        let mut regions = BTreeSet::new();
        for e in entries {
            if e != ANY_REGION && !label(e) {
                return Err(SurfaceError::Invalid(format!("`[residency] regions` entry `{e}` is neither a region such as `eu-west-1` nor `*`")));
            }
            regions.insert(e.clone());
        }
        Ok(Residency { regions })
    }

    pub fn admits(&self, region: &str) -> bool {
        self.regions.contains(ANY_REGION) || self.regions.contains(region)
    }

    /// Refuse the start when any resource resolves outside the allow-set, naming every such
    /// resource (`surface.reside.region-mismatch`).
    pub fn enforce(&self, resources: &[Resource]) -> Result<(), SurfaceError> {
        let outside: Vec<String> = resources.iter().filter(|r| !self.admits(&r.region)).map(|r| format!("{} resolves to `{}`", r.name, r.region)).collect();
        if outside.is_empty() {
            return Ok(());
        }
        Err(SurfaceError::EnforceRegionMismatch(format!(
            "{}; `[residency] regions` admits [{}], so nothing serves",
            outside.join(", "),
            self.entries().join(", ")
        )))
    }

    /// The entries, sorted.
    pub fn entries(&self) -> Vec<String> {
        self.regions.iter().cloned().collect()
    }
}

/// Hold a push to the allow-set the bucket manifest records. `own` is the pushing site's
/// allow-set, `None` when it declares no `[residency]`. A set recorded by another site that
/// differs from `own`, a missing one included, raises `ResidencySitesDiverge`, naming both
/// sites and both sets (`surface.reside.site-regions`). A record the pushing site wrote
/// itself passes, so the site holding the record changes the policy first.
pub fn compare_sites(site_id: &str, own: Option<&[String]>, recorded: Option<&SiteRegions>) -> Result<(), SurfaceError> {
    let Some(r) = recorded else { return Ok(()) };
    if r.site_id == site_id || own == Some(r.regions.as_slice()) {
        return Ok(());
    }
    let shown = own.map(|o| format!("regions [{}]", o.join(", "))).unwrap_or_else(|| "no `[residency]`".into());
    Err(SurfaceError::ResidencySitesDiverge(format!(
        "site `{site_id}` declares {shown} and the bucket records regions [{}] from site `{}`; one residency policy governs one bucket, and a policy change starts at site `{}`",
        r.regions.join(", "),
        r.site_id,
        r.site_id
    )))
}
