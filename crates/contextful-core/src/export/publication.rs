//! Coordinated changes: immutable delivery identity and one visibility barrier.

use super::{ChangeState, ExportError, TYPED_REQUEST_BYTES};
use crate::run::journal::sha256_hex;
use crate::store::lay_out::is_path_segment;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub type GroupState = BTreeMap<String, ChangeState>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationDeclaration {
    pub name: String,
    pub job: String,
    pub members: Vec<String>,
}

pub fn declarations(text: &str) -> Result<Vec<PublicationDeclaration>, ExportError> {
    #[derive(Deserialize)]
    struct Manifest {
        #[serde(default)]
        publication: Vec<PublicationDeclaration>,
    }
    let parsed: Manifest = toml::from_str(text).map_err(|e| invalid(e.to_string()))?;
    let exports = super::parse_exports(text)?;
    let mut names = BTreeSet::new();
    for group in &parsed.publication {
        if !is_path_segment(&group.name)
            || !is_path_segment(&group.job)
            || !names.insert(&group.name)
            || group.members.is_empty()
        {
            return Err(invalid(
                "publication names and jobs are unique path segments with nonempty members",
            ));
        }
        let mut members = BTreeSet::new();
        let mut endpoint = None;
        let mut headers = None;
        let mut tables = BTreeSet::new();
        for name in &group.members {
            let export = exports.iter().find(|e| &e.name == name).ok_or_else(|| {
                invalid(format!(
                    "publication `{}` names undeclared export `{name}`",
                    group.name
                ))
            })?;
            if !members.insert(name)
                || !tables.insert(&export.table)
                || export.signal != super::Signal::ChangesV1
                || endpoint.is_some_and(|e| e != &export.endpoint)
                || headers.is_some_and(|h| h != &export.headers)
            {
                return Err(invalid(
                    "publication members are distinct typed tables with one endpoint",
                ));
            }
            endpoint = Some(&export.endpoint);
            headers = Some(&export.headers);
        }
    }
    Ok(parsed.publication)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationPlan {
    pub name: String,
    pub incarnation: String,
    pub namespace: String,
    pub destination: String,
    pub read_identity: String,
    pub base: Option<String>,
    pub order: u64,
    pub input: Value,
    /// Export name to landed table; every member carries a completion event.
    pub members: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationGroup {
    pub id: String,
    #[serde(flatten)]
    pub plan: PublicationPlan,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GroupEvent {
    pub id: String,
    pub sequence: u64,
    pub member: Option<String>,
    #[serde(flatten)]
    pub change: GroupChange,
}

impl<'de> Deserialize<'de> for GroupEvent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum Wire {
            Upsert {
                id: String,
                sequence: u64,
                member: Option<String>,
                key: Value,
                row: Value,
            },
            Delete {
                id: String,
                sequence: u64,
                member: Option<String>,
                key: Value,
            },
            MemberComplete {
                id: String,
                sequence: u64,
                member: Option<String>,
                changes: u64,
            },
            GroupComplete {
                id: String,
                sequence: u64,
                member: Option<String>,
            },
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Upsert {
                id,
                sequence,
                member,
                key,
                row,
            } => Self {
                id,
                sequence,
                member,
                change: GroupChange::Upsert { key, row },
            },
            Wire::Delete {
                id,
                sequence,
                member,
                key,
            } => Self {
                id,
                sequence,
                member,
                change: GroupChange::Delete { key },
            },
            Wire::MemberComplete {
                id,
                sequence,
                member,
                changes,
            } => Self {
                id,
                sequence,
                member,
                change: GroupChange::MemberComplete { changes },
            },
            Wire::GroupComplete {
                id,
                sequence,
                member,
            } => Self {
                id,
                sequence,
                member,
                change: GroupChange::GroupComplete,
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GroupChange {
    Upsert { key: Value, row: Value },
    Delete { key: Value },
    MemberComplete { changes: u64 },
    GroupComplete,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupBatch {
    pub version: u32,
    pub group: PublicationGroup,
    pub events: Vec<GroupEvent>,
}

pub fn invalid(message: impl Into<String>) -> ExportError {
    ExportError::Invalid(format!("PublicationIncompatible: {}", message.into()))
}

pub fn publication(
    plan: PublicationPlan,
    before: &GroupState,
    after: &GroupState,
) -> Result<GroupBatch, ExportError> {
    if !is_path_segment(&plan.name)
        || !is_path_segment(&plan.namespace)
        || plan.incarnation.is_empty()
        || plan.destination.is_empty()
        || plan.read_identity.is_empty()
        || plan.members.is_empty()
        || plan.order == 0
        || plan.members.keys().collect::<Vec<_>>() != after.keys().collect::<Vec<_>>()
        || before.keys().any(|name| !plan.members.contains_key(name))
    {
        return Err(invalid(
            "a publication binds its incarnation, namespace, order and exact members",
        ));
    }
    let id = sha256_hex(
        &serde_json::to_vec(&(&plan, before, after)).map_err(|e| invalid(e.to_string()))?,
    );
    let mut events = Vec::new();
    let mut push = |member: Option<String>, change| {
        let sequence = events.len() as u64;
        events.push(GroupEvent {
            id: format!("{id}:{sequence}"),
            sequence,
            member,
            change,
        });
    };
    for (member, state) in after {
        let empty = ChangeState::new();
        let prior = before.get(member).unwrap_or(&empty);
        let keys: BTreeSet<_> = prior.keys().chain(state.keys()).collect();
        let mut changes = 0;
        for key in keys {
            let change = match (prior.get(key), state.get(key)) {
                (Some(a), Some(b)) if a == b => continue,
                (_, Some(row)) => GroupChange::Upsert {
                    key: serde_json::from_str(key).map_err(|e| invalid(e.to_string()))?,
                    row: row.clone(),
                },
                (Some(_), None) => GroupChange::Delete {
                    key: serde_json::from_str(key).map_err(|e| invalid(e.to_string()))?,
                },
                (None, None) => continue,
            };
            push(Some(member.clone()), change);
            changes += 1;
        }
        push(
            Some(member.clone()),
            GroupChange::MemberComplete { changes },
        );
    }
    push(None, GroupChange::GroupComplete);
    Ok(GroupBatch {
        version: 1,
        group: PublicationGroup { id, plan },
        events,
    })
}

/// One request prefix under the typed delivery byte and row ceilings.
pub fn batch(publication: &GroupBatch, offset: usize) -> Result<Option<GroupBatch>, ExportError> {
    if offset >= publication.events.len() {
        return Ok(None);
    }
    let mut result = GroupBatch {
        version: 1,
        group: publication.group.clone(),
        events: Vec::new(),
    };
    for event in publication.events[offset..]
        .iter()
        .take(super::EXPORT_BATCH_ROWS)
    {
        result.events.push(event.clone());
        if serde_json::to_vec(&result)
            .map_err(|e| invalid(e.to_string()))?
            .len()
            > TYPED_REQUEST_BYTES
        {
            result.events.pop();
            break;
        }
    }
    if result.events.is_empty() {
        return Err(super::ExportError::ExportEventTooLarge(
            "publication event exceeds typed request ceiling".into(),
        ));
    }
    Ok(Some(result))
}

fn encoded_len(value: &impl Serialize) -> Result<usize, ExportError> {
    Ok(serde_json::to_vec(value)
        .map_err(|e| invalid(e.to_string()))?
        .len())
}

/// The group's request bytes without events, and each event's bytes in a request.
fn request_sizes(publication: &GroupBatch) -> Result<(usize, Vec<usize>), ExportError> {
    let header = encoded_len(&GroupBatch {
        version: publication.version,
        group: publication.group.clone(),
        events: Vec::new(),
    })?;
    let events = publication
        .events
        .iter()
        .map(encoded_len)
        .collect::<Result<_, _>>()?;
    Ok((header, events))
}

/// Every request of a group fits the typed ceiling (`run.export.group-request-bound`).
fn sendable(publication: &GroupBatch) -> Result<(), ExportError> {
    let (header, events) = request_sizes(publication)?;
    if header + events.iter().min().copied().unwrap_or(0) > TYPED_REQUEST_BYTES {
        return Err(ExportError::PublicationHeaderTooLarge(format!(
            "group `{}` header holds {header} B of the {TYPED_REQUEST_BYTES} B request ceiling",
            publication.group.plan.name
        )));
    }
    if let Some(at) = events
        .iter()
        .position(|len| header + len > TYPED_REQUEST_BYTES)
    {
        return Err(ExportError::ExportEventTooLarge(format!(
            "publication event `{}` exceeds {TYPED_REQUEST_BYTES} B beside its group header",
            publication.events[at].id
        )));
    }
    Ok(())
}

/// A producer completion pinning its input frontier and every declared output frontier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourcePublication {
    pub run: String,
    pub input: Value,
    pub frontiers: BTreeMap<String, String>,
}

impl SourcePublication {
    /// A plan's `input`: the completion's SHA-256, bounded whatever its frontier holds
    /// (`run.export.group-input-digest`).
    pub fn digest(&self) -> Result<String, ExportError> {
        Ok(sha256_hex(
            &serde_json::to_vec(self).map_err(|e| invalid(e.to_string()))?,
        ))
    }

    /// A producer run records one frontier: recorded again over `latest`, the ledger's
    /// latest completion, it must carry identical bytes; a completion naming another run
    /// replaces `latest` (`run.export.group-source-latest`).
    pub fn check_history(&self, latest: Option<&SourcePublication>) -> Result<(), ExportError> {
        if latest.is_some_and(|prior| prior.run == self.run && prior != self) {
            return Err(invalid("producer run carries different frontier bytes"));
        }
        Ok(())
    }
}

/// One namespace's sender: the active group, its acknowledged state and the staged outbox.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GroupSender {
    identity: String,
    incarnation: String,
    active: Option<PublicationGroup>,
    state: GroupState,
    pending: Option<GroupBatch>,
    next_state: GroupState,
    offset: usize,
}

impl GroupSender {
    pub fn active(&self) -> Option<&PublicationGroup> {
        self.active.as_ref()
    }

    pub fn pending_identity(&self) -> Option<&str> {
        self.pending
            .as_ref()
            .filter(|_| !self.stranded())
            .map(|_| self.identity.as_str())
    }

    /// A pending group unable to form its first request, so never offered
    /// (`run.export.group-stranded`).
    fn stranded(&self) -> bool {
        self.offset == 0
            && self.pending.as_ref().is_some_and(|pending| {
                request_sizes(pending).is_ok_and(|(header, events)| {
                    events
                        .first()
                        .is_some_and(|first| header + first > TYPED_REQUEST_BYTES)
                })
            })
    }

    /// Returns whether the sender changed; a pending outbox stays immutable until acknowledged,
    /// unless it was never offered.
    pub fn stage(
        &mut self,
        plan: &PublicationPlan,
        states: &GroupState,
        identity: &str,
    ) -> Result<bool, ExportError> {
        if plan.read_identity != identity {
            return Err(invalid(
                "group read identity differs from its admitted binding",
            ));
        }
        if !self.incarnation.is_empty() && self.incarnation != plan.incarnation {
            return Err(invalid("store incarnation differs"));
        }
        if let Some(active) = &self.active {
            if active.plan.destination != plan.destination {
                return Err(invalid("target changed; use a fresh namespace rebuild"));
            }
            if active.plan.members != plan.members {
                return Err(invalid("active membership changed"));
            }
        }
        let stranded = self.stranded();
        if let Some(pending) = self.pending.as_ref().filter(|_| !stranded) {
            if pending.group.plan.members != plan.members
                || pending.group.plan.destination != plan.destination
                || self.identity != identity
            {
                return Err(invalid(
                    "pending membership, target or read identity differs",
                ));
            }
            return Ok(false);
        }
        if self.active.as_ref().map(|g| &g.id) != plan.base.as_ref()
            || self
                .active
                .as_ref()
                .map_or(Some(1), |g| g.plan.order.checked_add(1))
                != Some(plan.order)
        {
            return Err(invalid("sender predecessor or order differs"));
        }
        if self
            .active
            .as_ref()
            .is_some_and(|g| g.plan.input == plan.input && g.plan.members == plan.members)
            && self.identity == identity
            && self.state == *states
        {
            if stranded {
                self.pending = None;
                self.next_state = GroupState::new();
            }
            return Ok(stranded);
        }
        let next = publication(plan.clone(), &self.state, states)?;
        sendable(&next)?;
        self.pending = Some(next);
        self.identity = identity.into();
        self.incarnation = plan.incarnation.clone();
        self.next_state = states.clone();
        self.offset = 0;
        Ok(true)
    }

    /// The next request prefix of the pending outbox.
    pub fn batch(&self) -> Result<Option<GroupBatch>, ExportError> {
        self.pending
            .as_ref()
            .map(|p| batch(p, self.offset))
            .transpose()
            .map(Option::flatten)
    }

    /// Returns whether the acknowledgement completed the group and promoted its state.
    pub fn acknowledge(&mut self, group: &str, through: u64) -> Result<bool, ExportError> {
        let offered = self
            .batch()?
            .ok_or_else(|| invalid("acknowledgement names no pending batch"))?;
        if offered.group.id != group || offered.events.last().map(|e| e.sequence) != Some(through) {
            return Err(invalid("concurrent acknowledgement differs"));
        }
        self.offset += offered.events.len();
        let pending = self
            .pending
            .as_ref()
            .expect("offered batch has a pending group");
        if self.offset < pending.events.len() {
            return Ok(false);
        }
        self.active = self.pending.take().map(|p| p.group);
        self.state = std::mem::take(&mut self.next_state);
        self.offset = 0;
        Ok(true)
    }

    /// Resume the outbox after the event carrying `through`, an acknowledged sequence short of
    /// completion, whatever batch boundaries this binary cuts (`run.export.group-d1-acknowledge`).
    pub fn resume(&mut self, group: &str, through: u64) -> Result<(), ExportError> {
        let pending = self
            .pending
            .as_ref()
            .filter(|pending| pending.group.id == group)
            .ok_or_else(|| invalid("acknowledged group is not the pending group"))?;
        let position = pending
            .events
            .iter()
            .position(|event| event.sequence == through)
            .filter(|position| *position >= self.offset && position + 1 < pending.events.len())
            .ok_or_else(|| {
                invalid("acknowledged sequence names no unacknowledged event short of completion")
            })?;
        self.offset = position + 1;
        Ok(())
    }

    pub fn checkpoint(&self, name: &str, namespace: &str) -> Result<Vec<u8>, ExportError> {
        serde_json::to_vec(&(1, name, namespace, self)).map_err(|e| invalid(e.to_string()))
    }

    /// A checkpoint replays only byte-identical delivery under the current binding.
    pub fn restore(
        bytes: &[u8],
        name: &str,
        namespace: &str,
        incarnation: &str,
        identity: &str,
        current: Option<&GroupSender>,
    ) -> Result<Self, ExportError> {
        let (version, saved_name, saved_namespace, sender): (u32, String, String, GroupSender) =
            serde_json::from_slice(bytes).map_err(|e| invalid(e.to_string()))?;
        if version != 1
            || saved_name != name
            || saved_namespace != namespace
            || sender.incarnation != incarnation
            || sender.identity != identity
        {
            return Err(invalid(
                "checkpoint name, namespace, incarnation or read identity differs",
            ));
        }
        if let Some(pending) = &sender.pending {
            let canonical = publication(
                pending.group.plan.clone(),
                &sender.state,
                &sender.next_state,
            )?;
            if canonical != *pending || sender.offset >= pending.events.len() {
                return Err(invalid("checkpoint changes immutable delivery bytes"));
            }
        }
        if let Some(current) = current {
            if current.incarnation != incarnation
                || current.identity != identity
                || current.members() != sender.members()
            {
                return Err(invalid("checkpoint binding differs"));
            }
        }
        Ok(sender)
    }

    fn members(&self) -> Option<&BTreeMap<String, String>> {
        self.active
            .as_ref()
            .map(|g| &g.plan.members)
            .or_else(|| self.pending.as_ref().map(|g| &g.group.plan.members))
    }
}

/// Receiver state persists accepted bytes and staged rows before acknowledging delivery.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReceiverState {
    pub active: Option<PublicationGroup>,
    pub state: GroupState,
    pub accepted: BTreeMap<String, GroupBatch>,
    pub pending: Option<GroupBatch>,
    pub namespaces: BTreeSet<String>,
}

impl ReceiverState {
    pub fn receive(&mut self, batch: &GroupBatch) -> Result<(), ExportError> {
        if batch.version != 1 || batch.events.is_empty() {
            return Err(invalid(
                "a delivery has version one and a nonempty event prefix",
            ));
        }
        let empty_members = batch
            .group
            .plan
            .members
            .keys()
            .map(|name| (name.clone(), ChangeState::new()))
            .collect();
        publication(batch.group.plan.clone(), &GroupState::new(), &empty_members)?;
        if batch.group.id.len() != 64 || !batch.group.id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid("a group identity is a SHA-256 digest"));
        }
        if let Some(accepted) = self.accepted.get(&batch.group.id) {
            return validate_prefix(accepted, batch);
        }
        if let Some(active) = &self.active {
            if active.plan.incarnation != batch.group.plan.incarnation
                || active.plan.members != batch.group.plan.members
            {
                return Err(invalid("receiver incarnation or membership differs"));
            }
            if active.plan.namespace == batch.group.plan.namespace {
                if batch.group.plan.base.as_deref() != Some(&active.id)
                    || active.plan.order.checked_add(1) != Some(batch.group.plan.order)
                {
                    return Err(invalid(
                        "the predecessor and publication order differ from the active group",
                    ));
                }
            } else if self.namespaces.contains(&batch.group.plan.namespace)
                || batch.group.plan.base.is_some()
                || batch.group.plan.order != 1
            {
                return Err(invalid(
                    "a rebuild uses a fresh namespace with no predecessor",
                ));
            }
        } else if batch.group.plan.base.is_some() || batch.group.plan.order != 1 {
            return Err(invalid(
                "an empty receiver requires a full publication or rebuild",
            ));
        }
        let mut pending = self.pending.clone().unwrap_or_else(|| GroupBatch {
            version: 1,
            group: batch.group.clone(),
            events: Vec::new(),
        });
        if pending.group != batch.group {
            return Err(invalid("another immutable group is staged"));
        }
        for event in &batch.events {
            let index = usize::try_from(event.sequence).map_err(|e| invalid(e.to_string()))?;
            if index < pending.events.len() {
                if pending.events[index] != *event {
                    return Err(invalid("an event identity carries different bytes"));
                }
            } else if index == pending.events.len()
                && event.id == format!("{}:{index}", batch.group.id)
            {
                pending.events.push(event.clone());
            } else {
                return Err(invalid("delivery skips an event or its immutable identity"));
            }
        }
        if matches!(
            pending.events.last().map(|e| &e.change),
            Some(GroupChange::GroupComplete)
        ) {
            let before = if self
                .active
                .as_ref()
                .is_some_and(|g| g.plan.namespace == batch.group.plan.namespace)
            {
                self.state.clone()
            } else {
                GroupState::new()
            };
            let mut after: GroupState = batch
                .group
                .plan
                .members
                .keys()
                .map(|m| (m.clone(), before.get(m).cloned().unwrap_or_default()))
                .collect();
            let mut complete = BTreeSet::new();
            for event in &pending.events {
                match (&event.member, &event.change) {
                    (Some(member), GroupChange::Upsert { key, row })
                        if !complete.contains(member) =>
                    {
                        after
                            .get_mut(member)
                            .ok_or_else(|| invalid("undeclared publication member"))?
                            .insert(
                                serde_json::to_string(key).map_err(|e| invalid(e.to_string()))?,
                                row.clone(),
                            );
                    }
                    (Some(member), GroupChange::Delete { key }) if !complete.contains(member) => {
                        after
                            .get_mut(member)
                            .ok_or_else(|| invalid("undeclared publication member"))?
                            .remove(
                                &serde_json::to_string(key).map_err(|e| invalid(e.to_string()))?,
                            );
                    }
                    (Some(member), GroupChange::MemberComplete { .. })
                        if after.contains_key(member) && complete.insert(member.clone()) => {}
                    (None, GroupChange::GroupComplete) if complete.len() == after.len() => {}
                    _ => return Err(invalid("member completion and group barrier order differs")),
                }
            }
            let canonical = publication(batch.group.plan.clone(), &before, &after)?;
            if canonical != pending {
                return Err(invalid(
                    "group identity or event bytes differ from the committed transition",
                ));
            }
            self.namespaces.insert(batch.group.plan.namespace.clone());
            self.active = Some(batch.group.clone());
            self.state = after;
            self.accepted.insert(batch.group.id.clone(), pending);
            self.pending = None;
        } else {
            self.pending = Some(pending);
        }
        Ok(())
    }
}

fn validate_prefix(accepted: &GroupBatch, batch: &GroupBatch) -> Result<(), ExportError> {
    if accepted.group != batch.group {
        return Err(invalid("accepted group identity carries different bytes"));
    }
    for event in &batch.events {
        if usize::try_from(event.sequence)
            .ok()
            .and_then(|i| accepted.events.get(i))
            != Some(event)
        {
            return Err(invalid("accepted event identity carries different bytes"));
        }
    }
    Ok(())
}
