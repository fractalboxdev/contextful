//! `disclosure.bound-cohort` on the read face: a `cohort` model's source tables answer only
//! a grant holding `read-individual`, and a read grouping a cohort table by a strict subset
//! of its released group key refuses.

use super::engine::SqlEngine;
use super::fault::ReadFault;
use super::shape::cohort_widening;
use super::Face;
use contextful_core::disclosure::DisclosureError;
use contextful_core::grant::individual_read_covers;
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

    /// Refuse a read touching a `cohort` model's source table through no grant holding
    /// `read-individual` over it (`disclosure.bound-cohort.per-individual-row`).
    pub(crate) fn bind_cohort_sources(&self, session: &Session, touched: &BTreeSet<String>) -> Result<(), ReadFault> {
        let source = |t: &&String| self.cohorts.values().any(|sources| sources.contains(*t));
        match touched.iter().filter(source).find(|t| !individual_read_covers(session.grants(), t)) {
            Some(table) => Err(ReadFault::Refused(
                DisclosureError::PerIndividualUngranted(format!(
                    "table `{table}` is a `cohort` model's source; reading its rows needs a grant holding `read-individual` over it"
                ))
                .into(),
            )),
            None => Ok(()),
        }
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
