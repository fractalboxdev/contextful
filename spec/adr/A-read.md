# A-read — Reading and memory decisions

**Status:** accepted

## The read surface admits one read-only statement over registered relations

`read.guard` walks the syntax tree the executor itself serializes and admits exactly one read-only `SELECT` whose base relations are views registered for the caller or declared common table expressions; table functions and catalog reaches refuse, and a refusal never echoes another caller's relation. `read.register` lists only granted templates and binds arguments strictly. A filter's budget applies to the whole filter. `read.respond` reads a preview through its table's relation. The process transport requires a token, with an explicit owner flag for its signed store claim; the run-stream socket authenticates before upgrade and carries snapshots only.

| Option | Lost on | Cost |
| --- | --- | --- |
| Executor's own tree plus a relation allowlist *(chosen)* | — | Each statement serializes once before execution; a harmless unregistered relation still refuses. |
| Token or regex blocklist over the text | Completeness | Every new engine verb or dialect spelling is a bypass until listed. |
| An independent SQL parser | Parser agreement | What the guard admits is not what the executor runs. |
| Check only top-level `FROM` items | Coverage | A subquery or common table expression carries the forbidden relation. |

Consequences: a preview and a query apply identical row restriction, masks and zone gate; a socket connection mutates nothing.

## A pooled engine reuses a resolved session only while its whole key holds

**Status:** accepted

Context: each `Face` call walks `tables/`, reads every granted `schema.json` and run manifest, then opens a connection, registers the mask functions, fills subject and tenant tables and creates one view per relation; every Parquet footer re-reads.
Decision: `Face` pools resolved sessions with their connections, keyed on the admitted authority with token id and revocation epoch, the request zone, the bounds, the table set under `tables/`, and per granted table its `schema.json` digest, pointer and ledger file set. Any change misses. The pool sits behind the guard, masks and zone gate.
Gate: a `contextful-context` benchmark reports cold and warm `Face::query` p50 and p95 at 1, 50 and 500 unfolded runs, one statement, 200 iterations after 20 warm-up, on the reference target. Setup above 25% of warm p95 at 50 runs admits the pool.

| Option | Lost on | Cost |
| --- | --- | --- |
| Whole-key pool inside `Face`, benchmark-gated *(chosen)* | — | Key computation still lists ledgers and stats schemas; connections hold memory per authority. |
| Key on the pointer set alone | Freshness | An appended run, schema edit or new table serves a stale view set. |
| Cache in the embedder | Enforcement | Guard, masks and zone gate sit outside the cache. |
| A time-to-live on sessions | Invalidation by construction | A revoked token reads until expiry. |
| Rebuild per call | Warm latency | Setup scales with unfolded runs on every read. |

Criteria: no reuse crosses a snapshot, schema, table set or revocation, fixed; warm latency decides.
Consequences: `read.register.connection-views` rewords from per statement to per pool entry once the gate admits the pool.

## A result is reused only under the read frontier it was computed at

**Status:** accepted

Context: a dashboard issues one statement per panel over one shared read frontier, and each repeat executes again on the engine.
Decision: `read.cache` keeps a statement's response behind the guard, masks and zone gate, keyed on the session's principal, each touched relation with the files it reads and its request-ledger stamps, the bounds, the statement, its parameter values and the applied row ceiling. A table opts in with a time to live; a table declaring `private = true` never caches. A process declares a byte budget, least recently used evicted first, with no default.
Criteria: no hit crosses a restriction, revocation or frontier, fixed; then repeat-read cost.

| Option | Lost on | Cost |
| --- | --- | --- |
| Key on touched relations and their read frontier *(chosen)* | — | A hit still admits the statement and stamps ledger files; a truncated result caches as truncated. |
| Key on resolved snapshot ids | Freshness | A run landed before the next fold changes the result under the same ids. |
| A time to live as the staleness guard | Invalidation by construction | Every commit reads stale until expiry. |
| Cache outside the face | Enforcement | The cache duplicates the session's policy fingerprint, and one missed field crosses a restriction. |
| A default budget | Silence | A guessed number meets production memory unreviewed. |

Consequences: a commit on an untouched table leaves an entry live.

## A zone-withheld relation is named, with one whole-relation count

`read.respond` names each touched relation the session's zone excludes or column-masks in a restriction block that every transport serializes; `context.describe` states the session zone and whether each table admits it. `authority.place` counts the rows the zone step removes over the whole relation, after the tenant and row-policy steps, never over the caller's statement. `authority.compose.before-the-cut` holds: the count is a property of the relation, so no ranked position, filter or requested size moves it.

| Option | Lost on | Cost |
| --- | --- | --- |
| Name plus a whole-relation count *(chosen)* | — | The count discloses how many in-scope rows a placement withholds; each withheld relation costs one count query per read. |
| Empty result, nothing named | Distinguishability | A zone-emptied relation reads as a table holding no data. |
| Refuse the read | Composition | One excluded arm fails a prefix-wide ranked read or a join. |
| Count over the caller's statement | Oracle resistance | Varying a predicate turns the count into a probe of withheld values, and a count per cut traces withheld positions. |
| Name without a count | Diagnosis | A caller cannot tell an empty excluded table from a populated one. |

Consequences: an empty result carrying no block means no touched relation was withheld by zone.

## The networked read transport is MCP Streamable HTTP, admitted per request

**Status:** accepted

Context: the stdio tool server answers only its spawning process, one statement at a time, under one startup credential.
Decision: `read.register` serves the same tool server as MCP Streamable HTTP at `POST /mcp`, one JSON-RPC message per request, with no raw surface. Each request is admitted and revocation-checked on its own credential. The in-flight ceiling is required, with no default; past it a request answers `503`.
Criteria: MCP-client compatibility, then one surface per guarantee, then revocation latency; the first decided it.

| Option | Lost on | Cost |
| --- | --- | --- |
| MCP Streamable HTTP around the tool server *(chosen)* | — | A tool result reaches a caller inside a JSON-RPC envelope; the face holds no session and no server stream. |
| One HTTP route per read tool | MCP-client compatibility | An MCP client needs its own adapter, and the tool set gains a second envelope. |
| A pool of stdio children per consumer | Reach | The consumer shares the store's host; each child pays admission. |
| Admission once per connection or session | Revocation latency | A revoked credential reads until its connection closes. |
| A default ceiling | Silence | A guessed number meets production load unreviewed. |

Consequences: admission sits on every read, and a possession-proof check sits on every read under a holder-bound credential; an MCP client that signs nothing reads with a short-lived audience-bound bearer and re-exchanges it before expiry.
Revisit: a caller needs server-initiated messages or resumable streams.

## Memory writes validate or dead-letter, and outcomes settle under their source

`read.synthesize` validates every candidate against the declared output schema, retries with the error up to 3 attempts per batch, then dead-letters the response, template hash and drop reason with the cursor held. The relation vocabulary is a reserved core plus declared types; an undeclared edge dead-letters while the batch lands. `read.resolve-entity` dead-letters ambiguous mentions and dangling endpoints. `read.settle` requires one resolution form and one source — `metric`, `adjudicator` or `manual`; metric comparators evaluate outside the engine, verdicts carry an `http`/`https` citation, self-rated outcomes carry a null verdict, and the scored and unresolved views partition the join.

| Option | Lost on | Cost |
| --- | --- | --- |
| Validate, dead-letter, settle under the registered source *(chosen)* | — | Dead-lettered candidates wait for a human; extraction spends up to 3 model calls per batch. |
| Parse a non-conforming response best-effort | Provenance | Unvalidated text becomes an untraceable row. |
| Resolve an ambiguous mention to the highest score | Reversibility | A wrong merge propagates through every claim about both entities. |
| Evaluate metric rules with an embedded expression engine | Attack surface | Anyone registering a prediction reaches the evaluator. |
| Exclude self-rated outcomes in each reader | Repetition | The first reader that forgets the filter inflates calibration. |

## Claim standing is one derived tier, and revision is scoped to a validity line

`read.synthesize` stamps `tier` — `curated`, `derived` or `researched` — from the writing grant and the run's source, never the payload; mixed grounding takes the lowest tier. `read.recall` orders by tier, then score. `read.revise` lets a claim retire a prior of equal or higher standing; a lower-standing contradiction lands with a bounded validity end, and two claims revise each other only when both are open-ended or anchor to the same instant. Promotion is a human act, stamped beside the synthesizing agent. A fetched result's contributor key is its registrable domain; a coverage gap closes only on a declared source. Only `researched` claims expire, by stamping a validity end, not deleting.

| Option | Lost on | Cost |
| --- | --- | --- |
| One derived tier with lexicographic precedence *(chosen)* | — | A high-scoring researched claim ranks below a weak curated one. |
| A caller-supplied tier | Forgery | Any writer promotes its own output. |
| A separate store for self-directed conclusions | Recall coherence | Every reader merges two stores under its own rule. |
| A confidence ceiling per tier | Ordering | Scores across sources are incomparable, so ceilings leak. |
| Retire on subject, predicate and scope alone | History | A past-anchored belief retires the present one. |

## Memory access follows evidence grants, and forgetting is a separate privilege

`read.recall` serves a claim only when every evidence row resolves through the caller's own enforced session; otherwise the claim is suppressed with `MemoryEvidenceUnresolved`, and a claim naming more than 256 evidence entries with `MemoryEvidenceOverflow`, without failing the recall. `disclosure.erase` sits outside the default grant set and raises `ErasureUngranted` without the forget grant; tombstones and cascade markers commit in one local snapshot before the verb returns.

| Option | Lost on | Cost |
| --- | --- | --- |
| Per-row resolution at recall, 256 cap, separate forget grant, same-commit markers *(chosen)* | — | A claim whose source table is renamed goes silent; erasure needs a second credential and a heavier commit. |
| Serve the claim with an unresolved-evidence marker | Containment | The marker discloses the conclusion the gate holds. |
| Resolve evidence once at write time and cache it | Revocation latency | Later masks, grants and erasures never reach the cached verdict. |
| Forget inside the write grant | Blast radius | A routine synthesis credential erases curated claims. |
| Markers on the next ordinary commit | Marker durability | A replica pulling in between resurrects forgotten rows. |

Consequences: revocation takes effect on the next recall with no invalidation sweep; total recall cost of the evidence join is unmeasured.
Revisit: recall latency dominated by the evidence join; a suppression channel that names a withheld claim without its content; evidence lists clustering at the 256 cap.

## A citation into a keyed table resolves through a pepper-keyed digest of its key

A fold drops a key's superseded versions, so a citation resolved by run and sequence alone withdraws its claim at the next compaction. The landing write stamps each citation into a keyed table with a digest of the cited key under a secret derived from the mask pepper; recall matches a live row of that key when the cited row no longer reads, and counts the claim stale. The memory row stores no source value, and no caller holds the secret to test a guessed key.

| Option | Lost on | Cost |
| --- | --- | --- |
| Pepper-keyed digest of the key, stale claims counted *(chosen)* | — | A stale claim may contradict its key's live version, signalled only as a count; a pepper rotation or key change returns citations to run-and-sequence resolution. |
| Key cells copied into the citation | Containment | Every memory reader reads source key values outside the source table's grants, masks and erasure. |
| The fold keeps every cited version | Erasure reach | Compaction retains what memory cites, and erasing a key must find each retained version. |
| Withdraw the claim at the fold | Recall stability | A routine compaction silently forgets memory. |

Consequences: a fold no longer withdraws memory; each recall of a stale citation scans its table for the digest.
Revisit: stale counts dominating a recall; a reader needing the claim's cited version rather than its key's live one.

## The store holds nothing a user could not see

Query-time enforcement bounds a read, not the stored bytes, so the ceiling sits at the write path. `authority.refuse` rejects a credential-shaped value per value with `EnforceCredentialShapedValue`, landing the surrounding rows; an operator-declared exemption admits a source that legitimately carries such text. `connector.source` refuses every organization-twin API — security, eDiscovery, legal-hold export — with `ConnectorTwinApiSource`; a sanctioned, paid, disclosed organization-wide export path is admitted.

| Option | Lost on | Cost |
| --- | --- | --- |
| Refuse at the write boundary; refuse twin APIs everywhere *(chosen)* | — | A security corpus needs an explicit exemption; full coverage means per-user ingestion, with more connections and rate-limit budget. |
| Store and flag the value, or withhold it at query time | Reach | The value sits in every replica, readable with an object-store credential. |
| Refuse the whole batch containing the value | Proportion | One pasted key stops the pull on every retry. |
| Ingest through the twin API and restrict at query time | The ceiling | No query-time restriction lowers what the store contains. |
| Permit the twin API behind an organization-signed disclosure | Consent of the people ingested | The organization's signature is not their consent. |

Consequences: removing a credential never needs a rewrite across replicas, because it never landed.

## Nearness is a parent closure over declared edges, and the engine holds no geospatial type

`read.resolve-entity` answers nearness as the transitive closure over the deployment's declared parent edges: near a place means at it or under it. The closure materializes once and left-joins as an ordinary dimension, so an unmapped place tags null and the row survives. The engine holds no geometry type, distance function or radius.

| Option | Lost on | Cost |
| --- | --- | --- |
| Parent closure as an ordinary dimension *(chosen)* | — | A radius question is unanswerable; hierarchy gaps show as null tags, not errors; the opaque `place_id` is expensive to reverse. |
| A geospatial column type with radius search | Scope | A type system, index family and projection question enter for a workload that needs none. |
| Coordinate pairs with ad-hoc distance in SQL | Correctness | Raw latitude-longitude distance errs by an unstated latitude-dependent margin. |
| An external spatial service at query time | Consistency | A bounded read mixes two stores' clocks. |

Consequences: the place dimension replicates, reads at a vantage and falls under enforcement like any row; metric answers land as rows computed outside.
Revisit: a deployment declares artificial parent levels to approximate a radius; closure materialization dominates the dimension's build.

## Each statement owns its deadline and serialized response budget

Caller, grant and table limits select the least duration and byte ceilings. An absent limit imposes none. The read face interrupts the timed-out statement's own engine connection; another statement in the process keeps running. Byte accounting includes the response envelope and cut metadata, and admits only whole rows. A response whose first row cannot fit refuses rather than looking empty.

| Option | Lost on | Cost |
| --- | --- | --- |
| Per-connection interrupt and serialized row probes *(chosen)* | — | Each bounded statement needs a deadline watcher; byte-bounded reads serialize candidate rows before delivery. |
| Kill the serving process at a deadline | Isolation | Other statements and pooled sessions die with the timed-out one. |
| Measure elapsed time and response bytes after execution | Resource ceiling | Expensive work and oversized responses complete before a refusal. |

Consequences: a caller distinguishes a deadline refusal from a whole-row byte cut; neither returns a partial row or reports success for an oversized first row.

## Memory vocabulary and schedules are explicit

The five reserved relations cover evidence, contradiction, succession, subject and origin without assigning deployment-specific meanings. A declaration extends the vocabulary; the rename verb rewrites stored edges before the old name leaves the manifest. Synthesis runs when invoked unless the deployment supplies a schedule for that shape. A schedule does not alter the pass cursor.

| Option | Lost on | Cost |
| --- | --- | --- |
| Small core, declared extensions and operator-triggered default *(chosen)* | — | A deployment maintains migrations and schedules for its own relation types. |
| Open-ended relation strings | Validation | A typo becomes an edge type that no other consumer recognizes. |
| One global schedule | Workload fit | Shapes with different source rates either lag or spend idle model calls. |
| Automatic pass on every commit | Cost | A bulk landing launches repeated extraction before its source is complete. |

## Memory confidence and retention separate history from ranking

The source's emitted confidence is uncalibrated until a per-shape-and-predicate isotonic map improves held-out Brier score on settled outcomes. Reports expose sample count, Brier score, expected calibration error and that held-out score. A shape may opt into ranking decay with a declared half-life; recorded claims keep their validity until expiry or erasure. A stale citation stays gated, and a newly committed source row is the only automatic synthesis trigger. Equal-instant unscoped contradictions enter the dead-letter table without silently retiring either claim. An earlier direct observation keeps the existing out-of-order refusal.

| Option | Lost on | Cost |
| --- | --- | --- |
| Validated calibration, hard history and explicit decay *(chosen)* | — | Sparse shape-and-predicate groups remain uncalibrated; conflicts need resolution. |
| Treat model confidence as comparable | Calibration | Two model outputs with the same number can have different outcome frequencies. |
| Decay validity intervals by default | History | A later rank policy erases what the workspace once concluded. |
| Resynthesize each stale read | Read latency | A query can launch an unbounded model job and change the source cursor. |

## Memory reads expose use and resolve ownership from enforced rows

The request ledger records returned claim ids under the caller and frontier. Ownership answers include every visible attached principal, ordered by attachment time and id. An external graph engine is a derived index; the store's enforced rows remain the answer source.

| Option | Lost on | Cost |
| --- | --- | --- |
| Ledger use, complete ownership and store-backed answers *(chosen)* | — | Claim-id logs consume storage; ownership answers can grow with attachments. |
| Infer usage from citations | Observation | A claim can be read many times without another claim citing it. |
| Return the newest owner only | Completeness | Joint ownership silently loses every older live attachment. |
| Serve directly from a graph backend | Enforcement | Its grant, mask, zone and frontier rules become a second implementation. |
