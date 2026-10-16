//! The tool protocol: one JSON-RPC 2.0 message in, at most one answer out.
//!
//! [`Tools`] answers a message for an admitted authority the transport hands it, and
//! re-reads that authority at every tool call before any row is read. [`Server`] is the
//! transport over standard input and output, one message per line, holding one admitted
//! authority for its lifetime; the network transport admits one per request. A refusal
//! arrives in-band — a result flagged as an error under transport success — carrying the
//! refusal's wire payload (`read.respond.in-band-error`).
//!
//! Every read tool call [`Tools`] answers with a result appends one audit entry through its
//! [`ReadRecord`] before the result leaves (`disclosure.record.read-entry`); an entry that
//! does not persist answers `AuditEntryUnpersisted` in-band with no rows
//! (`disclosure.record.unpersisted-wire`).

use contextful_context::read::explain::ExplainRequest;
use contextful_context::read::{Face, ReadFault, ReadOptions, RecallRequest, RetrieveRequest};
use contextful_core::memory::declare::Shape;
use contextful_core::read::face::{read_backend, register_tool, require, BuildIdentity, FaceScope, ToolKind, TOOLS};
use contextful_core::read::error::ReadError;
use contextful_core::read::guard::admit;
use contextful_core::read::horizon::{horizon, inherit, within};
use contextful_core::read::pin::{Pins, PIN_ARGUMENT};
use contextful_core::read::template::READ_ARGUMENTS;
use contextful_core::run::journal::sha256_hex;

/// The block a bounded answer echoes its bounds under (`store.bound-time.echo`).
const BOUNDS_BLOCK: &str = "contextful.bounds";
use contextful_core::read::respond::Response;
use contextful_core::read::Refusal;
use contextful_core::store::bound_time::{Bound, Bounds, Vintage};
use contextful_core::ports::Clock;
use contextful_core::time::Instant;
use contextful_core::ports::SigningPort;
use contextful_core::AuthorityError;
use contextful_policy::audit::{attr, AuditError, AuditLog};
use contextful_policy::enforce::refuse::{payload, unpersisted};
use contextful_policy::enforce::session::{Request, Session};
use contextful_policy::verify::AdmittedAuthority;
use serde_json::{json, Map, Value};
use std::io::{BufRead, Write};

/// The tool-protocol revision this server speaks when a client names none.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// JSON-RPC error codes.
const PARSE_ERROR: i64 = -32700;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// What this package links, reported at the handshake (`read.embed.build-identity`): the
/// embedded SQL engine, the lexical ranker and the vector index, and the `http` face where
/// the `http` feature links the network transport. It links no connector family; a binary
/// linking more reports its own through [`Tools::with_build`].
pub fn build_identity() -> BuildIdentity {
    let faces = if cfg!(feature = "http") { vec!["http".into()] } else { Vec::new() };
    let lexical = contextful_context::read::LEXICAL_BACKEND.then(|| "fts".to_string());
    let backends = std::iter::once("duckdb".to_string()).chain(lexical).chain(["hnsw".to_string()]).collect();
    BuildIdentity { backends, connectors: Vec::new(), faces }
}

/// The re-check an effect boundary runs against the carried authority.
pub type Boundary<'a> = dyn Fn(&AdmittedAuthority) -> Result<(), AuthorityError> + 'a;

/// The authority one message is answered for, and the boundary re-reading it.
#[derive(Clone, Copy)]
pub struct Caller<'c> {
    pub authority: &'c AdmittedAuthority,
    pub boundary: &'c Boundary<'c>,
}

/// Where a read's audit entry goes: the read path appends through it and releases the
/// result only once the call returns.
pub trait ReadRecord: Send + Sync {
    /// Append one read's entry, returning once it is durable.
    fn record(&self, attributes: Value) -> Result<(), AuditError>;
}

/// The audit chain records a read as one entry of an append group.
impl<S: SigningPort + Send + Sync + 'static> ReadRecord for AuditLog<S> {
    fn record(&self, attributes: Value) -> Result<(), AuditError> {
        self.append(attributes).map(|_| ())
    }
}

/// The read-entry attribute listing the claim ids a memory read returned
/// (`read.recall.usage-ledger`).
pub const MEMORY_CLAIMS: &str = "contextful.memory.claims";

/// One read as its audit entry records it.
pub struct ReadEntry<'r> {
    pub tool: &'r str,
    pub authority: &'r AdmittedAuthority,
    /// The rows the result returns; a refused read returns none.
    pub rows: u64,
    pub at: Instant,
    /// The relations the read names (`disclosure.record.read-attributes`).
    pub tables: Vec<String>,
    /// The typed refusal of a read enforcement refused (`disclosure.record.refused-read`).
    pub refusal: Option<&'r str>,
    /// The claim ids a memory read returned (`read.recall.usage-ledger`); `None` for a
    /// read that is no memory read.
    pub claims: Option<Vec<String>>,
}

/// A read's entry attributes (`disclosure.record.read-attributes`): the tool, the
/// credential, each present subject member and its attestation, the rows returned, the
/// read's instant, its outcome and the relations it names.
pub fn read_attributes(read: &ReadEntry<'_>) -> Value {
    let mut attributes = Map::new();
    attributes.insert("contextful.tool".into(), json!(read.tool));
    attributes.insert("contextful.credential".into(), json!(read.authority.credential_id()));
    let subject = read.authority.subject();
    for (member, attestation) in subject.attestations() {
        let name = member.as_str();
        attributes.insert(format!("contextful.subject.{name}"), json!(subject.get(member)));
        attributes.insert(format!("contextful.subject.attestation.{name}"), json!(attestation));
    }
    attributes.insert(attr::ROWS.into(), json!(read.rows));
    attributes.insert(attr::READ_AT.into(), json!(read.at.to_rfc3339()));
    attributes.insert(attr::OUTCOME.into(), json!(if read.refusal.is_some() { attr::REFUSED } else { attr::SERVED }));
    attributes.insert(attr::TABLES.into(), json!(read.tables));
    if let Some(identifier) = read.refusal {
        attributes.insert(attr::REFUSAL.into(), json!(identifier));
    }
    if let Some(claims) = &read.claims {
        attributes.insert(MEMORY_CLAIMS.into(), json!(claims));
    }
    Value::Object(attributes)
}

/// The answer to a read whose entry did not persist: `AuditEntryUnpersisted` in-band,
/// carrying no row (`disclosure.record.unpersisted-wire`).
fn unrecorded(e: &AuditError) -> Value {
    let p = unpersisted(&e.to_string());
    json!({ "content": [{ "type": "text", "text": p.to_string() }], "structuredContent": p, "isError": true })
}

/// The closed read tool set over one read face, shared by every transport.
pub struct Tools<'a> {
    face: &'a Face,
    clock: &'a (dyn Clock + Sync),
    record: &'a dyn ReadRecord,
    /// The identity the handshake reports and every read tool call checks.
    build: BuildIdentity,
}

/// The tool protocol over standard input and output for one admitted authority.
pub struct Server<'a> {
    tools: Tools<'a>,
    authority: AdmittedAuthority,
    boundary: &'a Boundary<'a>,
}

/// A protocol-level error: code and message.
struct Protocol(i64, String);

fn invalid(message: impl Into<String>) -> Protocol {
    Protocol(INVALID_PARAMS, message.into())
}

/// Read one optional argument of a JSON type.
fn arg<'v>(args: &'v Map<String, Value>, name: &str) -> Option<&'v Value> {
    args.get(name).filter(|v| !v.is_null())
}

fn string(args: &Map<String, Value>, name: &str) -> Result<Option<String>, Protocol> {
    match arg(args, name) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(invalid(format!("`{name}` is a string, not {other}"))),
    }
}

fn integer(args: &Map<String, Value>, name: &str) -> Result<Option<u64>, Protocol> {
    match arg(args, name) {
        None => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| invalid(format!("`{name}` is a non-negative integer, not {v}"))),
    }
}

fn boolean(args: &Map<String, Value>, name: &str) -> Result<bool, Protocol> {
    match arg(args, name) {
        None => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(other) => Err(invalid(format!("`{name}` is a boolean, not {other}"))),
    }
}

fn required(args: &Map<String, Value>, name: &str) -> Result<String, Protocol> {
    string(args, name)?.ok_or_else(|| invalid(format!("`{name}` is required")))
}

fn bound(args: &Map<String, Value>, name: &str) -> Result<Option<Bound>, Protocol> {
    string(args, name)?.map(|s| Bound::parse(&s)).transpose().map_err(|e| invalid(format!("`{name}`: {e}")))
}

fn bounds(args: &Map<String, Value>) -> Result<Bounds, Protocol> {
    Ok(Bounds { as_of: bound(args, "as_of")?, valid_as_of: bound(args, "valid_as_of")?, available_as_of: bound(args, "available_as_of")?, vintage: vintage(args)? })
}

/// The read argument choosing a vintage table's resolution.
const VINTAGE: &str = "vintage";

/// The vintage a read resolves on a vintage table: `first`, or `latest` by default
/// (`store.available-time.vintage-first`).
fn vintage(args: &Map<String, Value>) -> Result<Vintage, Protocol> {
    match string(args, VINTAGE)?.as_deref() {
        None | Some("latest") => Ok(Vintage::Latest),
        Some("first") => Ok(Vintage::First),
        Some(other) => Err(invalid(format!("`{VINTAGE}` is `first` or `latest`, not `{other}`"))),
    }
}

/// Refuse an argument a tool does not declare; every read tool declares the bounds and `zone`
/// (`read.register.bound-arguments`, `read.register.available-bound-argument`).
fn only(args: &Map<String, Value>, tool: &str, known: &[&str]) -> Result<(), Protocol> {
    match args.keys().find(|k| !known.contains(&k.as_str()) && !READ_ARGUMENTS.contains(&k.as_str()) && k.as_str() != VINTAGE) {
        Some(k) => Err(invalid(format!("`{tool}` takes no argument `{k}`"))),
        None => Ok(()),
    }
}

fn instant(args: &Map<String, Value>, name: &str) -> Result<Option<Instant>, Protocol> {
    string(args, name)?
        .map(|s| {
            let s = if s.len() == 10 { format!("{s}T00:00:00Z") } else { s };
            Instant::parse(&s).map_err(|e| invalid(e.to_string()))
        })
        .transpose()
}

/// `args` under the credential's horizon: `since` and `available_as_of` hold within it
/// (`read.horizon.exceeds-horizon`), and an absent `available_as_of` reads at it
/// (`read.horizon.inherited-bound`).
fn inherit_horizon(args: &Map<String, Value>, horizon: Option<Instant>) -> Result<Result<Map<String, Value>, ReadError>, Protocol> {
    if let Err(refused) = within(horizon, "since", instant(args, "since")?) {
        return Ok(Err(refused));
    }
    let bounds = match inherit(horizon, bounds(args)?) {
        Ok(b) => b,
        Err(refused) => return Ok(Err(refused)),
    };
    let mut args = args.clone();
    if let (false, Some(b)) = (args.contains_key("available_as_of"), bounds.available_as_of) {
        args.insert("available_as_of".into(), json!(b.at.to_rfc3339_nanos()));
    }
    Ok(Ok(args))
}

fn options(args: &Map<String, Value>) -> Result<ReadOptions, Protocol> {
    Ok(ReadOptions { limit: integer(args, "limit")?, max_duration_ms: integer(args, "max_duration_ms")?, max_response_bytes: integer(args, "max_response_bytes")?, internals: boolean(args, "internals")?, bounds: bounds(args)? })
}

/// A row-bearing response and the count of rows it returns.
fn answered(r: Response) -> (Value, u64) {
    let rows = r.rows.len() as u64;
    (r.to_json(), rows)
}

fn answered_in(r: Response, session: Session) -> (Value, u64, Session) {
    let (value, rows) = answered(r);
    (value, rows, session)
}

/// A tool result: the structured value and its text rendering.
fn result(value: Value) -> Value {
    json!({ "content": [{ "type": "text", "text": value.to_string() }], "structuredContent": value })
}

/// A refusal as an in-band tool error.
fn refused(r: &Refusal) -> Value {
    let p = payload(r);
    json!({ "content": [{ "type": "text", "text": p.to_string() }], "structuredContent": p, "isError": true })
}

impl<'a> Server<'a> {
    /// A server for one admitted authority.
    pub fn new(
        face: &'a Face,
        authority: AdmittedAuthority,
        boundary: &'a Boundary<'a>,
        clock: &'a (dyn Clock + Sync),
        record: &'a dyn ReadRecord,
    ) -> Result<Server<'a>, String> {
        Ok(Server { tools: Tools::new(face, clock, record)?, authority, boundary })
    }

    /// The server reporting `build` in place of this binary's linked identity.
    pub fn with_build(self, build: BuildIdentity) -> Server<'a> {
        Server { tools: self.tools.with_build(build), ..self }
    }

    /// Serve until the input closes.
    pub fn serve(&self, input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(reply) = self.handle(&line) {
                writeln!(output, "{reply}")?;
                output.flush()?;
            }
        }
        Ok(())
    }

    /// Answer one message; a notification has no answer.
    pub fn handle(&self, line: &str) -> Option<Value> {
        self.tools.handle_text(Caller { authority: &self.authority, boundary: self.boundary }, line)
    }
}

impl<'a> Tools<'a> {
    /// The tool set over `face`, recording each answered read through `record`. Every
    /// built-in tool registers as a read tool (`authority.resist.read-only-face`).
    pub fn new(face: &'a Face, clock: &'a (dyn Clock + Sync), record: &'a dyn ReadRecord) -> Result<Tools<'a>, String> {
        for tool in TOOLS {
            register_tool(FaceScope::Organization, tool, ToolKind::Read).map_err(|e| e.to_string())?;
        }
        Ok(Tools { face, clock, record, build: build_identity() })
    }

    /// The tool set reporting `build` in place of this binary's linked identity. A build
    /// reporting no embedded SQL engine refuses every read tool
    /// (`read.embed.absent-read-backend`).
    pub fn with_build(mut self, build: BuildIdentity) -> Tools<'a> {
        self.build = build;
        self
    }

    /// The identity the handshake and `/health` report.
    pub fn build(&self) -> &BuildIdentity {
        &self.build
    }

    /// The clock tool calls read the present from.
    pub fn clock(&self) -> &'a (dyn Clock + Sync) {
        self.clock
    }

    /// Answer one message's text for `caller`; a notification has no answer, and text
    /// that is not JSON answers a parse error.
    pub fn handle_text(&self, caller: Caller<'_>, text: &str) -> Option<Value> {
        match serde_json::from_str::<Value>(text) {
            Ok(message) => self.handle(caller, &message),
            Err(e) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": PARSE_ERROR, "message": e.to_string() } })),
        }
    }

    /// Answer one message for `caller`; a notification has no answer.
    pub fn handle(&self, caller: Caller<'_>, message: &Value) -> Option<Value> {
        let id = message.get("id").cloned()?;
        let method = message["method"].as_str().unwrap_or_default();
        let params = message.get("params").and_then(Value::as_object).cloned().unwrap_or_default();
        let answer = match method {
            "initialize" => self.initialize(&params),
            "ping" => Ok(json!({})),
            "tools/list" => self.list(caller),
            "tools/call" => self.call(caller, &params),
            other => Err(Protocol(METHOD_NOT_FOUND, format!("no method `{other}`"))),
        };
        Some(match answer {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(Protocol(code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
        })
    }

    /// The handshake: the reported build identity, and a refusal for any required name
    /// outside it, ahead of the first read (`read.embed.required-face`).
    fn initialize(&self, params: &Map<String, Value>) -> Result<Value, Protocol> {
        let wanted: Vec<String> = match params.get("require") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(names)) => {
                names.iter().map(|n| n.as_str().map(str::to_string).ok_or_else(|| invalid("`require` lists strings"))).collect::<Result<_, _>>()?
            }
            Some(_) => return Err(invalid("`require` is a list of names")),
        };
        let build = &self.build;
        require(Some(build), &wanted).map_err(|e| invalid(e.to_string()))?;
        let version = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION);
        Ok(json!({
            "protocolVersion": version,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "contextful", "version": env!("CARGO_PKG_VERSION") },
            "contextful.build": build,
        }))
    }

    fn session(&self, caller: Caller<'_>, zone: Option<&str>, bounds: Bounds, pins: &Pins) -> Result<contextful_policy::enforce::session::Session, ReadFault> {
        (caller.boundary)(caller.authority)?;
        self.face.session_pinned(caller.authority, &Request { zone }, bounds, pins)
    }

    /// The session a covered template runs in (`authority.grant.template-tables`).
    fn template_session(&self, caller: Caller<'_>, zone: Option<&str>, bounds: Bounds, pins: &Pins, id: &str) -> Result<contextful_policy::enforce::session::Session, ReadFault> {
        (caller.boundary)(caller.authority)?;
        self.face.template_session(caller.authority, &Request { zone }, bounds, pins, id)
    }

    fn list(&self, caller: Caller<'_>) -> Result<Value, Protocol> {
        match self.session(caller, None, Bounds::default(), &Pins::default()) {
            Ok(s) => Ok(json!({ "tools": self.face.tools(&s) })),
            Err(fault) => Err(Protocol(INVALID_PARAMS, fault.to_string())),
        }
    }

    fn call(&self, caller: Caller<'_>, params: &Map<String, Value>) -> Result<Value, Protocol> {
        let name = params.get("name").and_then(Value::as_str).ok_or_else(|| invalid("`name` is required"))?;
        let args = match params.get("arguments") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => return Err(invalid("`arguments` is an object")),
        };
        let read = |rows, refusal, claims| ReadEntry {
            tool: name,
            authority: caller.authority,
            rows,
            at: self.clock.now(),
            tables: self.relations(name, &args),
            refusal,
            claims,
        };
        match self.dispatch(caller, name, &args) {
            // The result leaves only once its entry is durable; otherwise no row does.
            Ok(Ok((value, rows, session))) => match self.bound_entry(name, &args, &value, &session).map(|bound| {
                let mut attributes = read_attributes(&read(rows, None, self.claims(name, &value)));
                if let (Value::Object(a), Some(bound)) = (&mut attributes, bound) {
                    a.extend(bound);
                }
                attributes
            }) {
                Err(fault) => Ok(json!({ "content": [{ "type": "text", "text": fault.to_string() }], "isError": true })),
                Ok(attributes) => match self.record.record(attributes) {
                    Ok(()) => Ok(result(value)),
                    Err(e) => Ok(unrecorded(&e)),
                },
            },
            // A refusal is recorded before it answers (`disclosure.record.refused-read`).
            Ok(Err(fault)) => match fault.refusal() {
                Some(r) => match self.record.record(read_attributes(&read(0, Some(r.identifier()), None))) {
                    Ok(()) => Ok(refused(r)),
                    Err(e) => Ok(unrecorded(&e)),
                },
                None => Ok(json!({ "content": [{ "type": "text", "text": fault.to_string() }], "isError": true })),
            },
            Err(protocol) => Err(protocol),
        }
    }

    /// A bounded read's added entry attributes (`disclosure.record.bound-entry`): the
    /// echoed bounds, the snapshot id per relation the read names, its query text and the
    /// SHA-256 of the answer it returns; `None` for an unbounded read.
    fn bound_entry(&self, name: &str, args: &Map<String, Value>, value: &Value, session: &Session) -> Result<Option<Map<String, Value>>, ReadFault> {
        let Some(echoed) = value.get(BOUNDS_BLOCK) else { return Ok(None) };
        // A ranked read names no relation; the tables its rows came from are the ones it reached.
        let mut tables = self.relations(name, args);
        if tables.is_empty() {
            let columns = value.get("columns").and_then(Value::as_array);
            if let (Some(at), Some(rows)) = (columns.and_then(|c| c.iter().position(|c| c == "_table")), value.get("rows").and_then(Value::as_array)) {
                tables = rows.iter().filter_map(|r| r.get(at).and_then(Value::as_str).map(str::to_string)).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
            }
        }
        let snapshots = self.face.snapshot_ids(session, tables.iter().map(String::as_str))?;
        let template = |id: &str| self.face.templates().iter().find(|t| t.id == id).map(|t| t.sql.clone());
        let query = match name {
            "context.query" => string(args, "sql").ok().flatten(),
            "context.execute_query" => args.get("id").and_then(Value::as_str).and_then(template),
            "corpus.retrieve" => string(args, "query").ok().flatten(),
            "context.file" => string(args, "path").ok().flatten(),
            "context.describe" | "context.reference" => string(args, "table").ok().flatten(),
            other => template(other),
        };
        let mut attributes = Map::new();
        attributes.insert(BOUNDS_BLOCK.into(), echoed.clone());
        attributes.insert("contextful.read.snapshots".into(), json!(snapshots));
        attributes.insert("contextful.read.query".into(), json!(query));
        attributes.insert("contextful.result.sha256".into(), json!(sha256_hex(&serde_json::to_vec(value).unwrap_or_default())));
        Ok(Some(attributes))
    }

    /// The claim ids a memory read returned (`read.recall.usage-ledger`): every row's
    /// `claim_id` of a `memory.recall` answer and of a knowledge card's claims, and each
    /// `memory_facts` row's of a `corpus.retrieve` answer returning one. Any other read,
    /// and a card that read no claims, returns `None`.
    fn claims(&self, name: &str, value: &Value) -> Option<Vec<String>> {
        if name == "memory.explain" {
            return self.claims("memory.recall", value.get("claims")?);
        }
        let columns = value.get("columns")?.as_array()?;
        let at = |column: &str| columns.iter().position(|c| c == column);
        let rows = value.get("rows")?.as_array()?;
        let id = |v: &Value| v.as_str().map(str::to_string);
        match name {
            "memory.recall" => {
                let i = at("claim_id")?;
                Some(rows.iter().filter_map(|r| r.get(i).and_then(id)).collect())
            }
            "corpus.retrieve" => {
                let (table, row) = (at("_table")?, at("_row")?);
                let memory = self.face.memory();
                let claims: Vec<String> = rows
                    .iter()
                    .filter(|r| r.get(table).and_then(Value::as_str).and_then(|t| memory.table(t)).is_some_and(|t| t.shape == Shape::Facts))
                    .filter_map(|r| r.get(row).and_then(|o| o.get("claim_id")).and_then(id))
                    .collect();
                (!claims.is_empty()).then_some(claims)
            }
            _ => None,
        }
    }

    /// The relations a read names: the base relations its statement's text names, parsed
    /// without the session's registrations so a refused read names them too, or the
    /// `table` argument of a tool reading one table.
    fn relations(&self, name: &str, args: &Map<String, Value>) -> Vec<String> {
        let template = |id: &str| self.face.templates().iter().find(|t| t.id == id).map(|t| t.sql.clone());
        let sql = match name {
            "context.query" => args.get("sql").and_then(Value::as_str).map(str::to_string),
            "context.execute_query" => args.get("id").and_then(Value::as_str).and_then(template),
            "context.describe" | "context.reference" | "memory.recall" => return args.get("table").and_then(Value::as_str).map(str::to_string).into_iter().collect(),
            "memory.explain" => return ["table", "claims"].iter().filter_map(|k| args.get(*k).and_then(Value::as_str).map(str::to_string)).collect(),
            "context.file" => return args.get("path").and_then(Value::as_str).and_then(|path| contextful_context::read::face::preview_target(path).ok()).into_iter().collect(),
            "context.subscribe" => return args.get("tables").and_then(Value::as_array).map(|ts| ts.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default(),
            other => template(other),
        };
        sql.and_then(|sql| self.face.serialize(&sql).ok())
            .and_then(|tree| admit(&tree, |_| true).ok())
            .map(|a| a.relations.into_iter().collect())
            .unwrap_or_default()
    }

    /// Run one tool: its structured result and the rows it returns.
    fn dispatch(&self, caller: Caller<'_>, name: &str, args: &Map<String, Value>) -> Result<Result<(Value, u64, Session), ReadFault>, Protocol> {
        let zone = string(args, "zone")?;
        let zone = zone.as_deref();
        // Every read tool admits a pin map (`read.resolve-pin.pin-parameter`).
        let pins = Pins::parse(args.get(PIN_ARGUMENT)).map_err(invalid)?;
        let pins = &pins;
        if let Err(absent) = read_backend(&self.build, name) {
            return Ok(Err(absent.into()));
        }
        let horizon = horizon(caller.authority.grants());
        let inherited;
        let args = match horizon {
            // `memory.recall` names its own clocks and a knowledge card reads the latest
            // state, each reading the horizon below; a subscription takes no bound.
            Some(_) if name != "memory.recall" && name != "memory.explain" && name != "context.subscribe" => match inherit_horizon(args, horizon)? {
                Ok(a) => {
                    inherited = a;
                    &inherited
                }
                Err(refused) => return Ok(Err(refused.into())),
            },
            _ => args,
        };
        Ok(match name {
            "context.describe" => {
                only(args, name, &["table", "internals"])?;
                let table = string(args, "table")?;
                let opts = options(args)?;
                self.session(caller, zone, opts.bounds, pins).and_then(|s| self.face.describe_with_options(&s, table.as_deref(), opts).map(|v| (v, 0, s)))
            }
            "context.query" => {
                only(args, name, &["sql", "parameters", "limit", "max_duration_ms", "max_response_bytes", "internals"])?;
                let sql = required(args, "sql")?;
                let parameters = match arg(args, "parameters") {
                    None => Map::new(),
                    Some(Value::Object(m)) => m.clone(),
                    Some(_) => return Err(invalid("`parameters` is an object")),
                };
                let opts = options(args)?;
                self.session(caller, zone, opts.bounds, pins)
                    .and_then(|s| self.face.query_with(&s, &sql, &parameters, opts).map(|r| answered_in(r, s)))
            }
            "context.execute_query" => {
                only(args, name, &["id", "arguments", "limit", "max_duration_ms", "max_response_bytes", "internals"])?;
                let id = required(args, "id")?;
                let arguments = match arg(args, "arguments") {
                    None => Map::new(),
                    Some(Value::Object(m)) => m.clone(),
                    Some(_) => return Err(invalid("`arguments` is an object")),
                };
                let opts = options(args)?;
                self.template_session(caller, zone, opts.bounds, pins, &id)
                    .and_then(|s| self.face.execute_template(&s, &id, &arguments, opts).map(|r| answered_in(r, s)))
            }
            "context.reference" => {
                only(args, name, &["table", "run", "seq", "max_duration_ms", "max_response_bytes", "internals"])?;
                let table = required(args, "table")?;
                let run = required(args, "run")?;
                let seq = args.get("seq").and_then(Value::as_i64).filter(|seq| *seq >= 0)
                    .ok_or_else(|| invalid("`seq` is a non-negative integer"))?;
                let opts = options(args)?;
                self.session(caller, zone, opts.bounds, pins)
                    .and_then(|s| self.face.reference(&s, &table, &run, seq, opts).map(|r| answered_in(r, s)))
            }
            "context.files" => {
                only(args, name, &["internals"])?;
                let opts = options(args)?;
                self.session(caller, zone, opts.bounds, pins).and_then(|s| self.face.files_with_options(&s, opts).map(|r| answered_in(r, s)))
            }
            "context.file" => {
                only(args, name, &["path", "limit", "max_duration_ms", "max_response_bytes", "internals"])?;
                let path = required(args, "path")?;
                let opts = options(args)?;
                self.session(caller, zone, opts.bounds, pins).and_then(|s| self.face.file(&s, &path, opts).map(|r| answered_in(r, s)))
            }
            "context.subscribe" => {
                // A subscription reads the commit record, never rows under a bound
                // (`read.register.bound-arguments`).
                if let Some(k) = args.keys().find(|k| !["tables", "after", "zone", PIN_ARGUMENT].contains(&k.as_str())) {
                    return Err(invalid(format!("`{name}` takes no argument `{k}`")));
                }
                // A notification names a commit instant and sequence the horizon withholds
                // (`read.subscribe.horizon-refusal`).
                if let Some(at) = horizon {
                    return Ok(Err(ReadError::SubscriptionUnderHorizon(format!("`{name}` serves no credential carrying horizon {}", at.to_rfc3339_nanos())).into()));
                }
                let tables = match arg(args, "tables") {
                    Some(Value::Array(xs)) if !xs.is_empty() => xs
                        .iter()
                        .map(|x| x.as_str().map(str::to_string).ok_or_else(|| invalid("`tables` lists strings")))
                        .collect::<Result<Vec<String>, _>>()?,
                    _ => return Err(invalid("`tables` is a non-empty list of strings")),
                };
                let after = match arg(args, "after") {
                    None => std::collections::BTreeMap::new(),
                    Some(Value::Object(m)) => m
                        .iter()
                        .filter(|(_, v)| !v.is_null())
                        .map(|(t, v)| v.as_str().map(|p| (t.clone(), p.to_string())).ok_or_else(|| invalid("`after` maps a table to a `<run-id>/<node-id>` string")))
                        .collect::<Result<_, _>>()?,
                    Some(_) => return Err(invalid("`after` is an object")),
                };
                if let Some(t) = tables.iter().enumerate().find_map(|(i, t)| tables[..i].contains(t).then_some(t)) {
                    return Err(invalid(format!("`tables` lists `{t}` twice")));
                }
                if let Some(t) = after.keys().find(|t| !tables.contains(t)) {
                    return Err(invalid(format!("`after` names `{t}`, which `tables` does not list")));
                }
                self.session(caller, zone, Bounds::default(), pins).and_then(|s| {
                    let page = self.face.subscribe(&s, &tables, &after)?;
                    let sent = page["messages"].as_array().map_or(0, |m| m.len() as u64);
                    Ok((page, sent, s))
                })
            }
            "corpus.retrieve" => {
                only(args, name, &["prefix", "tables", "query", "query_embedding", "filter", "kinds", "limit", "since", "min_score", "internals"])?;
                let tables = match arg(args, "tables") {
                    None => None,
                    Some(Value::Array(xs)) if !xs.is_empty() => Some(
                        xs.iter()
                            .map(|x| x.as_str().map(str::to_string).ok_or_else(|| invalid("`tables` lists strings")))
                            .collect::<Result<Vec<String>, _>>()?,
                    ),
                    Some(_) => return Err(invalid("`tables` is a non-empty list of strings")),
                };
                let kinds = match arg(args, "kinds") {
                    None => None,
                    Some(Value::Array(xs)) => Some(
                        xs.iter()
                            .map(|x| x.as_str().map(str::to_string).ok_or_else(|| invalid("`kinds` lists strings")))
                            .collect::<Result<Vec<String>, _>>()?,
                    ),
                    Some(_) => return Err(invalid("`kinds` is a list of strings")),
                };
                let query_embedding = match arg(args, "query_embedding") {
                    None => None,
                    Some(Value::Array(xs)) => Some(
                        xs.iter()
                            .map(|x| x.as_f64().map(|f| f as f32).ok_or_else(|| invalid("`query_embedding` lists numbers")))
                            .collect::<Result<Vec<f32>, _>>()?,
                    ),
                    Some(_) => return Err(invalid("`query_embedding` is a list of numbers")),
                };
                let b = bounds(args)?;
                let request = RetrieveRequest {
                    query_embedding,
                    // The face checks the filter's shape and budget in-band
                    // (`read.retrieve.filter-budget-refusal`).
                    filter: arg(args, "filter").cloned(),
                    tables,
                    kinds,
                    limit: integer(args, "limit")?,
                    max_duration_ms: integer(args, "max_duration_ms")?,
                    max_response_bytes: integer(args, "max_response_bytes")?,
                    since: instant(args, "since")?,
                    min_score: integer(args, "min_score")?.map(|m| u32::try_from(m).unwrap_or(u32::MAX)),
                    internals: boolean(args, "internals")?,
                    ..RetrieveRequest::new(
                        string(args, "prefix")?.unwrap_or_default(),
                        required(args, "query")?,
                        b.as_of.map_or_else(|| self.clock.now(), |a| a.at),
                    )
                };
                self.session(caller, zone, b, pins).and_then(|s| self.face.retrieve(&s, &request, b).map(|r| answered_in(r, s)))
            }
            "memory.recall" => {
                only(args, name, &["table", "subject", "observed_at", "as_of_ingest", "limit", "internals"])?;
                // The keyed read names its two clocks itself (`read.register.bound-arguments`).
                if let Some(k) = ["as_of", "valid_as_of", "available_as_of", VINTAGE].into_iter().find(|k| args.contains_key(*k)) {
                    return Err(invalid(format!("`{name}` takes no argument `{k}`; it reads `observed_at` and `as_of_ingest`")));
                }
                // Under a horizon an absent `observed_at` reads at it, and the claims table at
                // `available_as_of` equal to it (`read.horizon.recall-horizon`).
                let at_horizon = horizon.map(|at| Bound { at, inclusive: true });
                let request = RecallRequest {
                    observed_at: bound(args, "observed_at")?.or(at_horizon),
                    available_as_of: at_horizon,
                    as_of_ingest: bound(args, "as_of_ingest")?,
                    limit: integer(args, "limit")?,
                    max_duration_ms: integer(args, "max_duration_ms")?,
                    max_response_bytes: integer(args, "max_response_bytes")?,
                    internals: boolean(args, "internals")?,
                    ..RecallRequest::new(required(args, "table")?, required(args, "subject")?, self.clock.now())
                };
                self.session(caller, zone, request.bounds(), pins).and_then(|s| self.face.recall(&s, &request).map(|r| answered_in(r, s)))
            }
            "memory.explain" => {
                only(args, name, &["table", "reference", "claims", "internals"])?;
                // A knowledge card reads the latest state (`read.register.bound-arguments`).
                if let Some(k) = ["as_of", "valid_as_of", "available_as_of", VINTAGE].into_iter().find(|k| args.contains_key(*k)) {
                    return Err(invalid(format!("`{name}` takes no argument `{k}`")));
                }
                let request = ExplainRequest {
                    claims: string(args, "claims")?,
                    available_as_of: horizon.map(|at| Bound { at, inclusive: true }),
                    max_duration_ms: integer(args, "max_duration_ms")?,
                    max_response_bytes: integer(args, "max_response_bytes")?,
                    internals: boolean(args, "internals")?,
                    ..ExplainRequest::new(required(args, "table")?, required(args, "reference")?, self.clock.now())
                };
                self.session(caller, zone, request.bounds(), pins).and_then(|s| {
                    let card = self.face.explain(&s, &request)?;
                    let rows = u64::from(!card["entity"].is_null()) + card["claims"]["rows"].as_array().map_or(0, |r| r.len() as u64);
                    Ok((card, rows, s))
                })
            }
            template if self.face.templates().iter().any(|t| t.id == template) => {
                // No template parameter takes a read argument's name (`read.guard.template-reserved-parameter`).
                let mut arguments = args.clone();
                for key in READ_ARGUMENTS {
                    arguments.remove(key);
                }
                // `vintage` is no reserved name: a template declaring that parameter keeps it.
                if !self.face.templates().iter().any(|t| t.id == template && t.parameters.iter().any(|p| p.name == VINTAGE)) {
                    arguments.remove(VINTAGE);
                }
                let opts = options(args)?;
                self.template_session(caller, zone, opts.bounds, pins, template)
                    .and_then(|s| self.face.execute_template(&s, template, &arguments, opts).map(|r| answered_in(r, s)))
            }
            other => return Err(invalid(format!("no tool `{other}`"))),
        })
    }
}
