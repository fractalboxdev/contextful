//! Tenant connection routes served by `contextful serve --http` (`surface.connection`).
//!
//! A connection record lives in the store owner's snapshot directory beside the applied
//! pointer and claims each version under the lock every control claim takes
//! (`surface.connection.create`). The admitted credential's tenant and principal bound the
//! request before the record is read and the record before it changes
//! (`surface.connection.authority`).

use crate::run::ProjectArgs;
use anyhow::Result;
use contextful_agent::http::HttpRequest;
use contextful_core::grant::Action;
use contextful_core::surface::arm::Schedule;
use contextful_policy::verify::AdmittedAuthority;
use contextful_snapshot::{ConnectionRecord, ControlError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

const ACTIVE: &str = "active";
const PAUSED: &str = "paused";
const DELETED: &str = "deleted";

/// A refusal a connection route answers with its own identifier.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ConnectionError {
    #[error("ConnectionScopeDenied: {0}")]
    ScopeDenied(String),
    #[error("ConnectionIdConflict: {0}")]
    IdConflict(String),
    #[error("ConnectionRequestMalformed: {0}")]
    RequestMalformed(String),
    #[error("ConnectionUnknown: {0}")]
    Unknown(String),
    #[error(transparent)]
    Control(#[from] ControlError),
}

impl ConnectionError {
    /// The status and body a route answers the refusal with; a control failure answers
    /// as the control routes do.
    pub(crate) fn answer(&self) -> (u16, Value) {
        let status = match self {
            ConnectionError::ScopeDenied(_) => 403,
            ConnectionError::IdConflict(_) => 409,
            ConnectionError::RequestMalformed(_) => 400,
            ConnectionError::Unknown(_) => 404,
            ConnectionError::Control(_) => 503,
        };
        let text = self.to_string();
        let (identifier, message) = text.split_once(": ").unwrap_or(("ControlUnavailable", text.as_str()));
        (status, json!({ "error": { "http": status, "identifier": identifier, "message": message } }))
    }
}

/// The declaration a create binds, exactly these fields (`surface.connection.malformed`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    id: String,
    tenant: String,
    principal: String,
    file_ids: Vec<String>,
    refresh_secret: String,
    schedule: String,
}

impl Declaration {
    /// Parse and check a create body before any record is read. The secret is a bare name,
    /// never a reference, code or token (`surface.connection.consent-boundary`).
    fn parse(body: &[u8]) -> Result<Declaration, ConnectionError> {
        let declaration: Declaration = serde_json::from_slice(body).map_err(|e| ConnectionError::RequestMalformed(format!("the create body: {e}")))?;
        if !is_name(&declaration.id, 64) {
            return Err(ConnectionError::RequestMalformed("`id` is 1 to 64 letters, digits, `-` or `_`".into()));
        }
        if declaration.tenant.is_empty() || declaration.principal.is_empty() {
            return Err(ConnectionError::RequestMalformed("`tenant` and `principal` are non-empty".into()));
        }
        if declaration.file_ids.is_empty() || declaration.file_ids.iter().any(String::is_empty) {
            return Err(ConnectionError::RequestMalformed("`file_ids` names at least one file".into()));
        }
        if !declaration.refresh_secret.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/')) || declaration.refresh_secret.is_empty() || declaration.refresh_secret.len() > 128 {
            return Err(ConnectionError::RequestMalformed("`refresh_secret` is a secret name of at most 128 letters, digits, `-`, `_`, `.` or `/`".into()));
        }
        Schedule::parse(&declaration.schedule).map_err(|e| ConnectionError::RequestMalformed(e.to_string()))?;
        Ok(declaration)
    }

    /// The record body a create claims: the declaration, its pipeline id and its state.
    fn record(&self, state: &str) -> Value {
        let mut body = serde_json::to_value(self).expect("a declaration serializes");
        body["pipeline"] = json!(format!("connection-{}", self.id));
        body["state"] = json!(state);
        body
    }
}

fn is_name(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Whether `authority` holds a write grant scoped to `tenant` on behalf of `principal`.
fn admits(authority: &AdmittedAuthority, tenant: &str, principal: &str) -> bool {
    authority.subject().on_behalf_of() == Some(principal)
        && authority.grants().iter().any(|grant| grant.actions.contains(&Action::Write) && grant.tenant.as_ref().is_some_and(|scope| scope.admits(tenant.as_bytes())))
}

/// Refuse a standing record outside the caller's tenant and principal.
fn owned(record: &ConnectionRecord, authority: &AdmittedAuthority, id: &str) -> Result<(), ConnectionError> {
    let field = |key: &str| record.body.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
    if admits(authority, &field("tenant"), &field("principal")) {
        Ok(())
    } else {
        Err(ConnectionError::ScopeDenied(format!("connection `{id}` belongs to another tenant or principal")))
    }
}

fn state(record: &ConnectionRecord) -> &str {
    record.body.get("state").and_then(Value::as_str).unwrap_or_default()
}

/// The answer body: the record's declaration and state with its version.
fn answer(record: &ConnectionRecord) -> Value {
    let mut body = record.body.clone();
    body["version"] = json!(record.version);
    body
}

/// Answer one connection request: its status and body, or the refusal it raised.
pub(crate) fn served(
    project: &ProjectArgs,
    declaration: Option<PathBuf>,
    request: &HttpRequest,
    authority: &AdmittedAuthority,
    boundary: &dyn Fn() -> Result<(), ControlError>,
) -> Result<(u16, Value), ConnectionError> {
    let path = request.target.split('?').next().unwrap_or_default().to_string();
    if path == "/connections" {
        return create(project, declaration, &request.body, authority, boundary);
    }
    let rest = path.strip_prefix("/connections/").unwrap_or_default();
    if !matches!(serde_json::from_slice::<Value>(&request.body), Ok(Value::Object(fields)) if fields.is_empty()) {
        return Err(ConnectionError::RequestMalformed("a pause or delete body is `{}`".into()));
    }
    let (id, pausing) = match rest.strip_suffix("/pause") {
        Some(id) => (id, true),
        None => (rest, false),
    };
    // An id outside the record grammar names no connection, so no storage call reads it.
    if !is_name(id, 64) {
        return Err(ConnectionError::Unknown(format!("no standing connection `{id}`")));
    }
    if pausing {
        pause(project, declaration, id, authority, boundary)
    } else {
        delete(project, declaration, id, authority, boundary)
    }
}

fn create(project: &ProjectArgs, declaration: Option<PathBuf>, body: &[u8], authority: &AdmittedAuthority, boundary: &dyn Fn() -> Result<(), ControlError>) -> Result<(u16, Value), ConnectionError> {
    let declared = Declaration::parse(body)?;
    if !admits(authority, &declared.tenant, &declared.principal) {
        return Err(ConnectionError::ScopeDenied(format!("the credential holds no write grant for tenant `{}` on behalf of `{}`", declared.tenant, declared.principal)));
    }
    let owner = crate::cadence::control_owner(project, declaration).map_err(control)?;
    let (record, claimed) = owner.claim_connection_guarded::<ConnectionError>(&declared.id, |standing| match standing {
        None => Ok(Some(declared.record(ACTIVE))),
        Some(record) => {
            owned(record, authority, &declared.id)?;
            let mut standing_declaration = record.body.clone();
            if let Some(fields) = standing_declaration.as_object_mut() {
                fields.remove("pipeline");
                fields.remove("state");
            }
            if state(record) == DELETED || standing_declaration != serde_json::to_value(&declared).expect("a declaration serializes") {
                return Err(ConnectionError::IdConflict(format!("connection `{}` already binds a different declaration at v{}", declared.id, record.version)));
            }
            Ok(None)
        }
    }, boundary)?;
    Ok((if claimed { 201 } else { 200 }, answer(&record)))
}

/// The standing, undeleted record of `id` the caller owns.
fn standing<'r>(record: Option<&'r ConnectionRecord>, id: &str, authority: &AdmittedAuthority) -> Result<&'r ConnectionRecord, ConnectionError> {
    let record = record.filter(|record| state(record) != DELETED).ok_or_else(|| ConnectionError::Unknown(format!("no standing connection `{id}`")))?;
    owned(record, authority, id)?;
    Ok(record)
}

fn paused(record: &ConnectionRecord) -> Value {
    let mut body = record.body.clone();
    body["state"] = json!(PAUSED);
    body
}

fn pause(project: &ProjectArgs, declaration: Option<PathBuf>, id: &str, authority: &AdmittedAuthority, boundary: &dyn Fn() -> Result<(), ControlError>) -> Result<(u16, Value), ConnectionError> {
    let owner = crate::cadence::control_owner(project, declaration).map_err(control)?;
    let (record, _) = owner.claim_connection_guarded::<ConnectionError>(id, |record| {
        let record = standing(record, id, authority)?;
        Ok((state(record) != PAUSED).then(|| paused(record)))
    }, boundary)?;
    Ok((200, answer(&record)))
}

/// Delete pauses the connection first, then retires it in a second claim
/// (`surface.connection.delete`).
fn delete(project: &ProjectArgs, declaration: Option<PathBuf>, id: &str, authority: &AdmittedAuthority, boundary: &dyn Fn() -> Result<(), ControlError>) -> Result<(u16, Value), ConnectionError> {
    let owner = crate::cadence::control_owner(project, declaration).map_err(control)?;
    owner.claim_connection_guarded::<ConnectionError>(id, |record| {
        let record = standing(record, id, authority)?;
        Ok((state(record) != PAUSED).then(|| paused(record)))
    }, boundary)?;
    let (record, _) = owner.claim_connection_guarded::<ConnectionError>(id, |record| {
        let record = standing(record, id, authority)?;
        let mut body = record.body.clone();
        body["state"] = json!(DELETED);
        Ok(Some(body))
    }, boundary)?;
    Ok((200, answer(&record)))
}

/// A control-plane failure resolving the owner, kept typed when it is one.
fn control(error: anyhow::Error) -> ConnectionError {
    match error.downcast::<ControlError>() {
        Ok(control) => ConnectionError::Control(control),
        Err(error) => match error.downcast::<contextful_core::surface::SurfaceError>() {
            Ok(surface) => ConnectionError::Control(ControlError::Surface(surface)),
            Err(error) => ConnectionError::Control(ControlError::Storage(format!("{error:#}"))),
        },
    }
}
