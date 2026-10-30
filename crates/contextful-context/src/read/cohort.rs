//! `disclosure.bound-cohort` on the read face: a `cohort` model's source tables, and every
//! derive output of one, answer only a grant holding `read-individual`, and a read grouping a
//! cohort table by a strict subset of its released group key refuses.

use super::engine::SqlEngine;
use super::fault::ReadFault;
use super::shape::cohort_widening;
use super::Face;
use contextful_core::disclosure::DisclosureError;
use contextful_core::grant::{individual_read_covers, Grant};
use contextful_core::pipeline::model::{Governance, ModelSpec};
use contextful_core::read::guard::named_relations;
use contextful_policy::enforce::session::Session;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

impl Face {
    /// The face knowing each `cohort` model among `models`, each statement resolved: its
    /// table and the tables its statement names. A statement that does not parse refuses
    /// the face, since its source tables are then unknown.
    pub fn with_cohorts(mut self, models: &[ModelSpec]) -> Result<Face, ReadFault> {
        let cohort = |m: &&ModelSpec| m.disclosure.as_ref().is_some_and(|d| d.governance() == Governance::Cohort);
        for model in models.iter().filter(cohort) {
            let tree = self.serialize(&model.sql)?;
            if tree.get("error").and_then(Value::as_bool) == Some(true) {
                return Err(ReadFault::Engine(format!("`cohort` model `{}`: its statement does not parse", model.id)));
            }
            let sources = named_relations(&tree).into_iter().filter(|t| *t != model.id).collect();
            self.cohorts.insert(model.id.clone(), sources);
        }
        Ok(self)
    }

    /// The face knowing each derive pipeline's parent and joined tables beside its output
    /// tables, so an output of a per-individual table is itself per-individual
    /// (`disclosure.bound-cohort.derived-individual`).
    pub fn with_derived(mut self, derives: &[(Vec<String>, Vec<String>)]) -> Face {
        self.derives = derives.to_vec();
        self
    }

    /// Every per-individual table: each table a `cohort` model's statement names, then each
    /// output of a derive reading one, to a fixed point.
    pub fn individual_tables(&self) -> BTreeSet<String> {
        let mut tables: BTreeSet<String> = self.cohorts.values().flatten().cloned().collect();
        loop {
            let reached: Vec<&String> = self.derives.iter()
                .filter(|(parents, _)| parents.iter().any(|p| tables.contains(p)))
                .flat_map(|(_, outputs)| outputs)
                .filter(|o| !tables.contains(*o))
                .collect();
            if reached.is_empty() {
                return tables;
            }
            tables.extend(reached.into_iter().cloned());
        }
    }

    /// Refuse reading a per-individual table among `tables` under `grants` holding no
    /// `read-individual` over it; `None`, a read under no credential, holds none
    /// (`disclosure.bound-cohort.per-individual-row`).
    pub fn bind_individual<'t>(&self, grants: Option<&[Grant]>, tables: impl IntoIterator<Item = &'t String>) -> Result<(), ReadFault> {
        let individual = self.individual_tables();
        let ungranted = |t: &&String| individual.contains(*t) && !grants.is_some_and(|g| individual_read_covers(g, t));
        match tables.into_iter().find(ungranted) {
            Some(table) => Err(ReadFault::Refused(
                DisclosureError::PerIndividualUngranted(format!(
                    "table `{table}` holds a `cohort` model's per-individual rows; reading them needs a grant holding `read-individual` over it"
                ))
                .into(),
            )),
            None => Ok(()),
        }
    }

    /// [`Face::bind_individual`] under the session's grants.
    pub(crate) fn bind_cohort_sources(&self, session: &Session, touched: &BTreeSet<String>) -> Result<(), ReadFault> {
        self.bind_individual(Some(session.grants()), touched)
    }

    /// [`Face::bind_cohort_sources`], then refuse a statement grouping a touched cohort table
    /// by a strict subset of the group key its published release records
    /// (`disclosure.bound-cohort.cohort-widening`). A cohort table with no release holds no
    /// row to group.
    pub(crate) fn bind_cohorts(&self, engine: &SqlEngine, session: &Session, touched: &BTreeSet<String>, tree: &Value) -> Result<(), ReadFault> {
        self.bind_cohort_sources(session, touched)?;
        let mut keys = BTreeMap::new();
        for table in touched.iter().filter(|t| self.cohorts.contains_key(*t)) {
            if let Some(release) = self.store.current_snapshot(table)? {
                keys.insert(table.clone(), release.primary_key.into_iter().collect::<BTreeSet<String>>());
            }
        }
        if keys.is_empty() {
            return Ok(());
        }
        match cohort_widening(tree, &keys, &engine.aggregate_functions()?) {
            Some((table, missing)) => Err(ReadFault::Refused(
                DisclosureError::CohortWidening(format!(
                    "the statement groups cohort table `{table}` without its group key column `{missing}`, merging its released groups"
                ))
                .into(),
            )),
            None => Ok(()),
        }
    }
}
