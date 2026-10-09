//! The read face over one store: sessions, statements, templates, descriptions, file
//! listings and previews. Every function returning a stored row takes the [`Session`] an
//! admitted authority opened, so a row path skipping enforcement does not type-check.

use super::engine::{SqlEngine, ENGINE};
use super::fault::ReadFault;
use super::pool::{self, SessionPool};
use super::results::{self, ResultCache};
use crate::scan::{scan, scan_at};
use crate::store::Store;
use contextful_core::grant::{authorize_template, least_row_ceiling, list_templates, raw_read_covers, Grant};
use contextful_core::read::face::{estimated_tokens, TOOLS};
use contextful_core::read::guard::{admit, named_relations, Admitted};
use contextful_core::read::pin::{Pins, Resolved, PIN_ARGUMENT, RESOLVED_BLOCK};
use contextful_core::read::respond::{Cell, Internals, Response};
use contextful_core::read::template::{bind_query, parse_templates, Bindings, Bound, ParamType, QueryTemplate};
use contextful_core::read::ReadError;
use contextful_core::enforce::EnforceError;
use contextful_core::disclosure::DisclosureError;
use contextful_core::memory::declare::{DeclareError, MemoryDeclarations};
use contextful_core::pipeline::declare::ManifestFile;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::StoreError;
use contextful_core::store::declare::{DeclarationMalformed, TableDecl};
use contextful_core::store::reconcile::{Column, ColumnType};
use contextful_core::store::relation::{ident, relation, relation_with_encryption};
use contextful_core::store::reserve::{COMMIT_SEQ, INGESTED_AT, ROW_SEQ, RUN_ID, SITE_ID};
use contextful_policy::enforce::mask::Pepper;
use contextful_policy::enforce::policy::TablePolicy;
use contextful_policy::enforce::scope;
use contextful_policy::enforce::session::{Request, Session, TableSource};
use contextful_policy::verify::AdmittedAuthority;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// A registered row statement whose scope the face admits. Its fields and constructor
/// stay in this module; the native execution port accepts no caller-written SQL.
pub(crate) struct AdmittedRows<'a> {
    session: &'a Session,
    sql: String,
    parameters: Bindings,
}

impl AdmittedRows<'_> {
    pub(crate) fn session(&self) -> &Session { self.session }
    pub(crate) fn sql(&self) -> &str { &self.sql }
    pub(crate) fn parameters(&self) -> &Bindings { &self.parameters }
}

/// What a read asks for beside its statement.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReadOptions {
    /// The request's row limit.
    pub limit: Option<u64>,
    /// The request's statement deadline in milliseconds.
    pub max_duration_ms: Option<u64>,
    /// The request's serialized response ceiling in bytes.
    pub max_response_bytes: Option<u64>,
    /// Return the internals block.
    pub internals: bool,
    /// The read's transaction-time and valid-time bounds (`store.bound-time`).
    pub bounds: Bounds,
}

/// The read face over one store and its manifest.
pub struct Face {
    pub(crate) store: Store,
    lexicon: Lexicon,
    decls: BTreeMap<String, TableDecl>,
    policies: BTreeMap<String, TablePolicy>,
    templates: Vec<QueryTemplate>,
    memory: MemoryDeclarations,
    pepper: Pepper,
    /// Opened full-text sidecars (`read.rank.lexical-index-cache`).
    pub(crate) fulltext: crate::fulltext::SidecarCache<crate::fulltext::FulltextSidecar>,
    /// Resolved sessions and their connections (`read.cache.session-pool`).
    pub(crate) pool: SessionPool,
    /// Statement results reused under their whole key (`read.cache.result-key`); absent
    /// where the process declares no budget (`read.cache.budget`).
    results: Option<ResultCache>,
}

/// The store's vocabulary carried by table descriptions (`read.register.lexicon-surface`).
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Lexicon {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    numeric_identifiers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    badges: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct ManifestLexicon {
    #[serde(default)]
    lexicon: Lexicon,
}

/// The columns a table with no landed batch registers over: the injected columns every
/// write path carries.
fn injected_columns() -> Vec<Column> {
    vec![
        Column::new(INGESTED_AT, ColumnType::Timestamp, false),
        Column::new(RUN_ID, ColumnType::Utf8, false),
        Column::new(ROW_SEQ, ColumnType::Int64, false),
        Column::new(COMMIT_SEQ, ColumnType::Int64, false),
        Column::new(SITE_ID, ColumnType::Utf8, false),
    ]
}

impl Face {
    /// Open the face at startup. Every table policy, every template and every mask
    /// against its table's schema is checked here, once, caller-independently; one
    /// failure refuses the whole manifest (`read.guard.startup-time-check`).
    pub fn open(store: Store, manifest: &str, pepper: Pepper) -> Result<Face, ReadFault> {
        Face::open_declared(store, manifest, &[], pepper)
    }

    /// [`Face::open`] over the declaration set (`read.register.declaration-set`): the
    /// manifest's tables, then those the `pipelines/` files declare. Memory declarations
    /// and templates come from the manifest alone.
    pub fn open_declared(store: Store, manifest: &str, pipelines: &[ManifestFile], pepper: Pepper) -> Result<Face, ReadFault> {
        let lexicon = toml::from_str::<ManifestLexicon>(manifest)
            .map_err(|e| ReadFault::Policy(DeclarationMalformed(e.to_string()).into()))?
            .lexicon;
        let pack = contextful_core::store::declare::DeclarationSet::parse(manifest, pipelines).map_err(|e| match e {
            contextful_core::store::declare::DeclarationSetError::Pipeline(e) => ReadFault::Policy(e.into()),
            contextful_core::store::declare::DeclarationSetError::Memory(DeclareError::Memory(m)) => ReadFault::Refused(m.into()),
            contextful_core::store::declare::DeclarationSetError::Memory(DeclareError::Malformed(m)) => ReadFault::Policy(m.into()),
        })?;
        let (parsed, memory) = pack.into_parts();
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
        let face = Face { store, lexicon, decls, policies, templates, memory, pepper, fulltext, pool: SessionPool::default(), results: None };
        let tables = face.tables()?;
        let engine = SqlEngine::bare()?;
        for t in &face.templates {
            let statements = SqlEngine::statement_count(&t.sql)?;
            if statements > 1 {
                return Err(ReadFault::Refused(DisclosureError::TemplateMultiStatement(format!("template `{}` holds {statements} statements", t.id)).into()));
            }
            t.check(&engine.serialize(&t.sql)?, &tables)?;
        }
        Ok(face)
    }

    /// The face with a result cache holding at most `budget` bytes (`read.cache.budget`).
    pub fn with_result_cache(self, budget: u64) -> Face {
        Face { results: Some(ResultCache::new(budget)), ..self }
    }

    /// The face's result cache, where the process declares a budget.
    pub fn results(&self) -> Option<&ResultCache> {
        self.results.as_ref()
    }

    /// The least time to live the touched tables declare, or `None` where one declares
    /// none or is tagged private (`read.cache.cache-is-opt-in`). A request ledger answers
    /// to its table's declaration.
    fn cache_ttl(&self, touched: &BTreeSet<String>) -> Option<std::time::Duration> {
        let mut least: Option<u64> = None;
        for t in touched {
            let decl = self.decl(contextful_core::store::ledger::ledger_table(t).unwrap_or(t));
            if decl.is_private() {
                return None;
            }
            if decl.retain_rows.is_some() {
                return None;
            }
            let secs = decl.result_cache_secs().ok().flatten()?;
            least = Some(least.map_or(secs, |l| l.min(secs)));
        }
        least.map(std::time::Duration::from_secs)
    }

    /// Execute an admitted statement under `ceiling` and attach its restriction block, or
    /// return the cached response under the same key (`read.cache.hit-identical`). A
    /// `tree` calling a volatile function executes uncached (`read.cache.volatile-bypasses`).
    /// The internals object rides outside the cached projection.
    #[allow(clippy::too_many_arguments)]
    fn answer(
        &self,
        engine: &SqlEngine,
        session: &Session,
        touched: &BTreeSet<String>,
        sql: &str,
        parameters: &Bindings,
        ceiling: u64,
        opts: ReadOptions,
        tree: &Value,
    ) -> Result<Response, ReadFault> {
        let started = std::time::Instant::now();
        let frontier = self.store.frontier_stamp()?;
        self.ensure_session_frontier(session)?;
        let cache = self.results.as_ref().and_then(|c| Some((c, self.cache_ttl(touched)?))).filter(|_| !results::volatile(tree));
        if cache.is_none() {
            if let Some(c) = &self.results {
                c.bypass();
            }
        }
        let key = cache.map(|_| results::key(session, &results::Statement { touched, text: sql, parameters, ceiling, bounds: opts.bounds }));
        let (response, state) = match (cache, key) {
            (Some((c, ttl)), Some(key)) => match c.get(&key) {
                Some(hit) => (hit, Some("hit")),
                None => {
                    let fresh = self.execute(engine, session, touched, sql, parameters, ceiling, opts)?;
                    c.put(key, &fresh, ttl);
                    (fresh, Some("miss"))
                }
            },
            _ => (self.execute(engine, session, touched, sql, parameters, ceiling, opts)?, None),
        };
        let response = if opts.internals {
            let internals = Internals {
                sql: Some(sql.to_string()),
                engine: ENGINE,
                limit: Some(ceiling),
                row_count: response.rows.len() as u64,
                elapsed_ms: started.elapsed().as_millis() as u64,
                cache: state,
            };
            response.with_block("internals", serde_json::to_value(internals).expect("internals serialize"))
        } else {
            response
        };
        self.publish_read(Some(session), &frontier, response)
    }

    /// One admitted statement's projection and restriction block, without internals.
    #[allow(clippy::too_many_arguments)]
    fn execute(
        &self,
        engine: &SqlEngine,
        session: &Session,
        touched: &BTreeSet<String>,
        sql: &str,
        parameters: &Bindings,
        ceiling: u64,
        opts: ReadOptions,
    ) -> Result<Response, ReadFault> {
        let deadline = self.duration_budget(session, touched, opts.max_duration_ms);
        let response = respond_with_deadline(engine, sql, parameters, Some(ceiling), ReadOptions { internals: false, ..opts }, deadline)?;
        self.restrict_timed(engine, session, touched.iter().map(String::as_str), response, deadline)
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
    /// the table's registered relation under its admitted read budgets.
    pub fn rows(&self, session: &Session, table: &str, run: Option<&str>) -> Result<Response, ReadFault> {
        self.table_rows(session, table, run, None)
    }

    /// Stored JSON kinds for a native task, under the same admitted relation and
    /// response budgets as [`Face::rows`]. Text never becomes a number by its spelling.
    pub fn source_rows(&self, session: &Session, table: &str, run: Option<&str>, columns: &[&str]) -> Result<Response, ReadFault> {
        self.table_rows(session, table, run, Some(columns))
    }

    fn table_rows(&self, session: &Session, table: &str, run: Option<&str>, native: Option<&[&str]>) -> Result<Response, ReadFault> {
        let frontier = self.read_frontier(session)?;
        let r = self.registered(session, table)?;
        let engine = self.pool.engine(session, self.store.parquet_key())?;
        let (sql, parameters) = match run {
            Some(run) => {
                (format!("SELECT * FROM {} WHERE {} = ?", ident(r.name()), ident(RUN_ID)), Bindings::positional([Bound::Text(run.to_string())]))
            }
            None => (format!("SELECT * FROM {}", ident(r.name())), Bindings::default()),
        };
        let tree = engine.serialize(&sql)?;
        let admitted = admit_in(session, &tree)?;
        scope::guard(&tree, session, &parameters)?;
        let opts = ReadOptions::default();
        // Declared ceilings only; the face ceiling bounds caller reads (`read.respond.engine-composed-ceiling`).
        let ceiling = Self::declared_ceiling(session, &admitted.relations);
        let deadline = self.duration_budget(session, &admitted.relations, None);
        let response = if let Some(selected) = native {
            let read = AdmittedRows { session, sql, parameters };
            let (columns, rows) = engine.run_native(&read, selected, Response::fetch_count(ceiling), deadline)?;
            Response::cut(columns, rows, ceiling)
        } else {
            respond_with_deadline(&engine, &sql, &parameters, ceiling, opts, deadline)?
        };
        let response = self.restrict_timed(&engine, session, [table], response, deadline)?;
        let response = self.finish_budget(session, &admitted.relations, opts, None, ceiling.unwrap_or(u64::MAX), response)?;
        self.publish_read(Some(session), &frontier, response)
    }

    /// A table's declaration, or an undeclared table's defaults.
    pub fn decl(&self, table: &str) -> TableDecl {
        self.decls.get(table).cloned().unwrap_or_else(|| TableDecl::named(table))
    }

    /// One citation resolves through the caller's relation and the certified erasure
    /// index, with no store path or source payload in its verdict.
    pub fn reference(&self, session: &Session, table: &str, run: &str, seq: i64, opts: ReadOptions) -> Result<Response, ReadFault> {
        let started = std::time::Instant::now();
        let frontier = self.read_frontier(session)?;
        self.registered(session, table)?;
        let touched = BTreeSet::from([table.to_string()]);
        self.bind_valid_time(&touched, opts.bounds)?;
        let reference = contextful_core::memory::synthesize::EvidenceRef { table:table.to_string(), run:run.to_string(), seq, key_digest:None };
        let engine = self.pool.engine(session, self.store.parquet_key())?;
        let verdict = self.evidence_read(&engine, session, &reference, &touched, opts.max_duration_ms)?;
        let readable = verdict == contextful_core::memory::recall::EvidenceRead::Readable;
        let metadata = !session.tenant_scoped() && session.zone_admitted(table) == Some(true)
            && !Self::masked(session, table)
            && session.closed_ledger(&format!("{table}__requests")).is_none();
        let reason = if readable { "available" } else if !metadata { "unreadable" }
            else if crate::erasure_frontier::erased_references(&self.store, table)?.contains(&(run.to_string(), seq)) { "erased" }
            else { "missing" };
        let ceiling = self.ceiling(session, &touched, opts.limit, None);
        let mut response = Response::cut(vec!["available".into(), "reason".into()], vec![vec![json!(readable), json!(reason)]], Some(ceiling));
        if let Some(bounds) = opts.bounds.echo() { response = response.with_block("bounds", bounds); }
        if let Some(resolved) = super::pin::resolved(session, [table]) { response = response.with_block(RESOLVED_BLOCK, resolved); }
        let mut response = self.finish_budget(session, &touched, opts, None, ceiling, response)?;
        if opts.internals {
            let sql = session.relation(table).map(|r| super::evidence::row_reads_sql(r.name())).into_iter().collect::<Vec<_>>();
            let rows = response.rows.len() as u64;
            response = response.with_block("internals", internals_block(&sql, Some(ceiling), rows, started));
        }
        self.publish_read(Some(session), &frontier, response)
    }

    /// One table under the request's bounds, or under its pinned build where `pin` names
    /// one and `as_of` is not the earlier bound (`read.resolve-pin.earlier-bound-wins`). A
    /// table no batch has landed in registers as a zero-row relation over the injected
    /// columns (`read.register.quiet-table`).
    fn source(&self, table: &str, bounds: Bounds, pin: Option<&str>) -> Result<TableSource, ReadFault> {
        let decl = self.decl(table);
        let policy = match self.policies.get(table) {
            Some(p) => p.clone(),
            None => TablePolicy::from_decl(&decl)?,
        };
        let ledger = crate::ledger::files(&self.store, table)?.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        Ok(match self.store.try_schema(table)? {
            Some(_) => {
                let pinned = match pin {
                    Some(build) => super::pin::pinned(&self.store, table, build, bounds.as_of)?,
                    None => None,
                };
                let s = scan_at(&self.store, &decl, bounds, pinned.as_ref())?;
                let files = s.files.iter().map(|f| self.absolute(f)).collect::<Result<Vec<_>, _>>()?;
                let resolved = s.publish.as_ref().map(Resolved::of);
                TableSource { decl, policy, base: s.relation, files, columns: s.columns, landed: true, ledger, resolved }
            }
            None => {
                // A pin on a table holding no build names no committed manifest.
                if let Some(build) = pin {
                    super::pin::pinned(&self.store, table, build, bounds.as_of)?;
                }
                let columns = injected_columns();
                let base = relation(&TableDecl::named(table), &[], &columns, &[], None)?;
                TableSource { decl, policy, base, files: Vec::new(), columns, landed: false, ledger, resolved: None }
            }
        })
    }

    /// A store-root-relative path as the absolute path a relation reads.
    fn absolute(&self, rel: &str) -> Result<String, ReadFault> {
        Ok(self.store.logical_path(rel)?.to_string_lossy().into_owned())
    }

    /// A retained session carries physical files of its admitted table frontier.
    /// Another replacement cannot reuse those files through a connection or cache.
    fn ensure_session_frontier(&self, session: &Session) -> Result<(), ReadFault> {
        let _frontier = self.store.frontier_stamp()?;
        for relation in session.relations() {
            let current = self.store.table_dir(relation.name())?;
            if relation.files().iter().any(|file| !std::path::Path::new(file).starts_with(&current)) {
                return Err(crate::ContextError::from(contextful_core::disclosure::erase::ErasureError::ErasureTransactionIncomplete("the retained session belongs to an obsolete table frontier".into())).into());
            }
        }
        Ok(())
    }

    pub(crate) fn read_frontier(&self, session: &Session) -> Result<Option<String>, ReadFault> {
        let frontier = self.store.frontier_stamp()?;
        self.ensure_session_frontier(session)?;
        Ok(frontier)
    }

    /// The shared project fence covers the final validation and value handoff.
    pub(crate) fn publish_read<T>(&self, session: Option<&Session>, frontier: &Option<String>, value: T) -> Result<T, ReadFault> {
        let _release = self.store.lock_frontier()?;
        if self.store.frontier_stamp()? != *frontier {
            return Err(crate::ContextError::from(contextful_core::disclosure::erase::ErasureError::ErasureTransactionIncomplete("the erasure frontier changed before response publication".into())).into());
        }
        if let Some(session) = session { self.ensure_session_frontier(session)?; }
        Ok(value)
    }

    /// Open a session for an admitted authority: one relation per table its read grants
    /// cover, compiled under the request's zone and bounds. A `valid_as_of` wraps only the
    /// tables declaring a valid-time pair (`store.bound-time.valid-as-of`); a table
    /// declaring none compiles under `as_of` alone and refuses only a read that names it
    /// ([`Face::bind_valid_time`]). A session pooled under the same whole key is reused;
    /// the key is computed before any resolution, so a commit racing the resolution lands
    /// under a key no later call computes (`read.cache.session-pool`).
    pub fn session(&self, authority: &AdmittedAuthority, request: &Request<'_>, bounds: Bounds) -> Result<Session, ReadFault> {
        self.session_pinned(authority, request, bounds, &Pins::default())
    }

    /// [`Face::session`] with each table `pins` names resolved to its pinned build
    /// (`read.resolve-pin.pin-parameter`); an unnamed table resolves to the latest
    /// published state. The pin map joins the pool key (`read.cache.session-pool`). A pin
    /// on a table the session registers no relation for — absent or outside the grants —
    /// refuses as a table publishing no build, so the refusal names no table the
    /// credential cannot read, and the pin never widens to the latest state
    /// (`read.resolve-pin.unknown-build`).
    pub fn session_pinned(&self, authority: &AdmittedAuthority, request: &Request<'_>, bounds: Bounds, pins: &Pins) -> Result<Session, ReadFault> {
        let tables = self.tables()?;
        let granted: Vec<String> = tables.iter().filter(|t| raw_read_covers(authority.grants(), t)).cloned().collect();
        if let Some((table, build)) = pins.iter().find(|(t, _)| !granted.iter().any(|g| g == t)) {
            return Err(super::pin::unavailable(table, build, None));
        }
        let principal = pool::principal(authority, request, bounds, pins);
        let state = pool::store_state(&self.store, &tables, &granted)?;
        let transaction = Bounds { valid_as_of: None, ..bounds };
        self.pool.session(principal, state, || {
            let sources = granted
                .iter()
                .map(|t| self.source(t, if self.decl(t).valid_time.is_some() { bounds } else { transaction }, pins.build(t)))
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

    /// The least row ceiling over the grants, the request, a template, every touched
    /// table's published `limits.max_rows` (`read.respond.row-ceiling`) and the face
    /// ceiling (`read.respond.face-ceiling`). A request ledger answers to its table's ceiling.
    pub(crate) fn ceiling(&self, session: &Session, touched: &BTreeSet<String>, request: Option<u64>, template: Option<u64>) -> u64 {
        let grant = Self::grant_limit(session, touched, |g| g.max_rows);
        let owner = |t: &String| contextful_core::store::ledger::ledger_table(t).map(str::to_string).unwrap_or_else(|| t.clone());
        let table = touched.iter().filter_map(|t| session.policy(&owner(t)).and_then(|p| p.max_rows)).min();
        least_row_ceiling([grant, request, template, table])
    }

    /// The least of the grant's and touched tables' declared row ceilings, if any.
    fn declared_ceiling(session: &Session, touched: &BTreeSet<String>) -> Option<u64> {
        let grant = Self::grant_limit(session, touched, |g| g.max_rows);
        let owner = |t: &String| contextful_core::store::ledger::ledger_table(t).map(str::to_string).unwrap_or_else(|| t.clone());
        let table = touched.iter().filter_map(|t| session.policy(&owner(t)).and_then(|p| p.max_rows)).min();
        grant.into_iter().chain(table).min()
    }

    fn grant_limit(session: &Session, touched: &BTreeSet<String>, field: fn(&Grant) -> Option<u64>) -> Option<u64> {
        session.grants().iter().filter(|grant| {
            touched.iter().any(|table| {
                let owner = contextful_core::store::ledger::ledger_table(table).unwrap_or(table);
                raw_read_covers(std::slice::from_ref(*grant), owner)
            })
        }).filter_map(field).min()
    }

    fn budget(&self, session: &Session, touched: &BTreeSet<String>, field: fn(&Grant) -> Option<u64>, table_field: fn(&TablePolicy) -> Option<u64>, request: Option<u64>) -> Option<(u64, &'static str)> {
        let owner = |t: &String| contextful_core::store::ledger::ledger_table(t).map(str::to_string).unwrap_or_else(|| t.clone());
        let grant = Self::grant_limit(session, touched, field);
        let table = touched.iter().filter_map(|t| session.policy(&owner(t)).and_then(table_field)).min();
        [(grant, "grant"), (table, "table"), (request, "request")]
            .into_iter().filter_map(|(n, source)| n.map(|n| (n, source))).min_by_key(|(n, _)| *n)
    }

    pub(crate) fn duration_budget(&self, session: &Session, touched: &BTreeSet<String>, request: Option<u64>) -> Option<(u64, &'static str)> {
        self.budget(session, touched, |g| g.max_duration_ms, |p| p.max_duration_ms, request)
    }

    fn byte_budget(&self, session: &Session, touched: &BTreeSet<String>, request: Option<u64>) -> Option<(u64, &'static str)> {
        self.budget(session, touched, |g| g.max_response_bytes, |p| p.max_response_bytes, request)
    }

    fn row_source(&self, session: &Session, touched: &BTreeSet<String>, request: Option<u64>, template: Option<u64>, ceiling: u64) -> &'static str {
        let owner = |t: &String| contextful_core::store::ledger::ledger_table(t).map(str::to_string).unwrap_or_else(|| t.clone());
        let grant = Self::grant_limit(session, touched, |g| g.max_rows);
        let table = touched.iter().filter_map(|t| session.policy(&owner(t)).and_then(|p| p.max_rows)).min();
        [(grant, "grant"), (table, "table"), (request, "request"), (template, "template")]
            .into_iter().find_map(|(n, source)| (n == Some(ceiling)).then_some(source)).unwrap_or("face")
    }

    pub(crate) fn finish_budget(&self, session: &Session, touched: &BTreeSet<String>, request: ReadOptions, template: Option<u64>, ceiling: u64, mut response: Response) -> Result<Response, ReadFault> {
        if response.truncated {
            response.blocks.insert("contextful.truncation".into(), json!({ "by": "rows", "ceiling": ceiling, "source": self.row_source(session, touched, request.limit, template, ceiling) }));
        }
        if let Some((bytes, source)) = self.byte_budget(session, touched, request.max_response_bytes) {
            if let Some(probe) = &response.probe_row {
                let mut next = response.clone();
                next.rows.push(probe.clone());
                next.blocks.insert("contextful.truncation".into(), json!({ "by": "bytes", "ceiling": bytes, "source": source }));
                if serde_json::to_vec(&next).expect("the response serializes").len() as u64 > bytes {
                    response.blocks.insert("contextful.truncation".into(), json!({ "by": "bytes", "ceiling": bytes, "source": source }));
                }
            }
            if serde_json::to_vec(&response).expect("the response serializes").len() as u64 > bytes {
                if response.rows.pop().is_none() {
                    return Err(ReadError::ReadResponseTooLarge(format!("{bytes} bytes from {source}; the response envelope or first row exceeds the ceiling")).into());
                }
                response.truncated = true;
                response.blocks.insert("contextful.truncation".into(), json!({ "by": "bytes", "ceiling": bytes, "source": source }));
                let mut size = serde_json::to_vec(&response).expect("the response serializes").len();
                while size as u64 > bytes {
                    let Some(row) = response.rows.pop() else {
                        return Err(ReadError::ReadResponseTooLarge(format!("{bytes} bytes from {source}; the response envelope or first row exceeds the ceiling")).into());
                    };
                    // Compact JSON loses the row's encoded bytes and one separator
                    // whenever another row remains. The envelope stays unchanged.
                    size -= serde_json::to_vec(&row).expect("the row serializes").len() + usize::from(!response.rows.is_empty());
                }
                if response.rows.is_empty() {
                    return Err(ReadError::ReadResponseTooLarge(format!("{bytes} bytes from {source}; the first row exceeds the ceiling")).into());
                }
            }
        }
        if let Some(Value::Object(retrieval)) = response.blocks.get_mut("contextful.retrieval") {
            retrieval.insert("returned".into(), json!(response.rows.len()));
            if let Some(index) = response.columns.iter().position(|column| column == "_in_window") {
                let in_window = response.rows.iter().filter(|row| row.get(index).and_then(Value::as_bool) == Some(true)).count();
                retrieval.insert("in_window".into(), json!(in_window));
            }
        }
        if let Some(Value::Object(internals)) = response.blocks.get_mut("contextful.internals") {
            internals.insert("row_count".into(), json!(response.rows.len()));
        }
        Ok(response)
    }

    /// Run operator text raw over every table the store holds or the manifest declares,
    /// each registered under its bare name as its unrestricted base relation at the
    /// latest committed state (`read.query.project-relations`).
    /// A store absent from disk raises `QueryProjectAbsent` (`read.query.project-store`).
    pub fn operator_query(&self, sql: &str, opts: ReadOptions) -> Result<Response, ReadFault> {
        let frontier = self.store.frontier_stamp()?;
        if !self.store.root().is_dir() {
            return Err(ReadError::QueryProjectAbsent(format!(
                "no store exists at `{}`; a declared table would read as quiet rather than as an answer",
                self.store.root().display()
            ))
            .into());
        }
        one_statement(sql)?;
        let engine = SqlEngine::raw_with_key(self.store.parquet_key())?;
        for t in self.tables()? {
            engine.register(&t, &self.source(&t, Bounds::default(), None)?.base)?;
        }
        // The least published row ceiling among the relations the statement names, beside
        // `--limit` (`read.query.raw-row-ceiling`).
        let named = engine.serialize(sql).map(|tree| named_relations(&tree)).unwrap_or_default();
        let published = named.iter().filter_map(|t| self.policies.get(t).and_then(|p| p.max_rows)).min();
        let ceiling = opts.limit.into_iter().chain(published).min();
        let response = respond(&engine, sql, &Bindings::default(), ceiling, opts)?;
        self.publish_read(None, &frontier, response)
    }

    /// Register every table on `engine` under its bare name as its unrestricted base
    /// relation at the latest committed state, as [`Face::operator_query`] does, and return
    /// the registered names.
    pub(crate) fn register_operator(&self, engine: &SqlEngine) -> Result<Vec<String>, ReadFault> {
        let tables = self.tables()?;
        for t in &tables {
            engine.register(t, &self.source(t, Bounds::default(), None)?.base)?;
        }
        Ok(tables)
    }

    /// Admit and run caller-written SQL carrying no parameter.
    pub fn query(&self, session: &Session, sql: &str, opts: ReadOptions) -> Result<Response, ReadFault> {
        self.query_with(session, sql, &Map::new(), opts)
    }

    /// Admit and run caller-written SQL: exactly one read-only `SELECT` over this
    /// session's registered relations, its placeholders bound from typed `parameters`
    /// (`read.guard.query-binding`), the scope guard over its tenant literals and bound
    /// values, then execution under the least row ceiling.
    pub fn query_with(&self, session: &Session, sql: &str, parameters: &Map<String, Value>, opts: ReadOptions) -> Result<Response, ReadFault> {
        let frontier = self.read_frontier(session)?;
        let engine = self.pool.engine(session, self.store.parquet_key())?;
        let tree = engine.serialize(sql)?;
        let admitted = admit_in(session, &tree)?;
        if admitted.placeholders.iter().any(|p| p.parse::<u64>().is_ok()) && positional_marker(sql) {
            return Err(ReadError::QueryParameterRejected("positional `?` is not accepted by `context.query`; use `$name` or `$1`".into()).into());
        }
        let bindings = bind_query(parameters, &admitted.placeholders)?;
        scope::guard(&tree, session, &bindings)?;
        engine.register_ledgers(&self.store, session, &admitted.relations)?;
        self.bind_valid_time(&admitted.relations, opts.bounds)?;
        let ceiling = self.ceiling(session, &admitted.relations, opts.limit, None);
        let mut response = self.answer(&engine, session, &admitted.relations, sql, &bindings, ceiling, opts, &tree)?;
        if opts.internals {
            if let Some(Value::Object(internals)) = response.blocks.get_mut("contextful.internals") {
                internals.insert("parameters".into(), Value::Object(parameters.clone()));
            }
        }
        let response = self.finish_budget(session, &admitted.relations, opts, None, ceiling, response)?;
        self.publish_read(Some(session), &frontier, response)
    }

    /// Run a declared template the credential's allowlist covers. Its body is operator
    /// text and runs unrewritten; each bare table name in it resolves to the caller's
    /// registered relation (`read.register.bare-name`).
    pub fn execute_template(&self, session: &Session, id: &str, arguments: &Map<String, Value>, opts: ReadOptions) -> Result<Response, ReadFault> {
        let frontier = self.read_frontier(session)?;
        let declared: Vec<String> = self.templates.iter().map(|t| t.id.clone()).collect();
        authorize_template(session.grants(), id, &declared)?;
        let template = self.templates.iter().find(|t| t.id == id).expect("an authorized template is declared");
        let values = template.bind(arguments)?;
        let engine = self.pool.engine(session, self.store.parquet_key())?;
        let tree = engine.serialize(&template.sql)?;
        let admitted = admit_in(session, &tree)?;
        let parameters = template.bindings(values, &admitted.placeholders);
        scope::guard(&tree, session, &parameters)?;
        engine.register_ledgers(&self.store, session, &admitted.relations)?;
        self.bind_valid_time(&admitted.relations, opts.bounds)?;
        let ceiling = self.ceiling(session, &admitted.relations, opts.limit, template.max_rows);
        let response = self.answer(&engine, session, &admitted.relations, &template.sql, &parameters, ceiling, opts, &tree)?;
        let response = self.finish_budget(session, &admitted.relations, opts, template.max_rows, ceiling, response)?;
        self.publish_read(Some(session), &frontier, response)
    }

    /// The tools this session sees: the closed built-in set and each declared template
    /// its grants cover, as a tool named by its identifier.
    pub fn tools(&self, session: &Session) -> Vec<Value> {
        let declared: Vec<String> = self.templates.iter().map(|t| t.id.clone()).collect();
        let allowed = list_templates(session.grants(), &declared);
        let mut tools: Vec<Value> = TOOLS.iter().map(|t| builtin_tool(t)).collect();
        tools.extend(self.templates.iter().filter(|t| allowed.contains(&t.id.as_str())).map(|t| {
            let mut tool = t.tool();
            let properties = tool["inputSchema"]["properties"].as_object_mut().expect("a template schema lists properties");
            for (name, schema) in bound_properties() {
                properties.insert(name, schema);
            }
            properties.insert("max_duration_ms".into(), json!({ "type": "integer", "minimum": 0 }));
            properties.insert("max_response_bytes".into(), json!({ "type": "integer", "minimum": 0 }));
            properties.insert(PIN_ARGUMENT.into(), pin_property());
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
        self.describe_value(session, table, bounds, None)
    }

    fn describe_value(&self, session: &Session, table: Option<&str>, bounds: Bounds, deadline: Option<(u64, &'static str)>) -> Result<Value, ReadFault> {
        let frontier = self.read_frontier(session)?;
        let echo = |mut v: Value, bounds: Bounds| {
            if let Some(b) = bounds.echo() {
                v["contextful.bounds"] = b;
            }
            v
        };
        let Some(table) = table else {
            let tables: Vec<Value> = session
                .relations()
                .map(|r| {
                    json!({
                        "table": r.name(),
                        "kind": if self.memory.table(r.name()).is_some() { "memory" } else { "data" },
                        "description": self.decl(r.name()).agent_description,
                        "zone_admitted": session.zone_admitted(r.name()),
                    })
                })
                .collect();
            return self.publish_read(Some(session), &frontier, echo(json!({ "session_zone": session.zone().label(), "tables": tables }), Bounds { valid_as_of: None, ..bounds }));
        };
        let r = self.registered(session, table)?;
        self.bind_valid_time(&BTreeSet::from([table.to_string()]), bounds)?;
        let engine = self.pool.engine(session, self.store.parquet_key())?;
        // The row count beside the serialized JSON length of the caller's restricted rows
        // (`read.register.size-estimate`).
        let sql = describe_sql(r.name());
        let (_, count) = match deadline {
            Some((ms, source)) => engine.run_timed(&sql, &Bindings::default(), None, ms, source)?,
            None => engine.run(&sql, &Bindings::default(), None)?,
        };
        let row_count = count.first().and_then(|r| r.first()).map(Cell::to_json).unwrap_or(Value::Null);
        let bytes = count.first().and_then(|r| r.get(1)).map(Cell::to_json).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|n| n.parse::<u64>().ok()))).unwrap_or(0);
        let decl = self.decl(table);
        let policy = session.policy(table).expect("a registered table carries its policy");
        let schema = session.columns(table).expect("a registered table carries its columns");
        let columns: Vec<Value> = schema.iter().map(|c| {
            let mut column = json!({ "name": c.name, "type": c.ty.name() });
            if let Some(hint) = decl.column_hints.as_ref().and_then(|hints| hints.get(&c.name)) {
                column["hint"] = json!(hint);
            }
            column
        }).collect();
        let fingerprint: String = {
            let text: Vec<String> = schema.iter().map(|c| format!("{}:{}", c.name, c.ty.name())).collect();
            Sha256::digest(text.join("\n").as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
        };
        let mut out = json!({
            "table": table,
            "row_count": row_count,
            "bytes": bytes,
            "estimated_tokens": estimated_tokens(bytes),
            "schema_fingerprint": fingerprint,
            "description": decl.agent_description,
            "hint": decl.agent_hint,
            "columns": columns,
            "indexes": decl.indexes.clone().unwrap_or_default(),
            "partition_by": decl.partition_by(),
            "zone": policy.placement.effective().labels(),
            "session_zone": session.zone().label(),
            "zone_admitted": session.zone_admitted(table),
            "lexicon": self.lexicon,
            "example_queries": decl.example_queries.clone().unwrap_or_default(),
        });
        let mut limits = Map::new();
        if let Some(max) = policy.max_rows {
            limits.insert("max_rows".into(), json!(max));
        }
        if let Some(max) = policy.max_duration_ms {
            limits.insert("max_duration_ms".into(), json!(max));
        }
        if let Some(max) = policy.max_response_bytes {
            limits.insert("max_response_bytes".into(), json!(max));
        }
        if !limits.is_empty() {
            out["limits"] = Value::Object(limits);
        }
        if let Some(resolved) = super::pin::resolved(session, [table]) {
            out[format!("contextful.{RESOLVED_BLOCK}")] = resolved;
        }
        self.publish_read(Some(session), &frontier, echo(out, bounds))
    }

    /// The table description or listing under its selected serialized byte ceiling.
    pub fn describe_with_options(&self, session: &Session, table: Option<&str>, opts: ReadOptions) -> Result<Value, ReadFault> {
        let started = std::time::Instant::now();
        let frontier = self.read_frontier(session)?;
        let touched = match table {
            Some(name) => BTreeSet::from([name.to_string()]),
            None => session.relations().map(|r| r.name().to_string()).collect(),
        };
        let deadline = self.duration_budget(session, &touched, opts.max_duration_ms);
        let mut value = self.describe_value(session, table, opts.bounds, deadline)?;
        if let Some((bytes, source)) = self.byte_budget(session, &touched, opts.max_response_bytes) {
            if serde_json::to_vec(&value).expect("the description serializes").len() as u64 > bytes {
                return Err(ReadError::ReadResponseTooLarge(format!("{bytes} bytes from {source}; the description exceeds the ceiling")).into());
            }
        }
        if opts.internals {
            // A listing runs no statement; a description runs its count.
            let (sql, rows) = match table {
                Some(name) => (vec![describe_sql(name)], 1),
                None => (Vec::new(), value["tables"].as_array().map_or(0, Vec::len) as u64),
            };
            value["contextful.internals"] = internals_block(&sql, None, rows, started);
        }
        self.publish_read(Some(session), &frontier, value)
    }

    /// Committed data files of the tables the session reads, store-root-relative: the files
    /// each registered relation reads, a pinned table's build included; a table outside the
    /// session contributes no path (`read.register.file-listing`). Only `as_of` selects
    /// files and only it echoes (`read.register.bound-listing`).
    pub fn files(&self, session: &Session, bounds: Bounds) -> Result<Response, ReadFault> {
        self.files_with_options(session, ReadOptions { bounds, ..ReadOptions::default() })
    }

    /// The committed file listing under the request's serialized response budget.
    pub fn files_with_options(&self, session: &Session, opts: ReadOptions) -> Result<Response, ReadFault> {
        let started = std::time::Instant::now();
        let frontier = self.read_frontier(session)?;
        let transaction = Bounds { valid_as_of: None, ..opts.bounds };
        let touched: BTreeSet<String> = session.relations().map(|r| r.name().to_string()).collect();
        let ceiling = contextful_core::read::respond::FACE_ROW_CEILING;
        let mut rows = Vec::new();
        for r in session.relations() {
            let directory = self.store.table_dir(r.name())?;
            for f in r.files() {
                let tail = std::path::Path::new(f).strip_prefix(&directory).map_err(|_| crate::ContextError::from(contextful_core::disclosure::erase::ErasureError::ErasureTransactionIncomplete("a listed file belongs to an obsolete table frontier".into())))?;
                let logical = format!("tables/{}/{}", r.name(), tail.to_string_lossy().replace('\\', "/"));
                rows.push(vec![json!(r.name()), json!(logical)]);
                if rows.len() as u64 > ceiling {
                    break;
                }
            }
            if rows.len() as u64 > ceiling {
                break;
            }
        }
        let mut response = Response::cut(vec!["table".into(), "path".into()], rows, Some(ceiling));
        if let Some(b) = transaction.echo() {
            response = response.with_block("bounds", b);
        }
        let response = match super::pin::resolved(session, touched.iter().map(String::as_str)) {
            Some(resolved) => response.with_block(RESOLVED_BLOCK, resolved),
            None => response,
        };
        let response = self.finish_budget(session, &touched, opts, None, ceiling, response)?;
        // A listing reads manifests and runs no statement.
        let response = if opts.internals {
            let rows = response.rows.len() as u64;
            response.with_block("internals", internals_block(&[], Some(ceiling), rows, started))
        } else {
            response
        };
        self.publish_read(Some(session), &frontier, response)
    }

    /// Preview one committed run file through its table's registered relation. A snapshot
    /// part, a traversal, an absolute path or a ledger file resolves to no table
    /// (`read.register.file-preview-target`). Under `as_of` the file is one the bound
    /// reaches; under `valid_as_of` its rows are those valid at the instant.
    pub fn file(&self, session: &Session, path: &str, opts: ReadOptions) -> Result<Response, ReadFault> {
        let frontier = self.read_frontier(session)?;
        let table = preview_target(path)?;
        self.registered(session, &table)?;
        let decl = self.decl(&table);
        let transaction = Bounds { valid_as_of: None, ..opts.bounds };
        if !scan(&self.store, &decl, transaction)?.files.iter().any(|f| f == path) {
            return Err(ReadError::FilePreviewNotATable(format!("`{path}` is no committed data file of `{table}`")).into());
        }
        let file = self.absolute(path)?;
        let schema = self.store.schema(&table)?;
        let carried = self.store.parquet_columns(std::path::Path::new(&file))?;
        let absent: Vec<Column> = schema.columns.iter().filter(|c| !carried.contains(&c.name)).cloned().collect();
        let key_name = self.store.parquet_key().map(|_| crate::encrypt::PARQUET_KEY_NAME);
        let base = relation_with_encryption(&decl, std::slice::from_ref(&file), &schema.columns, &absent, opts.bounds.valid_as_of, key_name)?;
        let preview = session.relation_over(&table, &base, vec![file]).expect("a registered table carries its source");
        let engine = SqlEngine::open(session, self.store.parquet_key())?;
        engine.register(PREVIEW_RELATION, preview.sql())?;
        let touched = BTreeSet::from([table]);
        let ceiling = self.ceiling(session, &touched, opts.limit, None);
        let deadline = self.duration_budget(session, &touched, opts.max_duration_ms);
        let response = respond_with_deadline(&engine, &format!("SELECT * FROM {}", ident(PREVIEW_RELATION)), &Bindings::default(), Some(ceiling), opts, deadline)?;
        let response = self.restrict_timed(&engine, session, touched.iter().map(String::as_str), response, deadline)?;
        let response = self.finish_budget(session, &touched, opts, None, ceiling, response)?;
        self.publish_read(Some(session), &frontier, response)
    }

    /// Attach the restriction block naming each touched relation the session's zone
    /// excludes or column-masks, or leave the response as it is where none is
    /// (`read.respond.restriction-block`). Each count reads the whole relation under its
    /// earlier steps, never the caller's statement (`authority.place.excluded-disclosed`).
    /// Every touched published model rides `contextful.resolved`, whatever the row count
    /// (`read.resolve-pin.resolved-echo`).
    pub(crate) fn restrict_timed<'t>(
        &self,
        engine: &SqlEngine,
        session: &Session,
        touched: impl IntoIterator<Item = &'t str>,
        response: Response,
        deadline: Option<(u64, &'static str)>,
    ) -> Result<Response, ReadFault> {
        let touched: BTreeSet<&str> = touched.into_iter().collect();
        let response = match super::pin::resolved(session, touched.iter().copied()) {
            Some(resolved) => response.with_block(RESOLVED_BLOCK, resolved),
            None => response,
        };
        let mut tables = Vec::new();
        for withheld in touched.into_iter().filter_map(|t| session.zone_withheld(t)) {
            let rows_dropped = match &withheld.dropped_sql {
                Some(sql) => {
                    let (_, count) = match deadline {
                        Some((ms, source)) => engine.run_timed(sql, &Bindings::default(), None, ms, source)?,
                        None => engine.run(sql, &Bindings::default(), None)?,
                    };
                    match count.first().and_then(|r| r.first()) {
                        Some(Cell::Integer { value, .. }) => u64::try_from(*value).unwrap_or(0),
                        _ => 0,
                    }
                }
                None => 0,
            };
            tables.push(json!({
                "table": withheld.relation,
                "excluded": withheld.excluded,
                "rows_dropped": rows_dropped,
                "columns_masked": withheld.columns_masked,
            }));
        }
        if tables.is_empty() {
            return Ok(response);
        }
        Ok(response.with_block(
            "restriction",
            json!({ "zone": session.zone().label(), "incognito": session.incognito(), "tables": tables }),
        ))
    }
}

/// Admit a statement over the session's registered relations. A session that is no owner
/// read naming the request ledger of a table it reads raises `LedgerNotTenantScoped`
/// rather than an unknown relation (`read.register.scoped-ledger`).
pub(crate) fn admit_in(session: &Session, tree: &Value) -> Result<Admitted, ReadFault> {
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

/// DuckDB's parse tree gives `?` and `$1` the same identifier. The source spelling
/// distinguishes the caller query's positional form from its supported numbered form.
fn positional_marker(sql: &str) -> bool {
    let bytes = sql.as_bytes();
    let (mut i, mut mode, mut block_depth, mut dollar_delimiter, mut escape_quote) = (0, 0u8, 0usize, 0..0, false);
    while i < bytes.len() {
        let next = bytes.get(i + 1).copied();
        match mode {
            0 => match (bytes[i], next) {
                (b'?', _) => return true,
                (b'\'', _) => {
                    escape_quote = i > 0 && matches!(bytes[i - 1], b'e' | b'E');
                    mode = 1;
                }
                (b'"', _) => mode = 2,
                (b'-', Some(b'-')) => { mode = 3; i += 1; }
                (b'/', Some(b'*')) => { mode = 4; block_depth = 1; i += 1; }
                (b'$', _) => {
                    let mut end = i + 1;
                    if bytes.get(end).is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_') {
                        while bytes.get(end).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') {
                            end += 1;
                        }
                    }
                    if bytes.get(end) == Some(&b'$') {
                        dollar_delimiter = i..end + 1;
                        mode = 5;
                        i = end;
                    }
                }
                _ => {}
            },
            1 | 2 => {
                let quote = if mode == 1 { b'\'' } else { b'"' };
                if mode == 1 && escape_quote && bytes[i] == b'\\' {
                    i += 1;
                } else if bytes[i] == quote {
                    if next == Some(quote) { i += 1; } else { mode = 0; }
                }
            }
            3 => if matches!(bytes[i], b'\n' | b'\r') { mode = 0; },
            4 => match (bytes[i], next) {
                (b'/', Some(b'*')) => { block_depth += 1; i += 1; }
                (b'*', Some(b'/')) => { block_depth -= 1; i += 1; if block_depth == 0 { mode = 0; } }
                _ => {}
            },
            _ => if bytes[i..].starts_with(&bytes[dollar_delimiter.clone()]) {
                i += dollar_delimiter.len() - 1;
                mode = 0;
            },
        }
        i += 1;
    }
    false
}

/// Run operator text raw over no store: no relation registers, and local files and table
/// functions stay reachable (`read.guard.statement-provenance`).
pub fn operator_query(sql: &str, opts: ReadOptions) -> Result<Response, ReadFault> {
    one_statement(sql)?;
    respond(&SqlEngine::raw()?, sql, &Bindings::default(), opts.limit, opts)
}

/// The statement a table description runs: the row count beside the serialized JSON
/// length of the caller's restricted rows (`read.register.size-estimate`).
fn describe_sql(relation: &str) -> String {
    format!("SELECT count(*), coalesce(sum(strlen(CAST(to_json(t) AS VARCHAR))), 0)::BIGINT FROM {} AS t", ident(relation))
}

/// The internals object of a read that executed `sql`, one statement per entry and absent
/// for a read running none, under `limit`, delivering `row_count` rows
/// (`read.respond.internals-opt-in`).
pub(crate) fn internals_block(sql: &[String], limit: Option<u64>, row_count: u64, started: std::time::Instant) -> Value {
    let internals = Internals {
        sql: (!sql.is_empty()).then(|| sql.join("\n")),
        engine: ENGINE,
        limit,
        row_count,
        elapsed_ms: started.elapsed().as_millis() as u64,
        cache: None,
    };
    serde_json::to_value(internals).expect("internals serialize")
}

/// Refuse operator text holding other than exactly one statement before any statement
/// runs (`read.query.one-statement`).
pub(crate) fn one_statement(sql: &str) -> Result<(), ReadFault> {
    match SqlEngine::statement_count(sql)? {
        1 => Ok(()),
        n => Err(ReadError::QueryNotOneStatement(format!("the text holds {n} statements; the verb runs exactly one")).into()),
    }
}

/// Execute `sql` under `ceiling` and serialize the one response projection
/// (`read.respond.one-projection`), with the internals block under `opts.internals`.
pub(crate) fn respond(engine: &SqlEngine, sql: &str, parameters: &Bindings, ceiling: Option<u64>, opts: ReadOptions) -> Result<Response, ReadFault> {
    respond_with_deadline(engine, sql, parameters, ceiling, opts, opts.max_duration_ms.map(|n| (n, "request")))
}

fn respond_with_deadline(engine: &SqlEngine, sql: &str, parameters: &Bindings, ceiling: Option<u64>, opts: ReadOptions, deadline: Option<(u64, &'static str)>) -> Result<Response, ReadFault> {
    let started = std::time::Instant::now();
    let fetch = Response::fetch_count(ceiling);
    let (columns, rows) = match deadline {
        Some((ms, source)) => engine.run_timed(sql, parameters, fetch, ms, source)?,
        None => engine.run(sql, parameters, fetch)?,
    };
    let rows: Vec<Vec<Value>> = rows.iter().map(|r| r.iter().map(Cell::to_json).collect()).collect();
    let mut response = Response::cut(columns, rows, ceiling);
    if let Some(b) = opts.bounds.echo() {
        response = response.with_block("bounds", b);
    }
    Ok(if opts.internals {
        let internals = Internals {
            sql: Some(sql.to_string()),
            engine: ENGINE,
            limit: ceiling,
            row_count: response.rows.len() as u64,
            elapsed_ms: started.elapsed().as_millis() as u64,
            cache: None,
        };
        response.with_block("internals", serde_json::to_value(internals).expect("internals serialize"))
    } else {
        response
    })
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
    let instant = |about: &str| json!({ "type": "string", "description": format!("{about} {INSTANT_LITERAL}") });
    let (description, mut properties, required): (&str, Value, Vec<&str>) = match name {
        "context.describe" => (
            "Describe one table this credential reads, or list them.",
            json!({ "table": { "type": "string" }, "internals": { "type": "boolean" }, "zone": { "type": "string" } }),
            vec![],
        ),
        "context.query" => (
            "Run one read-only SELECT over the tables this credential reads; each `$name` placeholder binds a typed parameter.",
            json!({
                "sql": { "type": "string" },
                "parameters": {
                    "type": "object",
                    "additionalProperties": {
                        "type": "object",
                        "properties": {
                            "type": { "type": "string", "enum": ParamType::NAMES },
                            "value": {}
                        },
                        "required": ["type", "value"],
                        "additionalProperties": false
                    }
                },
                "limit": { "type": "integer" }, "internals": { "type": "boolean" }, "zone": { "type": "string" } }),
            vec!["sql"],
        ),
        "context.reference" => (
            "Resolve one stored citation through this credential's registered relation.",
            json!({ "table": { "type": "string" }, "run": { "type": "string" }, "seq": { "type": "integer", "minimum": 0 }, "internals": { "type": "boolean" }, "zone": { "type": "string" } }),
            vec!["table", "run", "seq"],
        ),
        "context.execute_query" => (
            "Run a declared query template by identifier.",
            json!({ "id": { "type": "string" }, "arguments": { "type": "object" }, "limit": { "type": "integer" }, "internals": { "type": "boolean" }, "zone": { "type": "string" } }),
            vec!["id"],
        ),
        "context.files" => (
            "List committed data files of the tables this credential reads.",
            json!({ "as_of": { "type": "string" }, "internals": { "type": "boolean" }, "zone": { "type": "string" } }),
            vec![],
        ),
        "context.file" => (
            "Preview one committed run file through its table's relation.",
            json!({ "path": { "type": "string" }, "limit": { "type": "integer" }, "internals": { "type": "boolean" }, "zone": { "type": "string" } }),
            vec!["path"],
        ),
        "memory.recall" => (
            "The claims of one subject, matched exactly, valid at `observed_at` as known at `as_of_ingest`.",
            json!({
                "table": { "type": "string" }, "subject": { "type": "string" },
                "observed_at": instant("The valid-time instant the claims cover; absent, the call's own."),
                "as_of_ingest": instant("The transaction-time bound; absent, the latest committed state."),
                "limit": { "type": "integer" }, "internals": { "type": "boolean" }, "zone": { "type": "string" }
            }),
            vec!["table", "subject"],
        ),
        _ => (
            "Ranked rows across the tables under a prefix.",
            json!({
                "prefix": { "type": "string" }, "query": { "type": "string" },
                "query_embedding": { "type": "array", "items": { "type": "number" } },
                "filter": {
                    "type": "object",
                    "description": "Each column to a string, number or boolean it equals, or a list of them it is a member of.",
                    "additionalProperties": {
                        "anyOf": [
                            { "type": ["string", "number", "boolean"] },
                            { "type": "array", "items": { "type": ["string", "number", "boolean"] }, "minItems": 1 }
                        ]
                    }
                },
                "kinds": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                "limit": { "type": "integer" }, "as_of": { "type": "string" }, "since": { "type": "string" },
                "min_score": { "type": "integer" }, "internals": { "type": "boolean" }, "zone": { "type": "string" }
            }),
            vec!["query"],
        ),
    };
    // `memory.recall` names its two clocks itself (`read.register.bound-arguments`).
    if name != "memory.recall" {
        for (bound, schema) in bound_properties() {
            properties[bound.as_str()] = schema;
        }
    }
    properties["max_duration_ms"] = json!({ "type": "integer", "minimum": 0 });
    properties["max_response_bytes"] = json!({ "type": "integer", "minimum": 0 });
    properties[PIN_ARGUMENT] = pin_property();
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
    })
}

/// How a bound argument's literal reads (`store.bound-time.instant-comparison`).
const INSTANT_LITERAL: &str = "An RFC 3339 instant, or a YYYY-MM-DD date read as the start of the next day, exclusive.";

/// The two bound arguments every read tool but `memory.recall` declares
/// (`read.register.bound-arguments`).
fn bound_properties() -> [(String, Value); 2] {
    let instant = || json!({ "type": "string", "description": INSTANT_LITERAL });
    [("as_of".into(), instant()), ("valid_as_of".into(), instant())]
}

/// The `pin` argument every read tool declares (`read.resolve-pin.pin-parameter`).
fn pin_property() -> Value {
    json!({
        "type": "object",
        "description": "Each table to the build identifier it resolves to, or null for its latest published state.",
        "additionalProperties": { "type": ["string", "null"] }
    })
}
