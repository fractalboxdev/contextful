//! The read face over one store: sessions, statements, templates, descriptions, file
//! listings and previews. Every function returning a stored row takes the [`Session`] an
//! admitted authority opened, so a row path skipping enforcement does not type-check.

use super::engine::{SqlEngine, ENGINE};
use super::fault::ReadFault;
use crate::scan::scan;
use crate::store::Store;
use contextful_core::grant::{authorize_template, least_row_ceiling, list_templates, raw_read_covers};
use contextful_core::read::face::TOOLS;
use contextful_core::read::guard::{admit, Admitted};
use contextful_core::read::rank::LexicalIndexCache;
use contextful_core::read::respond::{Cell, Internals, Response};
use contextful_core::read::template::{parse_templates, Bound, QueryTemplate};
use contextful_core::read::ReadError;
use contextful_core::enforce::EnforceError;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::reconcile::{Column, ColumnType};
use contextful_core::store::relation::{ident, relation};
use contextful_core::store::reserve::{INGESTED_AT, ROW_SEQ, RUN_ID, SITE_ID};
use contextful_policy::enforce::mask::Pepper;
use contextful_policy::enforce::policy::TablePolicy;
use contextful_policy::enforce::scope;
use contextful_policy::enforce::session::{Request, Session, TableSource};
use contextful_policy::verify::AdmittedAuthority;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

/// What a read asks for beside its statement.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReadOptions {
    /// The request's row limit.
    pub limit: Option<u64>,
    /// Return the internals block.
    pub internals: bool,
}

/// The read face over one store and its manifest.
pub struct Face {
    pub(crate) store: Store,
    decls: BTreeMap<String, TableDecl>,
    policies: BTreeMap<String, TablePolicy>,
    templates: Vec<QueryTemplate>,
    pepper: Pepper,
    pub(crate) lexical: Mutex<LexicalIndexCache>,
}

/// The columns a table with no landed batch registers over: the injected columns every
/// write path carries.
fn injected_columns() -> Vec<Column> {
    vec![
        Column::new(INGESTED_AT, ColumnType::Timestamp, false),
        Column::new(RUN_ID, ColumnType::Utf8, false),
        Column::new(ROW_SEQ, ColumnType::Int64, false),
        Column::new(SITE_ID, ColumnType::Utf8, false),
    ]
}

impl Face {
    /// Open the face at startup. Every table policy, every template and every mask
    /// against its table's schema is checked here, once, caller-independently; one
    /// failure refuses the whole manifest (`read.guard.startup-time-check`).
    pub fn open(store: Store, manifest: &str, pepper: Pepper) -> Result<Face, ReadFault> {
        let parsed = TableDecl::parse_pipeline(manifest).map_err(|e| ReadFault::Policy(e.into()))?;
        let mut decls = BTreeMap::new();
        let mut policies = BTreeMap::new();
        for d in parsed {
            let policy = TablePolicy::from_decl(&d)?;
            if let Some(schema) = store.try_schema(&d.name)? {
                policy.check_schema(&d.name, &schema.columns)?;
            }
            policies.insert(d.name.clone(), policy);
            decls.insert(d.name.clone(), d);
        }
        let templates = parse_templates(manifest).map_err(|e| ReadFault::Policy(e.into()))?;
        let face = Face { store, decls, policies, templates, pepper, lexical: Mutex::new(LexicalIndexCache::default()) };
        let tables = face.tables()?;
        let engine = SqlEngine::bare()?;
        for t in &face.templates {
            t.check(&engine.serialize(&t.sql)?, &tables)?;
        }
        Ok(face)
    }

    /// Every table the store holds or the manifest declares, sorted.
    pub fn tables(&self) -> Result<Vec<String>, ReadFault> {
        let mut all: BTreeSet<String> = self.store.tables()?.into_iter().collect();
        all.extend(self.decls.keys().cloned());
        Ok(all.into_iter().collect())
    }

    pub fn templates(&self) -> &[QueryTemplate] {
        &self.templates
    }

    pub(crate) fn decl(&self, table: &str) -> TableDecl {
        self.decls.get(table).cloned().unwrap_or_else(|| TableDecl::named(table))
    }

    /// One table under the request's bounds. A table no batch has landed in registers as
    /// a zero-row relation over the injected columns (`read.register.quiet-table`).
    fn source(&self, table: &str, bounds: Bounds) -> Result<TableSource, ReadFault> {
        let decl = self.decl(table);
        let policy = match self.policies.get(table) {
            Some(p) => p.clone(),
            None => TablePolicy::from_decl(&decl)?,
        };
        let (base, columns) = match self.store.try_schema(table)? {
            Some(schema) => (scan(&self.store, &decl, bounds)?.relation, schema.columns),
            None => {
                let columns = injected_columns();
                (relation(&TableDecl::named(table), &[], &columns, &[], None)?, columns)
            }
        };
        Ok(TableSource { decl, policy, base, columns })
    }

    /// Open a session for an admitted authority: one relation per table its read grants
    /// cover, compiled under the request's zone and bounds.
    pub fn session(&self, authority: &AdmittedAuthority, request: &Request<'_>, bounds: Bounds) -> Result<Session, ReadFault> {
        let mut sources = Vec::new();
        for t in self.tables()? {
            if raw_read_covers(authority.grants(), &t) {
                sources.push(self.source(&t, bounds.clone())?);
            }
        }
        Ok(Session::open(authority, request, sources, &self.pepper)?)
    }

    /// The engine's serialization of a statement; no row is read.
    pub fn serialize(&self, sql: &str) -> Result<Value, ReadFault> {
        SqlEngine::bare()?.serialize(sql)
    }

    /// The least row ceiling over the grants, the request, a template and every touched
    /// table's published `limits.max_rows` (`read.respond.row-ceiling`).
    fn ceiling(&self, session: &Session, touched: &BTreeSet<String>, request: Option<u64>, template: Option<u64>) -> Option<u64> {
        let grant = session.grants().iter().filter_map(|g| g.max_rows).min();
        let table = touched.iter().filter_map(|t| session.policy(t).and_then(|p| p.max_rows)).min();
        least_row_ceiling([grant, request, template, table])
    }

    fn respond(
        &self,
        engine: &SqlEngine,
        sql: &str,
        parameters: &[Bound],
        ceiling: Option<u64>,
        opts: ReadOptions,
    ) -> Result<Response, ReadFault> {
        let started = std::time::Instant::now();
        let (columns, rows) = engine.run(sql, parameters, Response::fetch_count(ceiling))?;
        let rows: Vec<Vec<Value>> = rows.iter().map(|r| r.iter().map(Cell::to_json).collect()).collect();
        let response = Response::cut(columns, rows, ceiling);
        Ok(if opts.internals {
            let internals = Internals {
                sql: sql.to_string(),
                engine: ENGINE,
                limit: ceiling,
                row_count: response.rows.len() as u64,
                elapsed_ms: started.elapsed().as_millis() as u64,
            };
            response.with_block("internals", serde_json::to_value(internals).expect("internals serialize"))
        } else {
            response
        })
    }

    /// Admit and run caller-written SQL: exactly one read-only `SELECT` over this
    /// session's registered relations, the scope guard over its tenant literals, then
    /// execution under the least row ceiling.
    pub fn query(&self, session: &Session, sql: &str, opts: ReadOptions) -> Result<Response, ReadFault> {
        let engine = SqlEngine::open(session)?;
        let tree = engine.serialize(sql)?;
        let admitted: Admitted = admit(&tree, |name| session.reads(name))?;
        scope::guard(&tree, session, &[])?;
        let ceiling = self.ceiling(session, &admitted.relations, opts.limit, None);
        self.respond(&engine, sql, &[], ceiling, opts)
    }

    /// Run a declared template the credential's allowlist covers. Its body is operator
    /// text and runs unrewritten; each bare table name in it resolves to the caller's
    /// registered relation (`read.register.bare-name`).
    pub fn execute_template(&self, session: &Session, id: &str, arguments: &Map<String, Value>, opts: ReadOptions) -> Result<Response, ReadFault> {
        let declared: Vec<String> = self.templates.iter().map(|t| t.id.clone()).collect();
        authorize_template(session.grants(), id, &declared)?;
        let template = self.templates.iter().find(|t| t.id == id).expect("an authorized template is declared");
        let parameters = template.bind(arguments)?;
        let engine = SqlEngine::open(session)?;
        let tree = engine.serialize(&template.sql)?;
        let admitted = admit(&tree, |name| session.reads(name))?;
        scope::guard(&tree, session, &parameters)?;
        let ceiling = self.ceiling(session, &admitted.relations, opts.limit, template.max_rows);
        self.respond(&engine, &template.sql, &parameters, ceiling, opts)
    }

    /// The tools this session sees: the closed built-in set and each declared template
    /// its grants cover, as a tool named by its identifier.
    pub fn tools(&self, session: &Session) -> Vec<Value> {
        let declared: Vec<String> = self.templates.iter().map(|t| t.id.clone()).collect();
        let allowed = list_templates(session.grants(), &declared);
        let mut tools: Vec<Value> = TOOLS.iter().map(|t| builtin_tool(t)).collect();
        tools.extend(self.templates.iter().filter(|t| allowed.contains(&t.id.as_str())).map(QueryTemplate::tool));
        tools
    }

    fn registered<'s>(&self, session: &'s Session, table: &str) -> Result<&'s contextful_policy::enforce::session::RegisteredRelation, ReadFault> {
        session.relation(table).ok_or_else(|| EnforceError::UnknownRelation(format!("`{table}`")).into())
    }

    /// Describe one table the session reads, or list them all. A table outside the
    /// session is absent from the listing and refused by name
    /// (`authority.refuse.ungranted-table`). `limits.max_rows` appears exactly when the
    /// engine applies it (`read.register.advertised-is-enforced`).
    pub fn describe(&self, session: &Session, table: Option<&str>) -> Result<Value, ReadFault> {
        let Some(table) = table else {
            let tables: Vec<Value> = session
                .relations()
                .map(|r| json!({ "table": r.name(), "description": self.decl(r.name()).agent_description }))
                .collect();
            return Ok(json!({ "tables": tables }));
        };
        let r = self.registered(session, table)?;
        let engine = SqlEngine::open(session)?;
        let (_, count) = engine.run(&format!("SELECT count(*) FROM {}", ident(r.name())), &[], None)?;
        let row_count = count.first().and_then(|r| r.first()).map(Cell::to_json).unwrap_or(Value::Null);
        let decl = self.decl(table);
        let policy = session.policy(table).expect("a registered table carries its policy");
        let schema = self.store.try_schema(table)?.map(|s| s.columns).unwrap_or_else(injected_columns);
        let columns: Vec<Value> = schema.iter().map(|c| json!({ "name": c.name, "type": c.ty.name() })).collect();
        let fingerprint: String = {
            let text: Vec<String> = schema.iter().map(|c| format!("{}:{}", c.name, c.ty.name())).collect();
            Sha256::digest(text.join("\n").as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
        };
        let mut out = json!({
            "table": table,
            "row_count": row_count,
            "schema_fingerprint": fingerprint,
            "description": decl.agent_description,
            "hint": decl.agent_hint,
            "columns": columns,
            "indexes": [],
            "partition_by": decl.partition_by(),
            "zone": policy.placement.effective().labels(),
            "lexicon": {},
            "example_queries": decl.example_queries.clone().unwrap_or_default(),
        });
        if let Some(max) = policy.max_rows {
            out["limits"] = json!({ "max_rows": max });
        }
        Ok(out)
    }

    /// Committed data files of the tables the session reads, store-root-relative; a table
    /// outside the session contributes no path (`read.register.file-listing`).
    pub fn files(&self, session: &Session, bounds: Bounds) -> Result<Response, ReadFault> {
        let mut rows = Vec::new();
        for r in session.relations() {
            if self.store.try_schema(r.name())?.is_none() {
                continue;
            }
            for f in scan(&self.store, &self.decl(r.name()), bounds.clone())?.files {
                rows.push(vec![json!(r.name()), json!(f)]);
            }
        }
        Ok(Response::cut(vec!["table".into(), "path".into()], rows, None))
    }

    /// Preview one committed run file through its table's registered relation. A snapshot
    /// part, a traversal, an absolute path or a ledger file resolves to no table
    /// (`read.register.file-preview-target`).
    pub fn file(&self, session: &Session, path: &str, opts: ReadOptions) -> Result<Response, ReadFault> {
        let (table, run) = preview_target(path)?;
        let r = self.registered(session, &table)?;
        let engine = SqlEngine::open(session)?;
        let sql = format!("SELECT * FROM {} WHERE {} = ?", ident(r.name()), ident(RUN_ID));
        let touched = BTreeSet::from([table]);
        let ceiling = self.ceiling(session, &touched, opts.limit, None);
        self.respond(&engine, &sql, &[Bound::Text(run)], ceiling, opts)
    }
}

/// `(table, run_id)` of a committed run file's store-root-relative path.
fn preview_target(path: &str) -> Result<(String, String), ReadError> {
    let refuse = |why: &str| ReadError::FilePreviewNotATable(format!("`{path}` {why}"));
    if path.starts_with('/') || path.contains('\\') || path.split('/').any(|s| s == ".." || s == "." || s.is_empty()) {
        return Err(refuse("is not a store-root-relative path"));
    }
    let rest = path.strip_prefix("tables/").ok_or_else(|| refuse("lies outside `tables/`"))?;
    if rest.contains("/requests/") {
        return Err(refuse("is a request-ledger file"));
    }
    if rest.contains("/data/snapshots/") {
        return Err(refuse("is a snapshot part, which no single run owns"));
    }
    let (table, run_part) = rest.split_once("/data/runs/").ok_or_else(|| refuse("is no run file"))?;
    match run_part.split('/').collect::<Vec<_>>().as_slice() {
        [run, _node, file] if file.ends_with(".parquet") => Ok((table.to_string(), run.to_string())),
        _ => Err(refuse("is no run part")),
    }
}

/// A built-in tool's definition.
fn builtin_tool(name: &str) -> Value {
    let (description, properties, required): (&str, Value, Vec<&str>) = match name {
        "context.describe" => (
            "Describe one table this credential reads, or list them.",
            json!({ "table": { "type": "string" }, "zone": { "type": "string" } }),
            vec![],
        ),
        "context.query" => (
            "Run one read-only SELECT over the tables this credential reads.",
            json!({ "sql": { "type": "string" }, "limit": { "type": "integer" }, "internals": { "type": "boolean" }, "zone": { "type": "string" } }),
            vec!["sql"],
        ),
        "context.execute_query" => (
            "Run a declared query template by identifier.",
            json!({ "id": { "type": "string" }, "arguments": { "type": "object" }, "limit": { "type": "integer" }, "internals": { "type": "boolean" }, "zone": { "type": "string" } }),
            vec!["id"],
        ),
        "context.files" => (
            "List committed data files of the tables this credential reads.",
            json!({ "as_of": { "type": "string" }, "zone": { "type": "string" } }),
            vec![],
        ),
        "context.file" => (
            "Preview one committed run file through its table's relation.",
            json!({ "path": { "type": "string" }, "limit": { "type": "integer" }, "internals": { "type": "boolean" }, "zone": { "type": "string" } }),
            vec!["path"],
        ),
        _ => (
            "Ranked rows across the tables under a prefix.",
            json!({
                "prefix": { "type": "string" }, "query": { "type": "string" },
                "query_embedding": { "type": "array", "items": { "type": "number" } },
                "limit": { "type": "integer" }, "as_of": { "type": "string" }, "since": { "type": "string" },
                "min_score": { "type": "integer" }, "internals": { "type": "boolean" }, "zone": { "type": "string" }
            }),
            vec!["query"],
        ),
    };
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
    })
}
