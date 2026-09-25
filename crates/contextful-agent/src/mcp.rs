//! The tool protocol over standard input and output: one JSON-RPC 2.0 message per line.
//!
//! The server holds one admitted authority for its lifetime and re-reads it at every tool
//! call before any row is read. A refusal arrives in-band — a result flagged as an error
//! under transport success — carrying the refusal's wire payload
//! (`read.respond.in-band-error`).

use contextful_context::read::{Face, ReadFault, ReadOptions, RetrieveRequest};
use contextful_core::read::face::{register_tool, require, BuildIdentity, FaceScope, ToolKind, TOOLS};
use contextful_core::read::Refusal;
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_core::time::Instant;
use contextful_core::AuthorityError;
use contextful_policy::enforce::refuse::payload;
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
/// embedded SQL engine and the lexical ranker. It links no vector index, no connector
/// family behind a feature, and no other face.
pub fn build_identity() -> BuildIdentity {
    BuildIdentity { backends: vec!["duckdb".into(), "fts".into()], connectors: Vec::new(), faces: Vec::new() }
}

/// The re-check an effect boundary runs against the carried authority.
pub type Boundary<'a> = dyn Fn(&AdmittedAuthority) -> Result<(), AuthorityError> + 'a;

pub struct Server<'a> {
    face: &'a Face,
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

/// Refuse an argument a tool does not declare.
fn only(args: &Map<String, Value>, tool: &str, known: &[&str]) -> Result<(), Protocol> {
    match args.keys().find(|k| !known.contains(&k.as_str())) {
        Some(k) => Err(invalid(format!("`{tool}` takes no argument `{k}`"))),
        None => Ok(()),
    }
}

fn bounds(args: &Map<String, Value>) -> Result<Bounds, Protocol> {
    let as_of = string(args, "as_of")?.map(|s| Bound::parse(&s)).transpose().map_err(|e| invalid(e.to_string()))?;
    Ok(Bounds { as_of, valid_as_of: None })
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
    Ok(ReadOptions { limit: integer(args, "limit")?, internals: boolean(args, "internals")? })
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
    /// A server for one admitted authority. Every built-in tool registers as a read tool
    /// (`authority.resist.read-only-face`).
    pub fn new(face: &'a Face, authority: AdmittedAuthority, boundary: &'a Boundary<'a>) -> Result<Server<'a>, String> {
        for tool in TOOLS {
            register_tool(FaceScope::Organization, tool, ToolKind::Read).map_err(|e| e.to_string())?;
        }
        Ok(Server { face, authority, boundary })
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
        let message: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => return Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": PARSE_ERROR, "message": e.to_string() } })),
        };
        let id = message.get("id").cloned()?;
        let method = message["method"].as_str().unwrap_or_default();
        let params = message.get("params").and_then(Value::as_object).cloned().unwrap_or_default();
        let answer = match method {
            "initialize" => self.initialize(&params),
            "ping" => Ok(json!({})),
            "tools/list" => self.list(),
            "tools/call" => self.call(&params),
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

    fn session(&self, zone: Option<&str>, bounds: Bounds) -> Result<contextful_policy::enforce::session::Session, ReadFault> {
        (self.boundary)(&self.authority)?;
        self.face.session(&self.authority, &Request { zone }, bounds)
    }

    fn list(&self) -> Result<Value, Protocol> {
        match self.session(None, Bounds::default()) {
            Ok(s) => Ok(json!({ "tools": self.face.tools(&s) })),
            Err(fault) => Err(Protocol(INVALID_PARAMS, fault.to_string())),
        }
    }

    fn call(&self, params: &Map<String, Value>) -> Result<Value, Protocol> {
        let name = params.get("name").and_then(Value::as_str).ok_or_else(|| invalid("`name` is required"))?;
        let args = match params.get("arguments") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => return Err(invalid("`arguments` is an object")),
        };
        match self.dispatch(name, &args) {
            Ok(Ok(value)) => Ok(result(value)),
            Ok(Err(fault)) => match fault.refusal() {
                Some(r) => Ok(refused(r)),
                None => Ok(json!({ "content": [{ "type": "text", "text": fault.to_string() }], "isError": true })),
            },
            Err(protocol) => Err(protocol),
        }
    }

    fn dispatch(&self, name: &str, args: &Map<String, Value>) -> Result<Result<Value, ReadFault>, Protocol> {
        let zone = string(args, "zone")?;
        let zone = zone.as_deref();
        Ok(match name {
            "context.describe" => {
                only(args, name, &["table", "zone"])?;
                let table = string(args, "table")?;
                self.session(zone, Bounds::default()).and_then(|s| self.face.describe(&s, table.as_deref()))
            }
            "context.query" => {
                only(args, name, &["sql", "limit", "internals", "zone"])?;
                let sql = required(args, "sql")?;
                let opts = options(args)?;
                self.session(zone, Bounds::default()).and_then(|s| self.face.query(&s, &sql, opts)).map(|r| r.to_json())
            }
            "context.execute_query" => {
                only(args, name, &["id", "arguments", "limit", "internals", "zone"])?;
                let id = required(args, "id")?;
                let arguments = match arg(args, "arguments") {
                    None => Map::new(),
                    Some(Value::Object(m)) => m.clone(),
                    Some(_) => return Err(invalid("`arguments` is an object")),
                };
                let opts = options(args)?;
                self.session(zone, Bounds::default())
                    .and_then(|s| self.face.execute_template(&s, &id, &arguments, opts))
                    .map(|r| r.to_json())
            }
            "context.files" => {
                only(args, name, &["as_of", "zone"])?;
                let b = bounds(args)?;
                self.session(zone, b).and_then(|s| self.face.files(&s, b)).map(|r| r.to_json())
            }
            "context.file" => {
                only(args, name, &["path", "limit", "internals", "zone"])?;
                let path = required(args, "path")?;
                let opts = options(args)?;
                self.session(zone, Bounds::default()).and_then(|s| self.face.file(&s, &path, opts)).map(|r| r.to_json())
            }
            "corpus.retrieve" => {
                only(args, name, &["prefix", "query", "query_embedding", "limit", "as_of", "since", "min_score", "internals", "zone"])?;
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
                    prefix: string(args, "prefix")?.unwrap_or_default(),
                    query: required(args, "query")?,
                    query_embedding,
                    limit: integer(args, "limit")?,
                    since: instant(args, "since")?,
                    anchor: b.as_of.map(|a| a.at),
                    min_score: integer(args, "min_score")?.map(|m| u32::try_from(m).unwrap_or(u32::MAX)),
                    internals: boolean(args, "internals")?,
                };
                self.session(zone, b).and_then(|s| self.face.retrieve(&s, &request, b)).map(|r| r.to_json())
            }
            template if self.face.templates().iter().any(|t| t.id == template) => {
                let mut arguments = args.clone();
                arguments.remove("zone");
                self.session(zone, Bounds::default())
                    .and_then(|s| self.face.execute_template(&s, template, &arguments, ReadOptions::default()))
                    .map(|r| r.to_json())
            }
            other => return Err(invalid(format!("no tool `{other}`"))),
        })
    }
}
