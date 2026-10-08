//! Closed typed projections and owner-scoped identity for recorded body effects.

use crate::pipeline::transform::Chain;
use crate::pipeline::transform::TransformOp;
use crate::run::journal::{sha256_hex, EntryKey};
use crate::run::ports::Types;
use crate::run::ports::{Row, Shape};
use crate::run::RunError;
use crate::run::{Failure, FailureTag};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A compiled body's exact output projection for one recorded effect label.
#[derive(Debug, Clone)]
pub struct RecordedEffect {
    pub label: String,
    pub table: String,
    pub types: Types,
    pub transforms: Vec<TransformOp>,
    pub normalize: Option<crate::pipeline::normalize::Normalize>,
}

/// A closed effect set admitted against the job's declared output roots.
#[derive(Debug, Clone)]
pub struct RecordedBodyPlan {
    effects: BTreeMap<String, RecordedEffect>,
    identity: String,
}

impl RecordedBodyPlan {
    pub fn compile(effects: Vec<RecordedEffect>, outputs: &[String]) -> Result<Self, RunError> {
        let outputs: BTreeSet<_> = outputs.iter().map(String::as_str).collect();
        let mut admitted = BTreeMap::new();
        for effect in effects {
            if effect.label.trim().is_empty() || !outputs.contains(effect.table.as_str()) {
                return Err(RunError::Invalid(
                    "a recorded effect names a nonempty label and declared output root".into(),
                ));
            }
            for transform in &effect.transforms {
                transform.validate()?;
            }
            if admitted.values().any(|prior: &RecordedEffect| prior.table == effect.table && prior.normalize != effect.normalize) {
                return Err(RunError::Invalid("recorded effects for one output root share its normalization".into()));
            }
            if admitted.insert(effect.label.clone(), effect).is_some() {
                return Err(RunError::Invalid(
                    "a recorded body declares each effect label once".into(),
                ));
            }
        }
        let covered: BTreeSet<_> = admitted
            .values()
            .map(|effect| effect.table.as_str())
            .collect();
        if covered != outputs {
            return Err(RunError::Invalid(
                "a recorded body binds every declared output root".into(),
            ));
        }
        let canonical: Vec<_> = admitted
            .values()
            .map(|effect| {
                let types: BTreeMap<_, _> = effect
                    .types
                    .iter()
                    .map(|(column, ty)| (column, ty.name()))
                    .collect();
                (&effect.label, &effect.table, types, &effect.transforms, effect.normalize.map(|n| n.recording_projection()))
            })
            .collect();
        let bytes = serde_json::to_vec(&("recorded-body-projection-v1", canonical))
            .map_err(|e| RunError::Invalid(e.to_string()))?;
        Ok(Self {
            effects: admitted,
            identity: sha256_hex(&bytes),
        })
    }

    pub fn identity(&self) -> String {
        self.identity.clone()
    }

    pub fn effect(&self, label: &str) -> Result<&RecordedEffect, RunError> {
        self.effects
            .get(label)
            .ok_or_else(|| RunError::Invalid(format!("recorded effect `{label}` is undeclared")))
    }

    pub fn effects(&self) -> impl Iterator<Item = &RecordedEffect> {
        self.effects.values()
    }
}

/// A body effect's stable execution, row/label/input key and composite owner identity.
/// This request conveys no admission; the canonical writer signs and checks it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectScope {
    key: EntryKey,
    plan_identity: String,
}

impl EffectScope {
    pub fn new(key: &EntryKey, plan_identity: &str) -> Self {
        Self {
            key: key.clone(),
            plan_identity: plan_identity.into(),
        }
    }

    pub fn key(&self) -> &EntryKey {
        &self.key
    }
    pub fn plan_identity(&self) -> &str {
        &self.plan_identity
    }
}

/// Concrete root metadata derived by canonical payload admission, never raw effect bytes.
#[derive(Debug, Clone)]
pub struct EmissionSummary {
    pub rows: u64,
    pub columns: BTreeSet<String>,
    pub types: Types,
}

/// Canonical preparation and MAC admission, without journal or execution state.
pub trait EffectAdmission: Send + Sync {
    fn identity(&self, effect: &RecordedEffect) -> Result<String, Failure>;
    fn prepare(
        &self,
        effect: &RecordedEffect,
        rows: Vec<Row>,
        types: Types,
        scope: &EffectScope,
    ) -> Result<serde_json::Value, Failure>;
    fn admit(
        &self,
        effect: &RecordedEffect,
        scope: &EffectScope,
        payload: &serde_json::Value,
    ) -> Result<EmissionSummary, Failure>;
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EffectWire {
    version: u8,
    label: String,
    table: String,
    scope: EffectScope,
    authority: String,
    payload: serde_json::Value,
}

/// An opaque effect result; preparation or owner-bound canonical replay constructs it.
#[derive(Clone)]
pub struct PreparedEmission {
    wire: EffectWire,
    summary: EmissionSummary,
    masked_cells: BTreeMap<String, usize>,
}

fn refused(message: impl ToString) -> Failure {
    Failure::deterministic(FailureTag::Permanent, message.to_string())
}

impl PreparedEmission {
    pub fn prepare(
        plan: &RecordedBodyPlan,
        label: &str,
        scope: &EffectScope,
        result: &[u8],
        admission: &dyn EffectAdmission,
    ) -> Result<Self, Failure> {
        let effect = plan.effect(label).map_err(refused)?;
        let mut rows: Vec<Row> = serde_json::from_slice(result)
            .map_err(|_| refused("recorded effect result requires a JSON row array"))?;
        let masked_cells = crate::pipeline::guard::guard_rows(&mut rows);
        let chain = Chain {
            table: effect.table.clone(),
            ops: effect.transforms.clone(),
        };
        let types = chain.shape_types(effect.types.clone());
        let rows = chain.shape(rows).map_err(refused)?;
        let authority = admission.identity(effect)?;
        let payload = admission.prepare(effect, rows, types, scope)?;
        let summary = admission.admit(effect, scope, &payload)?;
        Ok(Self {
            wire: EffectWire {
                version: 1,
                label: label.into(),
                table: effect.table.clone(),
                scope: scope.clone(),
                authority,
                payload,
            },
            summary,
            masked_cells,
        })
    }

    pub fn replay(
        plan: &RecordedBodyPlan,
        label: &str,
        scope: &EffectScope,
        bytes: &[u8],
        admission: &dyn EffectAdmission,
    ) -> Result<Self, Failure> {
        let effect = plan.effect(label).map_err(refused)?;
        let wire: EffectWire = serde_json::from_slice(bytes)
            .map_err(|_| refused("recorded body effect envelope is malformed"))?;
        if wire.version != 1
            || wire.label != label
            || wire.table != effect.table
            || &wire.scope != scope
            || wire.authority != admission.identity(effect)?
        {
            return Err(refused(
                "recorded body effect owner, projection or canonical authority changed",
            ));
        }
        let summary = admission.admit(effect, scope, &wire.payload)?;
        Ok(Self { wire, summary, masked_cells: BTreeMap::new() })
    }

    pub fn encode(&self) -> Result<Vec<u8>, Failure> {
        serde_json::to_vec(&self.wire).map_err(refused)
    }
    /// Mask counts belong to fresh preparation; the durable envelope stores only masked values.
    pub fn masked_cells(&self) -> &BTreeMap<String, usize> {
        &self.masked_cells
    }
    pub fn table(&self) -> &str {
        &self.wire.table
    }
    pub fn label(&self) -> &str { &self.wire.label }
    /// A destination validates the signed payload again; no free bytes grant staging authority.
    pub fn admit_to(&self, destination: &dyn crate::run::ports::Destination, scope: &EffectScope) -> Result<EmissionSummary, Failure> {
        if &self.wire.scope != scope { return Err(refused("prepared body result owner changed")); }
        destination.admit_effect_recorded(&self.wire.table, &self.wire.payload, scope)
    }
    pub fn stage(&self, destination: &mut dyn crate::run::ports::Destination, stage: crate::run::ports::Stage, scope: &EffectScope) -> Result<crate::run::ports::Part, Failure> {
        if stage.table != self.wire.table || &self.wire.scope != scope { return Err(refused("prepared body result destination or owner changed")); }
        destination.stage_effect_recorded(stage, &self.wire.payload, scope)
    }
    pub fn scope(&self) -> &EffectScope {
        &self.wire.scope
    }
    pub fn summary(&self) -> &EmissionSummary {
        &self.summary
    }
}
