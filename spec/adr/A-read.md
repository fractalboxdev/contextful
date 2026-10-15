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

## A restricted sidecar probe doubles for a bounded number of rounds and reports under-fill

Context: rows a reader cannot see can cluster near a query point, so one oversampled probe re-joined through the reader's restriction returns fewer rows than the limit.
Decision: `read.retrieve` doubles the probe and probes again while visible rows under-fill the limit, for at most 4 rounds, then answers with the rows it holds and reports the under-fill in the retrieval block.
Criteria: bounded work per read, then recall, then an honest count.

| Option | Lost on | Cost |
| --- | --- | --- |
| Doubling for 4 rounds, under-fill reported *(chosen)* | — | A reader whose visible rows sit past the last round's reach receives fewer rows than the limit, named as such. |
| One fixed oversampling factor | Recall | A clustered restriction under-fills silently. |
| Probe until the limit fills | Bounded work | A reader seeing few rows walks the whole graph. |
| Estimate visibility per reader before probing | Simplicity | A per-reader statistic is one more input for a restriction to leak through. |

Consequences: an under-filled read costs up to 4 probes and re-joins.
Revisit: under-fill reported on more than a small share of restricted reads.

## Reciprocal rank calibrates the lexical leg, admitted by the native evaluation

Context: fusion adds a bounded cosine leg to a corpus-relative BM25 leg; min-max over the window let one outlier compress every other lexical score.
Decision: `read.rank` calibrates the BM25 leg by reciprocal rank, `61 / (60 + r)`. A calibration replaces the current one only when the native evaluation scores it at or above the current one on every tracked measure, on every leg; reciprocal rank met that against min-max and raised the hybrid leg's nDCG and R-precision.
Criteria: no measured regression, fixed; then calibration across corpora.

| Option | Lost on | Cost |
| --- | --- | --- |
| Reciprocal rank, admitted by the evaluation *(chosen)* | — | Score magnitudes stop informing order; the comparison runs the native set twice. |
| Keep min-max normalization | Calibration | One outlier score flattens the lexical leg in every window. |
| Swap without a gate | Measured quality | A ranking change ships without evidence it ranks no worse. |
| Learned score calibration | Determinism | A trained map adds a model per corpus and drifts as it grows. |

Revisit: a corpus where reciprocal rank loses a tracked measure to another calibration.

## A reranker reorders a masked post-authority window

Context: fused order scores a cosine leg and a reciprocal-rank lexical leg; a cross-encoder reading query and candidate together orders the leading rows better, at the price of a model call per candidate.
Decision: `read.rank` reranks only on a call naming `rerank_with`, over the first 50 rows of fused order after authority has cut and masked them. The reranker reads each masked snippet alone and scores it pointwise; rows past the window keep fused order. A reranker enters the declared set through the same native-evaluation gate a lexical calibration meets. A remote reranker is egress through `connector.infer.model-endpoint`, and a horizon routes it as it routes an embedder; a reranker with no checkpoint cut off before the horizon refuses. An undeclared name refuses.

| Option | Lost on | Cost |
| --- | --- | --- |
| Pointwise rerank of a masked 50-row window, on request *(chosen)* | — | A reranked read costs up to 50 model scores and depends on the reranker's build. |
| Rerank every ranked read by default | Determinism | Every answer changes with a model build the caller never named. |
| Listwise reranking in one prompt | Injection resistance | One poisoned candidate can instruct the model to promote itself. |
| Rerank before authority | Enforcement | Hidden rows reach a model, and their scores shape what the caller sees. |
| Fall back to fused order on an unknown name | Distinguishability | A ranking change passes silently. |

Consequences: the result cache keys on the reranker's identity and build, and the retrieval block names both.
Revisit: the native evaluation measuring no gain for a declared reranker over fused order.

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

## A served claim write stays outside read MCP

Context: an operator answer distils claims after its read turn, while a hosted console owns no local store root. Criteria: preserve the closed read tool set, the writer's actor and session, and the memory table's commit and evidence gates.

Decision: a separate served route admits the server-held credential, binds the actor and session to its subject tuple, and calls the direct memory write. The credential needs a narrow write grant on the declared claims table. The browser holds no writer credential; an Origin header refuses. The read-only edge profile mounts no write route.

| Option | Lost on | Cost |
| --- | --- | --- |
| Separate route and direct memory write *(chosen)* | — | The hosted console needs a writing credential whose task names its reading session. |
| Add `memory.write` to read MCP | Closed read tool set | A client-reachable tool can mutate a store. |
| Spawn the CLI from the console | Hosted reach | The console needs the store's local project root and signing configuration. |
| Keep claims in the console process | Durability | Restart loses conclusions and bypasses memory recall and erasure. |

Consequences: claim writes share memory's evidence, supersession, attribution and commit path. A credential with only read grants cannot land a claim.

Integration: `CONTEXTFUL_CONSOLE_MEMORY_SESSION_CLAIM` names a stable signed claim. Exchange maps `sub` to `on_behalf_of`, that claim to `task`, and grants evidence/claims read plus claims write. Missing mappings, tables or evidence suppress landing. Subject-keyed recall admits broad claims-table readers; the console filters its exact actor/session scope before model input.

## Memory writes validate or dead-letter, and outcomes settle under their source

`read.synthesize` validates every candidate against the declared output schema, retries with the error up to 3 attempts per batch, then dead-letters the response, template hash and drop reason with the cursor held. The relation vocabulary is a reserved core plus declared types; an undeclared edge dead-letters while the batch lands. `read.resolve-entity` dead-letters ambiguous mentions and dangling endpoints. `read.settle` requires one resolution form and one source — `metric`, `adjudicator` or `manual`; metric comparators evaluate outside the engine, verdicts carry an `http`/`https` citation, self-rated outcomes carry a null verdict, and the scored and unresolved views partition the join.

| Option | Lost on | Cost |
| --- | --- | --- |
| Validate, dead-letter, settle under the registered source *(chosen)* | — | Dead-lettered candidates wait for a human; extraction spends up to 3 model calls per batch. |
| Parse a non-conforming response best-effort | Provenance | Unvalidated text becomes an untraceable row. |
| Resolve an ambiguous mention to the highest score | Reversibility | A wrong merge propagates through every claim about both entities. |
| Evaluate metric rules with an embedded expression engine | Attack surface | Anyone registering a prediction reaches the evaluator. |
| Exclude self-rated outcomes in each reader | Repetition | The first reader that forgets the filter inflates calibration. |

## Gleaning is a declared, single extra pass

Context: one extraction pass over a batch omits claims the batch supports, and a second prompt asking for what the first missed recovers some of them.
Decision: `read.declare` admits `gleanings` of 0 or 1, off by default. `read.synthesize` re-prompts a schema-valid batch once, gives the gleaning response one attempt, and commits gleaned and first-pass claims in the batch's one commit; a gleaned claim carries `_pass = 'glean'`. A schema-invalid gleaning response discards only the gleaned claims and is counted.
Criteria: bounded model spend per batch, fixed; then recall of supported claims.

| Option | Lost on | Cost |
| --- | --- | --- |
| Declared, off by default, one pass *(chosen)* | — | A table opting in spends one more model call per batch, and its claims vary with the extra prompt. |
| Always on | Model spend | Every deployment pays a second call per batch whether its sources need one or not. |
| Unbounded passes until no new claim | Bounded work | A model that keeps emitting claims holds the cursor and spends without limit. |

Consequences: a failed gleaning never dead-letters a batch or holds its cursor.
Revisit: gleaned claims superseded or contradicted at a rate above first-pass claims.

## An extraction cache keys responses on fenced bytes, template and model

Context: re-running synthesis over unchanged rows, after a rebuild or an erasure's restaging, repeats model calls whose answer is already known.
Decision: `read.synthesize` keeps each schema-valid response in an enforced, tenant-scoped table keyed on the fenced batch bytes' digest, the template hash, the model identifier and its cutoffs. Keying on fenced bytes keeps two credentials with different masks apart. A hit copies claims and stamps attribution fresh. The cache is a table, never a side file, under "The store holds nothing a user could not see". It holds model responses and never evidence verdicts, because "Resolve evidence once at write time and cache it" stays rejected.
Criteria: no response crosses a mask or tenant, fixed; then repeated model spend.

| Option | Lost on | Cost |
| --- | --- | --- |
| Enforced table keyed on fenced bytes, template and model *(chosen)* | — | Cached responses occupy store space, and each entry's provenance must name its batch's rows for an erasure to reach it. |
| Key on source row identity | Containment | Two sessions masking one row differently share a response one of them never saw. |
| A side file outside the store | Enforcement | Its bytes escape grants, masks, replication rules and erasure. |
| No cache | Model spend | Every re-run repays every call. |

Consequences: a re-run over unchanged batches makes no model call; a changed template or model misses.

## Claim standing is one derived tier, and revision is scoped to a validity line

`read.synthesize` stamps `tier` — `curated`, `derived` or `researched` — from the writing grant and the run's source, never the payload; mixed grounding takes the lowest tier. `read.recall` orders by tier, then score. `read.revise` lets a claim retire a prior of equal or higher standing; a lower-standing contradiction lands with a bounded validity end, and two claims revise each other only when both are open-ended or anchor to the same instant. Promotion is a human act, stamped beside the synthesizing agent. A fetched result's contributor key is its registrable domain; a coverage gap closes only on a declared source. Only `researched` claims expire, by stamping a validity end, not deleting.

| Option | Lost on | Cost |
| --- | --- | --- |
| One derived tier with lexicographic precedence *(chosen)* | — | A high-scoring researched claim ranks below a weak curated one. |
| A caller-supplied tier | Forgery | Any writer promotes its own output. |
| A separate store for self-directed conclusions | Recall coherence | Every reader merges two stores under its own rule. |
| A confidence ceiling per tier | Ordering | Scores across sources are incomparable, so ceilings leak. |
| Retire on subject, predicate and scope alone | History | A past-anchored belief retires the present one. |

## Agent and third-party writes stand derived

Context: a direct write stamps `curated`, and recall puts curated claims first. An agent writing through ordinary calls, or a claim citing third-party rows, then outranks synthesis on the strength of the write path alone, which lets injected text become a top-ranked belief.
Decision: `read.revise` stamps a direct write `derived` when its credential carries an agent member or a cited row reads `ingested:third-party`, an unlabelled row included; promotion to `curated` stays a human act, as "Claim standing is one derived tier, and revision is scoped to a validity line" decides. `read.declare` lets a claims table withhold claims below a minimum serving taint.
Criteria: no model-chosen claim outranks a principal's, fixed; then recall order.

| Option | Lost on | Cost |
| --- | --- | --- |
| Agent and third-party writes stand derived *(chosen)* | — | An agent's correct claim waits for a human promotion to rank as curated. |
| Keep every direct write curated | Poisoning | One injected instruction lands a claim that leads recall. |
| Trust a claim agreed by several sources | Volume | An attacker controlling volume supplies the agreement. |
| Refuse agent direct writes | Reach | Agents lose the write path memory exists to serve. |

Consequences: the agent member is asserted, never verified, so a principal-only credential an agent presents still stamps curated.
Revisit: a verified binding between the agent member and the writer.

## Memory access follows evidence grants, and forgetting is a separate privilege

`read.recall` serves a claim only when every evidence row resolves through the caller's enforced session; otherwise the claim is suppressed with `MemoryEvidenceUnresolved`, without failing the recall. A claim serves at most 256 evidence entries and counts the rest in `evidence_omitted`; every entry, served or omitted, resolves, and a claim naming more than 4096 entries is suppressed with `MemoryEvidenceOverflow`. `disclosure.erase` sits outside the default grant set and raises `ErasureUngranted` without the forget grant; tombstones and cascade markers commit in one local snapshot before the verb returns.

| Option | Lost on | Cost |
| --- | --- | --- |
| Resolution of every entry, 256 served, 4096 gate bound, `evidence_omitted` count, separate forget grant, same-commit markers *(chosen)* | — | A claim whose source table is renamed goes silent; erasure needs a second credential and a heavier commit. |
| Suppress past 256 | Recall | A well-supported conclusion goes silent. |
| Serve the claim with an unresolved-evidence marker | Containment | The marker discloses the conclusion the gate holds. |
| Resolve evidence once at write time and cache it | Revocation latency | Later masks, grants and erasures never reach the cached verdict. |
| Forget inside the write grant, markers on the next ordinary commit | Blast radius and marker durability | A routine synthesis credential erases curated claims; a replica pulling in between resurrects forgotten rows. |

Consequences: revocation takes effect on the next recall with no invalidation sweep; recall cost of the evidence join is unmeasured.
Revisit: recall latency dominated by the evidence join at 4096; a suppression channel that names a withheld claim without its content; evidence lists clustering at the 256 cap.

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

The source's emitted confidence is uncalibrated until a per-shape-and-predicate isotonic map improves held-out Brier score on settled outcomes. Reports expose sample count, Brier score, expected calibration error and that held-out score. Recall fits the maps on the claims table's declared outcome table, read through the caller's session, and labels each confidence. A claims table may declare a half-life that scales its ranked scores; recorded claims keep their validity until expiry or erasure. A stale citation stays gated, and a newly committed source row is the only automatic synthesis trigger. Equal-instant unscoped contradictions enter the dead-letter table without silently retiring either claim. An earlier direct observation keeps the existing out-of-order refusal.

| Option | Lost on | Cost |
| --- | --- | --- |
| Validated calibration, hard history and explicit decay *(chosen)* | — | Sparse shape-and-predicate groups remain uncalibrated; conflicts need resolution. |
| Treat model confidence as comparable | Calibration | Two model outputs with the same number can have different outcome frequencies. |
| Decay validity intervals by default | History | A later rank policy erases what the workspace once concluded. |
| Resynthesize each stale read | Read latency | A query can launch an unbounded model job and change the source cursor. |

## Memory reads expose use and resolve ownership from enforced rows

A memory read's audit entry records the claim ids it returned under the caller's credential and subject. Ownership answers include every visible attached principal, ordered by attachment time and id. An external graph engine is a derived index; the store's enforced rows remain the answer source.

| Option | Lost on | Cost |
| --- | --- | --- |
| Ledger use, complete ownership and store-backed answers *(chosen)* | — | Claim-id logs consume storage; ownership answers can grow with attachments. |
| Infer usage from citations | Observation | A claim can be read many times without another claim citing it. |
| Return the newest owner only | Completeness | Joint ownership silently loses every older live attachment. |
| Serve directly from a graph backend | Enforcement | Its grant, mask, zone and frontier rules become a second implementation. |

## The as-of join matches on availability time, not valid time

`read.as-of-join` matches a driving row to the right relation's latest row whose declared availability instant precedes the driving instant. Valid time says when a value held; availability time says when anyone could see it. A restated figure keeps its original valid time, and a renamed identifier's new ticker is announced before it takes effect, so a valid-time match hands a backtest a value published after its decision. The join runs inside one statement over registered relations, so restriction, masks and the three bounds compose as they do for any read; a right relation declaring no availability time refuses rather than falling back.

| Option | Lost on | Cost |
| --- | --- | --- |
| Match on the declared availability instant *(chosen)* | — | Every right relation declares `available_time`; an undeclared one refuses. |
| Match on valid time | Leakage | A restatement published after the event joins as if known then. |
| Match on the transaction stamp | Backfill | A history landed in one commit joins nothing before that commit. |
| A dedicated join tool | Composition | The tool restates each relation's restriction outside the statement guard. |

## A horizon withholds load stamps and keeps citation stamps

Under a horizon `read.horizon.engine-stamps` drops `_ingested_at` and `_commit_seq` from the projection: each dates a row's load, and a 2026 backfill read under a 2014 horizon would reveal the present. `_run_id` and `_row_seq` stay, because `context.reference` cites a row by them and neither orders rows in time across runs.

| Option | Lost on | Cost |
| --- | --- | --- |
| Drop load stamps, keep citation stamps *(chosen)* | — | A run identifier can still name its date when an operator spells it so. |
| Drop every engine stamp | Citation | A horizon read returns rows no reference can name. |
| Drop every stamp and add an opaque citation token | Simplicity | A second identity scheme beside the one the store already records. |
