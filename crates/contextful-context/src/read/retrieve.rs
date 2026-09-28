//! `corpus.retrieve`: one arm per registered relation under a prefix, the recency
//! window, snippets and content tokens, the relevance floor, the ranking legs and the
//! retrieval block.

use super::engine::{cell, SqlEngine};
use super::face::Face;
use super::fault::ReadFault;
use contextful_core::memory::declare::Shape;
use contextful_core::memory::recall::{gate, EvidenceRead};
use contextful_core::memory::synthesize::EvidenceRef;
use contextful_core::read::embed::cosine;
use contextful_core::read::template::Bound;
use crate::fulltext::{self, FulltextSidecar, SidecarCache};
use crate::vector::{self, Fallback, VectorSidecar};
use contextful_core::read::rank::{
    candidate_window, sidecar_probe_size, fuse, min_max, order, Candidate, LexicalIndex, Publication, RetrievalBlock, RowRanking,
    Timeframe,
};
use contextful_core::read::respond::{Cell, Response};
use contextful_core::read::tokens::{content_tokens, lexical_score, passes_floor, relevance_floor};
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::index::IndexKind;
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::relation::ident;
use contextful_core::store::reserve::{AUTHORED_BY, INGESTED_AT, ROW_SEQ, RUN_ID};
use contextful_core::time::Instant;
use contextful_policy::enforce::session::Session;
use duckdb::types::Value as Engine;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// Default number of rows a ranked read returns.
pub const DEFAULT_LIMIT: u64 = 10;

/// Most rows one ranked read returns, whatever its limit and ceilings; it bounds the
/// candidate window every arm reads.
pub const MAX_LIMIT: u64 = 1000;

/// Label-priority columns, which lead a snippet (`read.retrieve.snippet`).
const LABEL_COLUMNS: [&str; 8] = ["title", "headline", "name", "subject", "summary", "abstract", "description", "thesis"];

/// Publication columns the engine resolves, in preference order, before falling back to
/// the ingestion instant (`read.retrieve.engine-resolved-date`).
const PUBLICATION_COLUMNS: [&str; 6] = ["published_at", "publication_date", "published", "pub_date", "date", "created_at"];

/// Reserved columns every ranked row projects, null where its table lacks them
/// (`read.retrieve.reserved-columns-project-null`).
const RESERVED_PROJECTED: [(&str, &str); 4] =
    [("_modality", "_modality"), ("_lang", "_lang"), ("_prompt_hash", "_prompt_hash"), ("_kind", "kind")];

/// The column carrying a row's stored vector.
const EMBEDDING_COLUMN: &str = "embedding";

/// Identifiers one re-join statement binds.
const REJOIN_CHUNK: usize = 256;

/// What a ranked read asks for.
#[derive(Debug, Clone)]
pub struct RetrieveRequest {
    pub prefix: String,
    pub query: String,
    pub query_embedding: Option<Vec<f32>>,
    pub limit: Option<u64>,
    /// The question's lower bound on publication.
    pub since: Option<Instant>,
    /// The instant the question is asked at; the timeframe's anchor.
    pub anchor: Instant,
    /// A caller minimum overriding the relevance floor.
    pub min_score: Option<u32>,
    pub internals: bool,
}

impl RetrieveRequest {
    /// A ranked read of `query` across `prefix`, asked at `anchor`, with every option unset.
    pub fn new(prefix: impl Into<String>, query: impl Into<String>, anchor: Instant) -> RetrieveRequest {
        RetrieveRequest {
            prefix: prefix.into(),
            query: query.into(),
            query_embedding: None,
            limit: None,
            since: None,
            anchor,
            min_score: None,
            internals: false,
        }
    }
}

/// Whether a column names an identifier or an instant, which never enter a snippet
/// (`read.retrieve.identifiers-never-snippet`).
fn identifier_like(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with('_')
        || n == "id"
        || ["_id", "_key", "hash", "url", "uri", "link", "_at", "_date", "_time", "email"].iter().any(|s| n.ends_with(s))
        || n == "date"
        || n == "time"
}

/// Up to three snippet columns: label-priority text columns first, then the table's
/// other prose-worthy text columns in schema order (`read.retrieve.snippet`). A
/// partition column, and a column carrying a declared class or mask, never enters one.
fn snippet_columns(text: &[&str], sensitive: impl Fn(&str) -> bool) -> Vec<String> {
    let eligible: Vec<&str> = text.iter().copied().filter(|c| !identifier_like(c) && !sensitive(c)).collect();
    let mut out: Vec<String> = LABEL_COLUMNS.iter().filter(|l| eligible.contains(l)).map(|l| l.to_string()).collect();
    out.extend(eligible.iter().filter(|c| !LABEL_COLUMNS.contains(c)).map(|c| c.to_string()));
    out.truncate(3);
    out
}

/// One candidate row of one arm.
struct Row {
    table: String,
    id: String,
    values: Vec<(String, Value)>,
    snippet: Option<String>,
    lexical: Option<u32>,
    vector: Option<f64>,
    publication: Publication,
    basis_column: String,
    ingested: Option<Instant>,
}

fn text_of(v: &Engine) -> Option<String> {
    match cell(v.clone()) {
        Cell::Null => None,
        Cell::Text(s) => Some(s),
        other => match other.to_json() {
            Value::String(s) => Some(s),
            j => Some(j.to_string()),
        },
    }
}

fn vector_of(v: &Engine) -> Option<Vec<f32>> {
    match v {
        Engine::List(items) | Engine::Array(items) => items
            .iter()
            .map(|x| match x {
                Engine::Float(f) => Some(*f),
                Engine::Double(f) => Some(*f as f32),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

impl Face {
    /// A ranked read across the tables under a prefix. Each arm reads its table's
    /// registered relation, so restriction completes before the cut
    /// (`authority.compose.before-the-cut`).
    pub fn retrieve(&self, session: &Session, request: &RetrieveRequest, bounds: Bounds) -> Result<Response, ReadFault> {
        let arms: Vec<String> =
            session.relations().map(|r| r.name().to_string()).filter(|n| n.starts_with(&request.prefix)).collect();
        // The same least row ceiling a statement meets: grants, the request and every arm's
        // published `limits.max_rows` (`read.respond.row-ceiling`), capped at MAX_LIMIT.
        let asked = request.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
        let touched: std::collections::BTreeSet<String> = arms.iter().cloned().collect();
        let limit = self.ceiling(session, &touched, Some(asked), None).unwrap_or(asked);
        let tokens = content_tokens(&request.query);
        let floor = relevance_floor(&tokens, request.min_score);
        let window = candidate_window(limit);
        let engine = SqlEngine::open(session)?;
        let anchor = request.anchor;
        let memory_tables: Vec<String> = self.memory().tables.iter().map(|t| t.name.clone()).collect();
        let mut suppressed: BTreeMap<&'static str, u64> = BTreeMap::new();
        let mut recalled = false;
        let mut rows: Vec<Row> = Vec::new();
        for table in &arms {
            let schema = self.store.try_schema(table)?.map(|s| s.columns).unwrap_or_default();
            let policy = session.policy(table);
            let text: Vec<&str> = schema.iter().filter(|c| c.ty == ColumnType::Utf8).map(|c| c.name.as_str()).collect();
            let decl = self.decl(table);
            let snippet = snippet_columns(&text, |c| {
                decl.partition_by().iter().any(|p| p == c)
                    || policy.and_then(|p| p.columns.get(c)).is_some_and(|p| p.class.is_some() || p.mask.is_some())
            });
            let basis = PUBLICATION_COLUMNS
                .iter()
                .find(|p| schema.iter().any(|c| c.name == **p && matches!(c.ty, ColumnType::Timestamp | ColumnType::Utf8)))
                .map_or(INGESTED_AT, |p| *p)
                .to_string();
            let claims = self.memory().table(table).is_some_and(|t| t.shape == Shape::Facts);
            recalled |= claims;
            // A claims arm keeps only live claims in SQL, ahead of the window, so retired and
            // expired claims never take a live claim's place; the evidence gate runs on each
            // page read, and pages continue until the window fills with served claims.
            let (live, parameters) = if claims {
                let live = format!(
                    " WHERE {} IS NULL AND ({} IS NULL OR TRY_CAST({} AS TIMESTAMPTZ) > ?)",
                    ident("superseded_by"),
                    ident("valid_to"),
                    ident("valid_to")
                );
                (live, vec![Bound::Timestamp(anchor)])
            } else {
                (String::new(), Vec::new())
            };
            let (mut kept, mut offset) = (0u64, 0u64);
            loop {
                let sql = format!(
                    "SELECT * FROM {}{live} ORDER BY {} DESC, {} DESC, {} DESC LIMIT {window} OFFSET {offset}",
                    ident(table),
                    ident(INGESTED_AT),
                    ident(RUN_ID),
                    ident(ROW_SEQ)
                );
                let (columns, values) = engine.run_values(&sql, &parameters, None)?;
                let page = values.len() as u64;
                let cx = ArmContext {
                    engine: &engine,
                    session,
                    table,
                    claims,
                    anchor,
                    window,
                    snippet: &snippet,
                    basis: &basis,
                    tokens: &tokens,
                    request,
                    memory_tables: &memory_tables,
                };
                self.arm_rows(&cx, &columns, values, &mut kept, &mut rows, &mut suppressed);
                offset += window;
                if !claims || page < window || kept >= window {
                    break;
                }
            }
            // Each sidecar adds its candidates to the window, each read through the relation,
            // so a row it recalls scores exactly as the exact path scores it
            // (`read.retrieve.sidecar-generates-candidates`).
            let mut probes: Vec<(String, Vec<String>)> = Vec::new();
            if !claims {
                if let Some(query) = request.query_embedding.as_deref() {
                    probes.extend(self.sidecar_candidates(session, table, query, limit).ok());
                }
                // A table declaring no full-text sidecar reads no snapshot manifest here.
                let fulltext = decl.indexes().iter().any(|i| i.kind == IndexKind::Fulltext);
                if fulltext && !tokens.is_empty() {
                    probes.extend(self.fulltext_candidates(session, table, &tokens, limit).ok());
                }
            }
            for (id_column, ids) in probes {
                let cx = ArmContext {
                    engine: &engine,
                    session,
                    table,
                    claims,
                    anchor,
                    window: u64::MAX,
                    snippet: &snippet,
                    basis: &basis,
                    tokens: &tokens,
                    request,
                    memory_tables: &memory_tables,
                };
                let present: std::collections::HashSet<String> = rows.iter().map(|r| r.id.clone()).collect();
                let mut recalled_rows = Vec::new();
                let mut added = 0u64;
                for chunk in ids.chunks(REJOIN_CHUNK) {
                    let marks = vec!["?"; chunk.len()].join(", ");
                    let sql = format!("SELECT * FROM {} WHERE CAST({} AS VARCHAR) IN ({marks})", ident(table), ident(&id_column));
                    let parameters: Vec<Bound> = chunk.iter().map(|id| Bound::Text(id.clone())).collect();
                    let (columns, values) = engine.run_values(&sql, &parameters, None)?;
                    self.arm_rows(&cx, &columns, values, &mut added, &mut recalled_rows, &mut suppressed);
                }
                rows.extend(recalled_rows.into_iter().filter(|r| !present.contains(&r.id)));
            }
        }
        let prefloor = rows.len() as u64;
        rows.retain(|r| passes_floor(floor, r.lexical, r.vector));
        let candidates = rows.len() as u64;

        let index = LexicalIndex::build(&rows.iter().map(|r| r.snippet.as_deref()).collect::<Vec<_>>());
        let bm25 = index.bm25(&tokens);
        let matched = bm25.iter().filter(|s| s.is_some()).count() as u64;
        let lexical = min_max(&bm25);
        let ranking_empty = matched == 0 && rows.iter().all(|r| r.vector.is_none());
        let timeframe = request.since.map(|since| Timeframe { since: Some(since), anchor: request.anchor });
        let mut ranked: Vec<(Candidate, usize)> = rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let in_window = timeframe.is_none_or(|tf| tf.admits(r.publication));
                let c = Candidate {
                    id: r.id.clone(),
                    in_window,
                    fused: fuse(r.vector, lexical[i]),
                    recency: r.publication.instant().or(r.ingested),
                };
                (c, i)
            })
            .collect();
        let mut order_only: Vec<Candidate> = ranked.iter().map(|(c, _)| c.clone()).collect();
        order(&mut order_only, ranking_empty);
        let rank: std::collections::HashMap<&str, usize> = order_only.iter().enumerate().map(|(i, c)| (c.id.as_str(), i)).collect();
        ranked.sort_by_key(|(c, _)| rank[c.id.as_str()]);
        // One probe row past the ceiling sets `truncated` (`read.respond.truncation-is-exact`).
        ranked.truncate(usize::try_from(limit).unwrap_or(usize::MAX).saturating_add(1));

        let columns: Vec<String> = [
            "_table", "_row", "_snippet", "_score", "_vscore", "_in_window", "_date_basis", "_run_id", "_ingested_at", "_authored_by",
        ]
        .iter()
        .map(|s| s.to_string())
        .chain(RESERVED_PROJECTED.iter().map(|(out, _)| out.to_string()))
        .collect();
        let in_window = ranked.iter().take(usize::try_from(limit).unwrap_or(usize::MAX)).filter(|(c, _)| c.in_window).count() as u64;
        let out_rows: Vec<Vec<Value>> = ranked
            .iter()
            .map(|(c, i)| {
                let r = &rows[*i];
                let own: Map<String, Value> =
                    r.values.iter().filter(|(k, _)| !k.starts_with('_') && k != EMBEDDING_COLUMN).cloned().collect();
                let field = |name: &str| r.values.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()).unwrap_or(Value::Null);
                let ranking = RowRanking {
                    score: r.lexical,
                    vscore: r.vector,
                    in_window: c.in_window,
                    date_basis: r.publication.basis(&r.basis_column),
                };
                let ranking = serde_json::to_value(&ranking).expect("ranking fields serialize");
                let mut row = vec![
                    json!(r.table),
                    Value::Object(own),
                    json!(r.snippet),
                    ranking["_score"].clone(),
                    ranking["_vscore"].clone(),
                    ranking["_in_window"].clone(),
                    ranking["_date_basis"].clone(),
                    field(RUN_ID),
                    field(INGESTED_AT),
                    field(AUTHORED_BY),
                ];
                row.extend(RESERVED_PROJECTED.iter().map(|(_, source)| field(source)));
                row
            })
            .collect();
        let response = Response::cut(columns, out_rows, Some(limit));
        let block = RetrievalBlock {
            window,
            candidates_prefloor: prefloor,
            candidates,
            matched,
            returned: response.rows.len() as u64,
            in_window,
            deduped: 0,
            padded: 0,
            floor,
            since: request.since.map(|s| s.to_rfc3339()),
        };
        let mut response = response
            .with_block("retrieval", serde_json::to_value(block).expect("the retrieval block serializes"));
        if let Some(b) = bounds.echo() {
            response = response.with_block("bounds", b);
        }
        if recalled {
            // Counts per identifier; no suppressed claim is named (`read.recall.suppression-count`).
            let counts: Map<String, Value> = ["MemoryEvidenceUnresolved", "MemoryEvidenceOverflow"]
                .iter()
                .map(|id| (id.to_string(), json!(suppressed.get(id).copied().unwrap_or(0))))
                .collect();
            response = response.with_block("recall", json!({ "suppressed": counts }));
        }
        Ok(response)
    }

    /// The `id_column` and the candidate identifiers the table's current vector sidecar
    /// yields for `query`, or why the arm takes the exact scan
    /// (`read.retrieve.sidecar-falls-back`). The probe widens where the session restricts
    /// the table, since it sees none of the restriction (`read.retrieve.sidecar-oversampling`).
    pub fn sidecar_candidates(&self, session: &Session, table: &str, query: &[f32], limit: u64) -> Result<(String, Vec<String>), Fallback> {
        let (dir, entry) = vector::current_entry(&self.store, table, EMBEDDING_COLUMN, query.len())?;
        let id_column = entry.get("id_column").and_then(|c| c.as_str()).ok_or(Fallback::ManifestMismatch)?.to_string();
        let policy = session.policy(table);
        if let Some(p) = policy {
            let withheld = |c: &str| p.columns.get(c).is_some_and(|c| c.mask.is_some()) || !p.column_set(c).admits(session.zone());
            if withheld(&id_column) || withheld(EMBEDDING_COLUMN) {
                return Err(Fallback::Withheld);
            }
        }
        let sidecar = VectorSidecar::open(&dir, table, &entry, &self.store.sealing())?;
        let restricted =
            session.tenant_scoped() || policy.is_some_and(|p| p.rows.is_some() || p.columns.values().any(|c| c.mask.is_some()));
        let k = usize::try_from(sidecar_probe_size(limit, restricted)).unwrap_or(usize::MAX);
        let ids = sidecar.probe(query, k)?.into_iter().map(|c| c.id).collect();
        Ok((id_column, ids))
    }

    /// The `id_column` and the candidate identifiers the table's current full-text
    /// sidecars yield for the content `tokens`, or why the arm adds none
    /// (`read.retrieve.fulltext-probe`, `read.retrieve.sidecar-falls-back`). A sidecar over
    /// a column the session masks, classes or zone-withholds, or keyed on an identifier it
    /// masks or withholds, is never probed, since its matches would rank rows by text the
    /// reader cannot see.
    pub fn fulltext_candidates(&self, session: &Session, table: &str, tokens: &[String], limit: u64) -> Result<(String, Vec<String>), Fallback> {
        let (dir, snapshot_id, entries) = fulltext::current_entries(&self.store, table)?;
        let policy = session.policy(table);
        let restricted =
            session.tenant_scoped() || policy.is_some_and(|p| p.rows.is_some() || p.columns.values().any(|c| c.mask.is_some()));
        let k = usize::try_from(sidecar_probe_size(limit, restricted)).unwrap_or(usize::MAX);
        let (mut id_column, mut first_fallback) = (None, None);
        let mut ids: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for entry in &entries {
            let probed = (|| -> Result<(String, Vec<String>), Fallback> {
                let field = |k: &str| entry.get(k).and_then(|v| v.as_str()).ok_or(Fallback::ManifestMismatch);
                let (id, column, path) = (field("id_column")?, field("column")?, field("path")?);
                let key_version = entry.get("key_version").and_then(|v| v.as_u64()).ok_or(Fallback::ManifestMismatch)?;
                if let Some(p) = policy {
                    let withheld = |c: &str| p.columns.get(c).is_some_and(|c| c.mask.is_some()) || !p.column_set(c).admits(session.zone());
                    if withheld(id) || withheld(column) || p.columns.get(column).is_some_and(|c| c.class.is_some()) {
                        return Err(Fallback::Withheld);
                    }
                }
                let key = fulltext::fingerprint(table, &snapshot_id, path, u32::try_from(key_version).unwrap_or(u32::MAX));
                let sidecar = self.fulltext.get_or_open(&key, || FulltextSidecar::open(&dir, table, entry, &self.store.sealing()))?;
                Ok((id.to_string(), sidecar.probe(tokens, k)?.candidates.into_iter().map(|c| c.id).collect()))
            })();
            match probed {
                Ok((id, found)) => {
                    id_column = Some(id);
                    ids.extend(found.into_iter().filter(|i| seen.insert(i.clone())));
                }
                Err(f) => {
                    first_fallback.get_or_insert(f);
                }
            }
        }
        match id_column {
            Some(c) => Ok((c, ids)),
            None => Err(first_fallback.unwrap_or(Fallback::NoSidecar)),
        }
    }

    /// The face's cache of opened full-text sidecars (`read.rank.lexical-index-cache`).
    pub fn fulltext_cache(&self) -> &SidecarCache<FulltextSidecar> {
        &self.fulltext
    }

    /// How one evidence row reads through the caller's session: its table registered, the
    /// row visible through the relation, and no cell of the table masked or nulled by zone.
    fn evidence_read(&self, engine: &SqlEngine, session: &Session, r: &EvidenceRef) -> EvidenceRead {
        let Some(relation) = session.relation(&r.table) else { return EvidenceRead::UnknownTable };
        let sql = format!("SELECT count(*) FROM {} WHERE {} = ? AND {} = ?", ident(relation.name()), ident(RUN_ID), ident(ROW_SEQ));
        let found = engine
            .run(&sql, &[Bound::Text(r.run.clone()), Bound::Integer(r.seq)], None)
            .ok()
            .and_then(|(_, rows)| rows.first().and_then(|row| row.first().cloned()))
            .is_some_and(|c| matches!(c, Cell::Integer { value, .. } if value > 0));
        if !found {
            return EvidenceRead::Unreadable;
        }
        let masked = session.policy(&r.table).is_some_and(|p| {
            p.columns.values().any(|c| c.mask.is_some())
                || p.columns.keys().any(|c| !p.column_set(c).admits(session.zone()))
        });
        if masked {
            EvidenceRead::Masked
        } else {
            EvidenceRead::Readable
        }
    }
}

/// What one arm's rows are read against.
struct ArmContext<'a> {
    engine: &'a SqlEngine,
    session: &'a Session,
    table: &'a str,
    claims: bool,
    anchor: Instant,
    window: u64,
    snippet: &'a [String],
    basis: &'a str,
    tokens: &'a [String],
    request: &'a RetrieveRequest,
    memory_tables: &'a [String],
}

impl Face {
    /// Take one page of an arm's rows as candidates, up to the window.
    fn arm_rows(
        &self,
        cx: &ArmContext<'_>,
        columns: &[String],
        values: Vec<Vec<Engine>>,
        kept: &mut u64,
        rows: &mut Vec<Row>,
        suppressed: &mut BTreeMap<&'static str, u64>,
    ) {
        let at = |name: &str| columns.iter().position(|c| c == name);
        for v in values {
            if *kept == cx.window {
                break;
            }
            let get = |name: &str| at(name).map(|i| &v[i]);
            if cx.claims {
                // Recall serves a claim only while live, and only when every evidence
                // row reads through this session (`read.recall.ranked-arm`).
                let superseded = get("superseded_by").and_then(text_of).is_some();
                let ended = match get("valid_to").map(|x| cell(x.clone())) {
                    Some(Cell::Timestamp(end)) => end <= cx.anchor,
                    Some(Cell::Null) | None => false,
                    Some(other) => Publication::cast(match other.to_json() {
                        Value::String(s) => Some(s),
                        _ => None,
                    }
                    .as_deref())
                    .instant()
                    .is_none_or(|end| end <= cx.anchor),
                };
                if superseded || ended {
                    continue;
                }
                let evidence = get("evidence").and_then(text_of);
                if let Err(e) = gate(evidence.as_deref(), cx.memory_tables, |r| self.evidence_read(cx.engine, cx.session, r)) {
                    *suppressed.entry(e.identifier()).or_insert(0) += 1;
                    continue;
                }
            }
            let snippet_text: Vec<String> = cx.snippet.iter().filter_map(|c| get(c).and_then(text_of)).collect();
            let snippet_text = (!cx.snippet.is_empty()).then(|| snippet_text.join(" — "));
            let publication = match get(cx.basis).map(|p| cell(p.clone())) {
                Some(Cell::Timestamp(t)) => Publication::At(t),
                Some(Cell::Null) | None => Publication::Null,
                Some(other) => Publication::cast(match other.to_json() {
                    Value::String(s) => Some(s),
                    _ => None,
                }
                .as_deref()),
            };
            let ingested = match get(INGESTED_AT).map(|p| cell(p.clone())) {
                Some(Cell::Timestamp(t)) => Some(t),
                _ => None,
            };
            let vector = match (&cx.request.query_embedding, get(EMBEDDING_COLUMN).and_then(vector_of)) {
                (Some(q), Some(row)) => cosine(q, &row),
                _ => None,
            };
            let id = format!(
                "{}\u{1f}{}\u{1f}{}",
                cx.table,
                get(RUN_ID).and_then(text_of).unwrap_or_default(),
                get(ROW_SEQ).and_then(text_of).unwrap_or_default()
            );
            let values = columns.iter().zip(&v).map(|(c, x)| (c.clone(), cell(x.clone()).to_json())).collect();
            *kept += 1;
            rows.push(Row {
                table: cx.table.to_string(),
                id,
                lexical: lexical_score(cx.tokens, snippet_text.as_deref()),
                snippet: snippet_text,
                vector,
                publication,
                basis_column: cx.basis.to_string(),
                ingested,
                values,
            });
        }
    }
}
