//! The read face over one store: sessions, statements, templates, descriptions, file
//! listings and previews. Every function returning a stored row takes the [`Session`] an
//! admitted authority opened, so a row path skipping enforcement does not type-check.

use super::engine::{SqlEngine, ENGINE};
use super::fault::ReadFault;
use super::pool::{self, SessionPool};
use crate::scan::scan;
use crate::store::Store;
use contextful_core::grant::{authorize_template, least_row_ceiling, list_templates, raw_read_covers};
use contextful_core::read::face::TOOLS;
use contextful_core::read::guard::{admit, Admitted};
use contextful_core::read::respond::{Cell, Internals, Response};
use contextful_core::read::template::{parse_templates, Bound, QueryTemplate};
use contextful_core::read::ReadError;
use contextful_core::enforce::EnforceError;
use contextful_core::memory::declare::{DeclareError, MemoryDeclarations};
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::StoreError;
use contextful_core::store::declare::{DeclarationMalformed, TableDecl};
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

/// What a read asks for beside its statement.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReadOptions {
    /// The request's row limit.
    pub limit: Option<u64>,
    /// Return the internals block.
    pub internals: bool,
    /// The read's transaction-time and valid-time bounds (`store.bound-time`).
    pub bounds: Bounds,
}

/// The read face over one store and its manifest.
pub struct Face {
    pub(crate) store: Store,
    decls: BTreeMap<String, TableDecl>,
    policies: BTreeMap<String, TablePolicy>,
    templates: Vec<QueryTemplate>,
    memory: MemoryDeclarations,
    pepper: Pepper,
    /// Opened full-text sidecars (`read.rank.lexical-index-cache`).
    pub(crate) fulltext: crate::fulltext::SidecarCache<crate::fulltext::FulltextSidecar>,
    /// Resolved sessions and their connections (`read.cache.session-pool`).
    pub(crate) pool: SessionPool,
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
        let mut parsed = TableDecl::parse_pipeline(manifest).map_err(|e| ReadFault::Policy(e.into()))?;
        let memory = MemoryDeclarations::parse(manifest).map_err(|e| match e {
            DeclareError::Memory(m) => ReadFault::Refused(m.into()),
            DeclareError::Malformed(m) => ReadFault::Policy(m.into()),
        })?;
        parsed.extend(memory.tables.iter().map(|t| t.table_decl()));
        // One name has one declaration: a second would replace the first's masks, zone and
        // predicate.
        let mut names = BTreeSet::new();
        if let Some(d) = parsed.iter().find(|d| !names.insert(d.name.clone())) {
            return Err(ReadFault::Policy(DeclarationMalformed(format!("table `{}` is declared more than once", d.name)).into()));
        }
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
        let fulltext = crate::fulltext::SidecarCache::new(contextful_core::read::rank::LEXICAL_INDEX_CACHE_ENTRIES);
        let face = Face { store, decls, policies, templates, memory, pepper, fulltext, pool: SessionPool::default() };
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

    /// The manifest's memory tables and declared relation types.
    pub fn memory(&self) -> &MemoryDeclarations {
        &self.memory
    }

    /// The store the face reads.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Every row of a table the session reads, or of one of its committed runs, through
    /// the table's registered relation; an engine-composed read with no row ceiling.
    pub fn rows(&self, session: &Session, table: &str, run: Option<&str>) -> Result<Response, ReadFault> {
        let r = self.registered(session, table)?;
        let engine = self.pool.engine(session)?;
        let (sql, parameters) = match run {
            Some(run) => (format!("SELECT * FROM {} WHERE {} = ?", ident(r.name()), ident(RUN_ID)), vec![Bound::Text(run.to_string())]),
            None => (format!("SELECT * FROM {}", ident(r.name())), Vec::new()),
        };
        self.respond(&engine, &sql, &parameters, None, ReadOptions::default())
    }

    /// A table's declaration, or an undeclared table's defaults.
    pub fn decl(&self, table: &str) -> TableDecl {
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
        let ledger = crate::ledger::files(&self.store, table)?.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        Ok(match self.store.try_schema(table)? {
            Some(schema) => {
                let s = scan(&self.store, &decl, bounds)?;
                let files = s.files.iter().map(|f| self.absolute(f)).collect();
                TableSource { decl, policy, base: s.relation, files, columns: schema.columns, landed: true, ledger }
            }
            None => {
                let columns = injected_columns();
                let base = relation(&TableDecl::named(table), &[], &columns, &[], None)?;
                TableSource { decl, policy, base, files: Vec::new(), columns, landed: false, ledger }
            }
        })
    }

    /// A store-root-relative path as the absolute path a relation reads.
    fn absolute(&self, rel: &str) -> String {
        self.store.root().join(rel).to_string_lossy().into_owned()
    }

    /// Open a session for an admitted authority: one relation per table its read grants
    /// cover, compiled under the request's zone and bounds. A `valid_as_of` wraps only the
    /// tables declaring a valid-time pair (`store.bound-time.valid-as-of`); a table
    /// declaring none compiles under `as_of` alone and refuses only a read that names it
    /// ([`Face::bind_valid_time`]). A session pooled under the same whole key is reused;
    /// the key is computed before any resolution, so a commit racing the resolution lands
    /// under a key no later call computes (`read.cache.session-pool`).
    pub fn session(&self, authority: &AdmittedAuthority, request: &Request<'_>, bounds: Bounds) -> Result<Session, ReadFault> {
        let tables = self.tables()?;
        let granted: Vec<String> = tables.iter().filter(|t| raw_read_covers(authority.grants(), t)).cloned().collect();
        let principal = pool::principal(authority, request, bounds);
        let state = pool::store_state(&self.store, &tables, &granted)?;
        let transaction = Bounds { valid_as_of: None, ..bounds };
        self.pool.session(principal, state, || {
            let sources = granted
                .iter()
                .map(|t| self.source(t, if self.decl(t).valid_time.is_some() { bounds } else { transaction }))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Session::open(authority, request, sources, &self.pepper)?)
        })
    }

    /// The face's session pool (`read.cache.session-pool`).
    pub fn pool(&self) -> &SessionPool {
        &self.pool
    }

    /// Refuse a `valid_as_of` read touching a table that declares no valid-time pair
    /// (`store.bound-time.valid-time-undeclared`). The session already compiles every
    /// declaring table under `valid_as_of`, so a pooled connection is never re-registered.
    pub(crate) fn bind_valid_time(&self, touched: &BTreeSet<String>, bounds: Bounds) -> Result<(), ReadFault> {
        if bounds.valid_as_of.is_none() {
            return Ok(());
        }
        for table in touched {
            if self.decl(table).valid_time.is_none() {
                return Err(StoreError::StoreValidTimeUndeclared(format!("table `{table}` declares no valid-time pair")).into());
            }
        }
        Ok(())
    }

    /// The engine's serialization of a statement; no row is read.
    pub fn serialize(&self, sql: &str) -> Result<Value, ReadFault> {
        SqlEngine::bare()?.serialize(sql)
    }

    /// The least row ceiling over the grants, the request, a template and every touched
    /// table's published `limits.max_rows` (`read.respond.row-ceiling`). A request ledger
    /// answers to its table's ceiling.
    pub(crate) fn ceiling(&self, session: &Session, touched: &BTreeSet<String>, request: Option<u64>, template: Option<u64>) -> Option<u64> {
        let grant = session.grants().iter().filter_map(|g| g.max_rows).min();
        let owner = |t: &String| contextful_core::store::ledger::ledger_table(t).map(str::to_string).unwrap_or_else(|| t.clone());
        let table = touched.iter().filter_map(|t| session.policy(&owner(t)).and_then(|p| p.max_rows)).min();
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
        let mut response = Response::cut(columns, rows, ceiling);
        if let Some(b) = opts.bounds.echo() {
            response = response.with_block("bounds", b);
        }
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
        let engine = self.pool.engine(session)?;
        let tree = engine.serialize(sql)?;
        let admitted = admit_in(session, &tree)?;
        scope::guard(&tree, session, &[])?;
        engine.register_ledgers(session, &admitted.relations)?;
        self.bind_valid_time(&admitted.relations, opts.bounds)?;
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
        let engine = self.pool.engine(session)?;
        let tree = engine.serialize(&template.sql)?;
        let admitted = admit_in(session, &tree)?;
        scope::guard(&tree, session, &parameters)?;
        engine.register_ledgers(session, &admitted.relations)?;
        self.bind_valid_time(&admitted.relations, opts.bounds)?;
        let ceiling = self.ceiling(session, &admitted.relations, opts.limit, template.max_rows);
        self.respond(&engine, &template.sql, &parameters, ceiling, opts)
    }

    /// The tools this session sees: the closed built-in set and each declared template
    /// its grants cover, as a tool named by its identifier.
    pub fn tools(&self, session: &Session) -> Vec<Value> {
        let declared: Vec<String> = self.templates.iter().map(|t| t.id.clone()).collect();
        let allowed = list_templates(session.grants(), &declared);
        let mut tools: Vec<Value> = TOOLS.iter().map(|t| builtin_tool(t)).collect();
        tools.extend(self.templates.iter().filter(|t| allowed.contains(&t.id.as_str())).map(|t| {
            let mut tool = t.tool();
            for (name, schema) in bound_properties() {
                tool["inputSchema"]["properties"].as_object_mut().expect("a template schema lists properties").insert(name, schema);
            }
            tool
        }));
        tools
    }

    fn registered<'s>(&self, session: &'s Session, table: &str) -> Result<&'s contextful_policy::enforce::session::RegisteredRelation, ReadFault> {
        session.relation(table).ok_or_else(|| EnforceError::UnknownRelation(format!("`{table}`")).into())
    }

    /// Describe one table the session reads, or list them all. A table outside the
    /// session is absent from the listing and refused by name
    /// (`authority.refuse.ungranted-table`). `limits.max_rows` appears exactly when the
    /// engine applies it (`read.register.advertised-is-enforced`). The row count reads
    /// under `bounds`, which a bounded description echoes; a listing reads under `as_of`
    /// alone and echoes only it (`read.register.bound-listing`).
    pub fn describe(&self, session: &Session, table: Option<&str>, bounds: Bounds) -> Result<Value, ReadFault> {
        let echo = |mut v: Value, bounds: Bounds| {
            if let Some(b) = bounds.echo() {
                v["contextful.bounds"] = b;
            }
            v
        };
        let Some(table) = table else {
            let tables: Vec<Value> = session
                .relations()
                .map(|r| json!({ "table": r.name(), "description": self.decl(r.name()).agent_description }))
                .collect();
            return Ok(echo(json!({ "tables": tables }), Bounds { valid_as_of: None, ..bounds }));
        };
        let r = self.registered(session, table)?;
        self.bind_valid_time(&BTreeSet::from([table.to_string()]), bounds)?;
        let engine = self.pool.engine(session)?;
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
        Ok(echo(out, bounds))
    }

    /// Committed data files of the tables the session reads, store-root-relative; a table
    /// outside the session contributes no path (`read.register.file-listing`). Only `as_of`
    /// selects files and only it echoes (`read.register.bound-listing`).
    pub fn files(&self, session: &Session, bounds: Bounds) -> Result<Response, ReadFault> {
        let transaction = Bounds { valid_as_of: None, ..bounds };
        let mut rows = Vec::new();
        for r in session.relations() {
            if self.store.try_schema(r.name())?.is_none() {
                continue;
            }
            for f in scan(&self.store, &self.decl(r.name()), transaction)?.files {
                rows.push(vec![json!(r.name()), json!(f)]);
            }
        }
        let response = Response::cut(vec!["table".into(), "path".into()], rows, None);
        Ok(match transaction.echo() {
            Some(b) => response.with_block("bounds", b),
            None => response,
        })
    }

    /// Preview one committed run file through its table's registered relation. A snapshot
    /// part, a traversal, an absolute path or a ledger file resolves to no table
    /// (`read.register.file-preview-target`). Under `as_of` the file is one the bound
    /// reaches; under `valid_as_of` its rows are those valid at the instant.
    pub fn file(&self, session: &Session, path: &str, opts: ReadOptions) -> Result<Response, ReadFault> {
        let table = preview_target(path)?;
        self.registered(session, &table)?;
        let decl = self.decl(&table);
        let transaction = Bounds { valid_as_of: None, ..opts.bounds };
        if !scan(&self.store, &decl, transaction)?.files.iter().any(|f| f == path) {
            return Err(ReadError::FilePreviewNotATable(format!("`{path}` is no committed data file of `{table}`")).into());
        }
        let file = self.absolute(path);
        let schema = self.store.schema(&table)?;
        let carried = crate::parquet_io::columns(std::path::Path::new(&file))?;
        let absent: Vec<Column> = schema.columns.iter().filter(|c| !carried.contains(&c.name)).cloned().collect();
        let base = relation(&decl, std::slice::from_ref(&file), &schema.columns, &absent, opts.bounds.valid_as_of)?;
        let preview = session.relation_over(&table, &base, vec![file]).expect("a registered table carries its source");
        let engine = SqlEngine::open(session)?;
        engine.register(PREVIEW_RELATION, preview.sql())?;
        let touched = BTreeSet::from([table]);
        let ceiling = self.ceiling(session, &touched, opts.limit, None);
        self.respond(&engine, &format!("SELECT * FROM {}", ident(PREVIEW_RELATION)), &[], ceiling, opts)
    }
}

/// Admit a statement over the session's registered relations. A session that is no owner
/// read naming the request ledger of a table it reads raises `LedgerNotTenantScoped`
/// rather than an unknown relation (`read.register.scoped-ledger`).
fn admit_in(session: &Session, tree: &Value) -> Result<Admitted, ReadFault> {
    let closed = std::cell::RefCell::new(None);
    let admitted = admit(tree, |name| {
        if let Some((table, reason)) = session.closed_ledger(name) {
            *closed.borrow_mut() = Some((name.to_string(), table.to_string(), reason));
            return false;
        }
        session.reads(name)
    });
    match (admitted, closed.into_inner()) {
        (Err(_), Some((name, table, reason))) => Err(ReadError::LedgerNotTenantScoped(format!(
            "`{name}` is closed: {reason}, and the request ledger of `{table}` carries no tenant or row column to narrow \
             on, so it registers on the owner read alone"
        ))
        .into()),
        (admitted, _) => Ok(admitted?),
    }
}

/// The relation one preview reads: the named file under its table's every step.
const PREVIEW_RELATION: &str = "__contextful_preview";

/// The table a run file's store-root-relative path belongs to.
fn preview_target(path: &str) -> Result<String, ReadError> {
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
        [_run, _node, file] if file.ends_with(".parquet") => Ok(table.to_string()),
        _ => Err(refuse("is no run part")),
    }
}

/// A built-in tool's definition.
fn builtin_tool(name: &str) -> Value {
    let (description, mut properties, required): (&str, Value, Vec<&str>) = match name {
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
    for (bound, schema) in bound_properties() {
        properties[bound.as_str()] = schema;
    }
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
    })
}

/// The two bound arguments every read tool declares (`read.register.bound-arguments`).
fn bound_properties() -> [(String, Value); 2] {
    let instant = || json!({ "type": "string", "description": "An RFC 3339 instant, or a YYYY-MM-DD date read as the start of the next day, exclusive." });
    [("as_of".into(), instant()), ("valid_as_of".into(), instant())]
}
