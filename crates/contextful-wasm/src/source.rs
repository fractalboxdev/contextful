//! A guest session behind the runner's [`Source`] port: one pull is one `next`, and the
//! position after it travels as `{"kind", "bytes"}` with the bytes in hex.
//!
//! A failed pull forgets the open read, so its retry reopens at the position the runner
//! asks for rather than trusting whatever state the failure left the guest's handle in.

use crate::batch::read;
use crate::host::{Cursor, CursorKind, Session};
use contextful_core::run::ports::{Cancellation, Pull, PullRequest, Source};
use contextful_core::run::{Failure, FailureTag};
use serde_json::{json, Value};

pub struct GuestSource {
    session: Session,
    table: String,
    /// The position the open read stands at, as last handed to the runner; `None` once a
    /// pull fails.
    at: Option<Option<Value>>,
}

impl GuestSource {
    pub fn new(session: Session, table: impl Into<String>) -> GuestSource {
        GuestSource { session, table: table.into(), at: None }
    }

    pub fn session(&mut self) -> &mut Session {
        &mut self.session
    }
}

fn kind_name(k: CursorKind) -> &'static str {
    match k {
        CursorKind::Monotonic => "monotonic",
        CursorKind::OpaqueToken => "opaque-token",
        CursorKind::SnapshotId => "snapshot-id",
    }
}

pub fn cursor_value(c: &Cursor) -> Value {
    json!({ "kind": kind_name(c.kind), "bytes": c.bytes.iter().map(|b| format!("{b:02x}")).collect::<String>() })
}

pub fn cursor_from(v: &Value) -> Result<Cursor, Failure> {
    let bad = || Failure::deterministic(FailureTag::SchemaIncompatible, format!("a guest position is `{{\"kind\", \"bytes\"}}`, found {v}"));
    let kind = match v.get("kind").and_then(Value::as_str) {
        Some("monotonic") => CursorKind::Monotonic,
        Some("opaque-token") => CursorKind::OpaqueToken,
        Some("snapshot-id") => CursorKind::SnapshotId,
        _ => return Err(bad()),
    };
    let hex = v.get("bytes").and_then(Value::as_str).ok_or_else(bad)?;
    if !hex.is_ascii() || hex.len() % 2 != 0 {
        return Err(bad());
    }
    let bytes = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16)).collect::<Result<Vec<u8>, _>>().map_err(|_| bad())?;
    Ok(Cursor { kind, bytes })
}

impl Source for GuestSource {
    fn pull(&mut self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        if cancel.requested() {
            return Err(Failure::canceled("the run was stopped ahead of the pull"));
        }
        let pulled = self.read(request);
        if pulled.is_err() {
            self.at = None;
        }
        pulled
    }
}

impl GuestSource {
    fn read(&mut self, request: &PullRequest) -> Result<Vec<u8>, Failure> {
        if self.at.as_ref() != Some(&request.position) {
            let from = request.position.as_ref().map(cursor_from).transpose()?;
            self.session.open(&self.table, from.as_ref())?;
        }
        let batch = self.session.next()?;
        let position = cursor_value(&self.session.position()?);
        let got = batch.as_deref().map(read).transpose()?.unwrap_or_default();
        let pull = Pull { rows: got.rows, types: got.types, cursor: Some(position.clone()), more: batch.is_some(), snapshot_complete: false, skipped: 0, declined: Default::default() };
        self.at = Some(Some(position));
        serde_json::to_vec(&pull).map_err(|e| Failure::new(FailureTag::Permanent, format!("encoding a pull: {e}")))
    }
}
