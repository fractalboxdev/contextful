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

use contextful_context::read::{Face, ReadFault, ReadOptions, RecallRequest, RetrieveRequest};
use contextful_core::read::face::{register_tool, require, BuildIdentity, FaceScope, ToolKind, TOOLS};
use contextful_core::read::guard::admit;
use contextful_core::read::pin::{Pins, PIN_ARGUMENT};
use contextful_core::read::template::READ_ARGUMENTS;
use contextful_core::read::respond::Response;
use contextful_core::read::Refusal;
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_core::ports::Clock;
use contextful_core::time::Instant;
use contextful_core::ports::SigningPort;
use contextful_core::AuthorityError;
use contextful_policy::audit::{attr, AuditError, AuditLog};
use contextful_policy::enforce::refuse::{payload, unpersisted};
use contextful_policy::enforce::session::Request;
use contextful_policy::verify::AdmittedAuthority;
use serde_json::{json, Map, Value};
use std::io::{BufRead, Write};

/// The tool-protocol revision this server speaks when a client names none.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// JSON-RPC error codes.
const PARSE_ERROR: i64 = -32700;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// What this binary links, reported at the handshake (`read.embed.build-identity`): the
/// embedded SQL engine and the lexical ranker, and the `http` face where the `http`
/// feature links the network transport. It links no vector index and no connector family
/// behind a feature.
pub fn build_identity() -> BuildIdentity {
    let faces = if cfg!(feature = "http") { vec!["http".into()] } else { Vec::new() };
    BuildIdentity { backends: vec!["duckdb".into(), "fts".into()], connectors: Vec::new(), faces }
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
    Ok(Bounds { as_of: bound(args, "as_of")?, valid_as_of: bound(args, "valid_as_of")? })
}

/// Refuse an argument a tool does not declare; every read tool declares both bounds and `zone`
/// (`read.register.bound-arguments`).
fn only(args: &Map<String, Value>, tool: &str, known: &[&str]) -> Result<(), Protocol> {
    match args.keys().find(|k| !known.contains(&k.as_str()) && !READ_ARGUMENTS.contains(&k.as_str())) {
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

fn options(args: &Map<String, Value>) -> Result<ReadOptions, Protocol> {
    Ok(ReadOptions { limit: integer(args, "limit")?, max_duration_ms: integer(args, "max_duration_ms")?, max_response_bytes: integer(args, "max_response_bytes")?, internals: boolean(args, "internals")?, bounds: bounds(args)? })
}

/// A row-bearing response and the count of rows it returns.
fn answered(r: Response) -> (Value, u64) {
    let rows = r.rows.len() as u64;
    (r.to_json(), rows)
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
        Ok(Tools { face, clock, record })
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
        let build = build_identity();
        require(Some(&build), &wanted).map_err(|e| invalid(e.to_string()))?;
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
        let read = |rows, refusal| ReadEntry {
            tool: name,
            authority: caller.authority,
            rows,
            at: self.clock.now(),
            tables: self.relations(name, &args),
            refusal,
        };
        match self.dispatch(caller, name, &args) {
            // The result leaves only once its entry is durable; otherwise no row does.
            Ok(Ok((value, rows))) => match self.record.record(read_attributes(&read(rows, None))) {
                Ok(()) => Ok(result(value)),
                Err(e) => Ok(unrecorded(&e)),
            },
            // A refusal is recorded before it answers (`disclosure.record.refused-read`).
            Ok(Err(fault)) => match fault.refusal() {
                Some(r) => match self.record.record(read_attributes(&read(0, Some(r.identifier())))) {
                    Ok(()) => Ok(refused(r)),
                    Err(e) => Ok(unrecorded(&e)),
                },
                None => Ok(json!({ "content": [{ "type": "text", "text": fault.to_string() }], "isError": true })),
            },
            Err(protocol) => Err(protocol),
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
            "context.describe" | "memory.recall" => return args.get("table").and_then(Value::as_str).map(str::to_string).into_iter().collect(),
            other => template(other),
        };
        sql.and_then(|sql| self.face.serialize(&sql).ok())
            .and_then(|tree| admit(&tree, |_| true).ok())
            .map(|a| a.relations.into_iter().collect())
            .unwrap_or_default()
    }

    /// Run one tool: its structured result and the rows it returns.
    fn dispatch(&self, caller: Caller<'_>, name: &str, args: &Map<String, Value>) -> Result<Result<(Value, u64), ReadFault>, Protocol> {
        let zone = string(args, "zone")?;
        let zone = zone.as_deref();
        // Every read tool admits a pin map (`read.resolve-pin.pin-parameter`).
        let pins = Pins::parse(args.get(PIN_ARGUMENT)).map_err(invalid)?;
        let pins = &pins;
        Ok(match name {
            "context.describe" => {
                only(args, name, &["table"])?;
                let table = string(args, "table")?;
                let opts = options(args)?;
                self.session(caller, zone, opts.bounds, pins).and_then(|s| self.face.describe_with_options(&s, table.as_deref(), opts)).map(|v| (v, 0))
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
                    .and_then(|s| self.face.query_with(&s, &sql, &parameters, opts))
                    .map(answered)
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
                self.session(caller, zone, opts.bounds, pins)
                    .and_then(|s| self.face.execute_template(&s, &id, &arguments, opts))
                    .map(answered)
            }
            "context.reference" => {
                only(args, name, &["table", "run", "seq", "max_duration_ms", "max_response_bytes"])?;
                let table = required(args, "table")?;
                let run = required(args, "run")?;
                let seq = args.get("seq").and_then(Value::as_i64).filter(|seq| *seq >= 0)
                    .ok_or_else(|| invalid("`seq` is a non-negative integer"))?;
                let opts = options(args)?;
                self.session(caller, zone, opts.bounds, pins)
                    .and_then(|session| self.face.reference(&session, &table, &run, seq, opts)).map(answered)
            }
            "context.files" => {
                only(args, name, &[])?;
                let opts = options(args)?;
                self.session(caller, zone, opts.bounds, pins).and_then(|s| self.face.files_with_options(&s, opts)).map(answered)
            }
            "context.file" => {
                only(args, name, &["path", "limit", "max_duration_ms", "max_response_bytes", "internals"])?;
                let path = required(args, "path")?;
                let opts = options(args)?;
                self.session(caller, zone, opts.bounds, pins).and_then(|s| self.face.file(&s, &path, opts)).map(answered)
            }
            "corpus.retrieve" => {
                only(args, name, &["prefix", "query", "query_embedding", "filter", "kinds", "limit", "since", "min_score", "internals"])?;
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
                self.session(caller, zone, b, pins).and_then(|s| self.face.retrieve(&s, &request, b)).map(answered)
            }
            "memory.recall" => {
                only(args, name, &["table", "subject", "observed_at", "as_of_ingest", "limit"])?;
                // The keyed read names its two clocks itself (`read.register.bound-arguments`).
                if let Some(k) = ["as_of", "valid_as_of"].into_iter().find(|k| args.contains_key(*k)) {
                    return Err(invalid(format!("`{name}` takes no argument `{k}`; it reads `observed_at` and `as_of_ingest`")));
                }
                let request = RecallRequest {
                    observed_at: bound(args, "observed_at")?,
                    as_of_ingest: bound(args, "as_of_ingest")?,
                    limit: integer(args, "limit")?,
                    max_duration_ms: integer(args, "max_duration_ms")?,
                    max_response_bytes: integer(args, "max_response_bytes")?,
                    ..RecallRequest::new(required(args, "table")?, required(args, "subject")?, self.clock.now())
                };
                self.session(caller, zone, request.bounds(), pins).and_then(|s| self.face.recall(&s, &request)).map(answered)
            }
            template if self.face.templates().iter().any(|t| t.id == template) => {
                // No template parameter takes a read argument's name (`read.guard.template-reserved-parameter`).
                let mut arguments = args.clone();
                for key in READ_ARGUMENTS {
                    arguments.remove(key);
                }
                let opts = options(args)?;
                self.session(caller, zone, opts.bounds, pins)
                    .and_then(|s| self.face.execute_template(&s, template, &arguments, opts))
                    .map(answered)
            }
            other => return Err(invalid(format!("no tool `{other}`"))),
        })
    }
}
