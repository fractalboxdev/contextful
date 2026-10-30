# A-read — Reading and memory decisions

**Status:** accepted

## read-surface-admits — The read surface admits one read-only statement over registered relations

The guard walks the syntax tree the executor itself serializes, so what it admits is what runs. A relation allowlist, not a verb blocklist, decides admission.

| Option | Lost on | Cost |
| --- | --- | --- |
| Executor's own tree plus a relation allowlist *(chosen)* | — | Each statement serializes once before execution; a harmless unregistered relation still refuses. |
| Token or regex blocklist over the text | Completeness | Every new engine verb or dialect spelling is a bypass until listed. |
| An independent SQL parser | Parser agreement | What the guard admits is not what the executor runs. |

## pooled-engine-reuses — A pooled engine reuses a resolved session only while its whole key holds

Each uncached call walks `tables/`, reads every granted schema and manifest, and builds one view per relation. The pool sits behind the guard, masks and zone gate. A `contextful-context` benchmark admits it: setup above 25% of warm p95 at 50 unfolded runs.

| Option | Lost on | Cost |
| --- | --- | --- |
| Whole-key pool inside `Face`, benchmark-gated *(chosen)* | — | Key computation still lists ledgers and schemas; connections hold memory per authority. |
| Key on the pointer set alone | Freshness | An appended run, schema edit or new table serves a stale view set. |
| A time-to-live on sessions | Invalidation by construction | A revoked token reads until expiry. |

Criteria: no reuse crosses a snapshot, schema, table set or revocation; then warm latency.

## result-reused-under — A result is reused only under the read frontier it was computed at

A dashboard issues one statement per panel over one frontier, and each repeat re-executes. The cache sits behind the guard, masks and zone gate, and keys on the read frontier of each touched relation.

| Option | Lost on | Cost |
| --- | --- | --- |
| Key on touched relations and their read frontier *(chosen)* | — | A hit still admits the statement and stamps ledger files; a truncated result caches as truncated. |
| Key on resolved snapshot ids | Freshness | A run committed before the next fold changes the result under the same ids. |
| A time to live as the staleness guard | Invalidation by construction | Every commit reads stale until expiry. |

## restricted-sidecar-probe — A probe blind to restriction doubles for a bounded number of rounds and reports under-fill

Rows a reader cannot see can rank above its own, so one oversampled probe under-fills the limit. A full-text probe's postings carry no row predicate, and an unfiltered vector probe can name rows its relation no longer yields. Bounded work comes first, then recall, then an honest count.

| Option | Lost on | Cost |
| --- | --- | --- |
| Doubling for 4 rounds, under-fill reported *(chosen)* | — | A reader whose visible rows sit past the last round receives fewer rows than the limit, named as such. |
| One fixed oversampling factor | Recall | A clustered restriction under-fills silently. |
| Probe until the limit fills | Bounded work | A reader seeing few rows walks the whole graph. |

Revisit: under-fill reported on more than a small share of restricted reads.

## filtered-vector-traversal — A filtered vector probe walks the graph admitting matching rows, and ranks a narrow match exactly

A filter or restriction can hide most of a graph near the query. A pre-scan of the reader's relation under the filter yields the identifiers the probe may return, so enforcement stays in the relation. The walk skips refused nodes as results and still expands their neighbours. An exact probe reads every node's identifier once, while the walk lengthens as the match narrows, so the crossover follows the matching count.

| Option | Lost on | Cost |
| --- | --- | --- |
| Traversal over pre-scanned identifiers, exact under a matching count *(chosen)* | — | Every filtered probe reads its relation's identifier column once. |
| Doubling over-fetch | Recall | A selective filter returns short after 4 rounds. |
| Exact distance for every filter | Latency | A broad filter over a large graph scores every matching row. |
| Filter columns copied into the sidecar | Enforcement | A copied column bypasses masks and row policy on disk. |

Revisit: a pre-scan exceeding the probe's own cost on a large relation.

## reciprocal-rank-calibrates — Reciprocal rank calibrates the lexical leg, admitted by the native evaluation

Min-max over the window let one outlier compress every other BM25 score. Reciprocal rank, `61 / (60 + r)`, met the gate against min-max and raised the hybrid leg's nDCG and R-precision.

| Option | Lost on | Cost |
| --- | --- | --- |
| Reciprocal rank, admitted by the evaluation *(chosen)* | — | Score magnitudes stop informing order; the comparison runs the native set twice. |
| Keep min-max normalization | Calibration | One outlier score flattens the lexical leg in every window. |
| Swap without a gate | Measured quality | A ranking change ships without evidence it ranks no worse. |

Revisit: a corpus where reciprocal rank loses a tracked measure to another calibration.

## reranker-reorders-masked — A reranker reorders a masked post-authority window

A cross-encoder orders the leading rows better than fused order, at one model call per candidate. `read.rank` reranks only on a call naming `rerank_with`, over the first 50 masked rows after authority, pointwise. A reranker enters the declared set through the native-evaluation gate; a remote one is egress through `connector.infer.model-endpoint`.

| Option | Lost on | Cost |
| --- | --- | --- |
| Pointwise rerank of a masked 50-row window, on request *(chosen)* | — | A reranked read costs up to 50 model scores and depends on the reranker's build. |
| Listwise reranking in one prompt | Injection resistance | One poisoned candidate can instruct the model to promote itself. |
| Rerank before authority | Enforcement | Hidden rows reach a model, and their scores shape what the caller sees. |

Revisit: the native evaluation measuring no gain for a declared reranker over fused order.

## zone-withheld-relation — A zone-withheld relation is named, with one whole-relation count

The count is a property of the relation, so no ranked position, filter or requested size moves it.

| Option | Lost on | Cost |
| --- | --- | --- |
| Name plus a whole-relation count *(chosen)* | — | The count discloses how many in-scope rows a placement withholds; each withheld relation costs one count query per read. |
| Empty result, nothing named | Distinguishability | A zone-emptied relation reads as a table holding no data. |
| Count over the caller's statement | Oracle resistance | Varying a predicate turns the count into a probe of withheld values. |

## networked-read-transport — The networked read transport is MCP Streamable HTTP, admitted per request

The stdio tool server answers only its spawning process, under one startup credential. MCP-client compatibility decided the shape; revocation latency decided per-request admission.

| Option | Lost on | Cost |
| --- | --- | --- |
| MCP Streamable HTTP around the tool server *(chosen)* | — | A tool result reaches a caller inside a JSON-RPC envelope; the face holds no session and no server stream. |
| One HTTP route per read tool | MCP-client compatibility | An MCP client needs its own adapter, and the tool set gains a second envelope. |
| Admission once per connection or session | Revocation latency | A revoked credential reads until its connection closes. |

Consequences: an MCP client that signs nothing reads with a short-lived audience-bound bearer and re-exchanges it before expiry.
Revisit: a caller needs server-initiated messages or resumable streams.

## served-claim-write — A served claim write stays outside read MCP

A hosted console distils claims after a read turn and owns no local store root. The route keeps the read tool set closed and reuses memory's commit and evidence gates. The browser holds no writer credential.

| Option | Lost on | Cost |
| --- | --- | --- |
| Separate route and direct memory write *(chosen)* | — | The hosted console needs a writing credential whose task names its reading session. |
| Add `memory.write` to read MCP | Closed read tool set | A client-reachable tool can mutate a store. |
| Spawn the CLI from the console | Hosted reach | The console needs the store's local project root and signing configuration. |

## memory-writes-validate — Memory writes validate or dead-letter, and outcomes settle under their source

A rejected candidate waits in the dead-letter table for a human rather than becoming an untraceable row. Metric comparators evaluate outside the engine.

| Option | Lost on | Cost |
| --- | --- | --- |
| Validate, dead-letter, settle under the registered source *(chosen)* | — | Dead-lettered candidates wait for a human; extraction spends up to 3 model calls per batch. |
| Resolve an ambiguous mention to the highest score | Reversibility | A wrong merge propagates through every claim about both entities. |
| Evaluate metric rules with an embedded expression engine | Attack surface | Anyone registering a prediction reaches the evaluator. |

## gleaning-declared-single — Gleaning is a declared, single extra pass

A second prompt asking what the first pass missed recovers some supported claims. `read.declare` admits `gleanings` of 0 or 1, off by default; a gleaned claim carries `_pass = 'glean'` and commits with the batch. Bounded model spend comes first, then recall.

| Option | Lost on | Cost |
| --- | --- | --- |
| Declared, off by default, one pass *(chosen)* | — | A table opting in spends one more model call per batch. |
| Always on | Model spend | Every deployment pays a second call per batch whether its sources need one or not. |
| Unbounded passes until no new claim | Bounded work | A model that keeps emitting claims holds the cursor and spends without limit. |

Consequences: a failed gleaning discards only gleaned claims and never holds the cursor.
Revisit: gleaned claims superseded or contradicted at a rate above first-pass claims.

## extraction-cache-keys — An extraction cache keys responses on marked bytes, template and model

Re-running synthesis over unchanged rows repeats model calls whose answer is known. Keying on fenced bytes keeps two credentials with different masks apart. The cache holds model responses, never evidence verdicts.

| Option | Lost on | Cost |
| --- | --- | --- |
| Enforced table keyed on marked bytes, template and model *(chosen)* | — | Entries occupy store space, and each names its batch's rows for an erasure to reach it. |
| Key on source row identity | Containment | Two sessions masking one row differently share a response one of them never saw. |
| A side file outside the store | Enforcement | Its bytes escape grants, masks, replication rules and erasure. |

## claim-standing-derived — Claim standing is one derived tier, and revision is scoped to a validity line

Recall orders by tier, then score. A lower-standing contradiction gets a bounded validity end instead of retiring the prior. Promotion is a human act. Only `researched` claims expire.

| Option | Lost on | Cost |
| --- | --- | --- |
| One derived tier with lexicographic precedence *(chosen)* | — | A high-scoring researched claim ranks below a weak curated one. |
| A caller-supplied tier | Forgery | Any writer promotes its own output. |
| Retire on subject, predicate and scope alone | History | A past-anchored belief retires the present one. |

## agent-third-party — Agent and third-party writes stand derived

Recall puts curated claims first, so a curated agent write lets injected text become a top-ranked belief. `read.revise` stamps a direct write `derived` when its credential carries an agent member or a cited row reads `ingested:third-party`.

| Option | Lost on | Cost |
| --- | --- | --- |
| Agent and third-party writes stand derived *(chosen)* | — | An agent's correct claim waits for a human promotion to rank as curated. |
| Keep every direct write curated | Poisoning | One injected instruction commits a claim that leads recall. |
| Trust a claim agreed by several sources | Volume | An attacker controlling volume supplies the agreement. |

Consequences: the agent member is asserted, never verified, so a principal-only credential an agent presents still stamps curated.
Revisit: a verified binding between the agent member and the writer.

## memory-access-follows — Memory access follows evidence grants, and forgetting is a separate privilege

Every evidence entry resolves at recall, so revocation takes effect on the next recall with no invalidation sweep.

| Option | Lost on | Cost |
| --- | --- | --- |
| Resolve every entry at recall; separate forget grant *(chosen)* | — | A claim whose source table is renamed goes silent; erasure needs a second credential. |
| Resolve evidence once at write time and cache it | Revocation latency | Later masks, grants and erasures never reach the cached verdict. |
| Forget inside the write grant | Blast radius | A routine synthesis credential erases curated claims. |

Revisit: recall latency dominated by the evidence join at 4096 entries; evidence lists clustering at the 256 cap.

## citation-keyed-table — A citation into a keyed table resolves through a pepper-keyed digest of its key

A fold drops superseded versions, so a citation by run and sequence alone withdraws its claim at the next compaction. No caller holds the secret to test a guessed key.

| Option | Lost on | Cost |
| --- | --- | --- |
| Pepper-keyed digest of the key, stale claims counted *(chosen)* | — | A stale claim may contradict its key's live version; a pepper rotation returns citations to run-and-sequence resolution. |
| Key cells copied into the citation | Containment | Every memory reader reads source key values outside the source table's grants, masks and erasure. |
| Withdraw the claim at the fold | Recall stability | A routine compaction silently forgets memory. |

Revisit: stale counts dominating a recall; a reader needing the claim's cited version rather than its key's live one.

## store-holds-nothing — The store holds nothing a user could not see

Query-time enforcement bounds a read, not the stored bytes, so the ceiling sits at the write path.

| Option | Lost on | Cost |
| --- | --- | --- |
| Refuse at the write boundary; refuse twin APIs everywhere *(chosen)* | — | A security corpus needs an explicit exemption; full coverage means per-user ingestion. |
| Ingest through the twin API and restrict at query time | The ceiling | No query-time restriction lowers what the store contains. |
| Permit the twin API behind an organization-signed disclosure | Consent of the people ingested | The organization's signature is not their consent. |

## nearness-parent-closure — Nearness is a parent closure over declared edges, and the engine holds no geospatial type

`read.resolve-entity` answers nearness as the transitive closure over declared parent edges: near a place means at it or under it. The closure left-joins as an ordinary dimension, so an unmapped place tags null.

| Option | Lost on | Cost |
| --- | --- | --- |
| Parent closure as an ordinary dimension *(chosen)* | — | A radius question is unanswerable; hierarchy gaps show as null tags. |
| A geospatial column type with radius search | Scope | A type system and index family enter for a workload that needs none. |
| Coordinate pairs with ad-hoc distance in SQL | Correctness | Raw latitude-longitude distance errs by a latitude-dependent margin. |

Revisit: a deployment declares artificial parent levels to approximate a radius; closure materialization dominates the dimension's build.

## statement-owns-deadline — Each statement owns its deadline and serialized response budget

Interrupting one connection leaves every other statement running. A first row that cannot fit refuses rather than looking empty.

| Option | Lost on | Cost |
| --- | --- | --- |
| Per-connection interrupt and serialized row probes *(chosen)* | — | Each bounded statement needs a deadline watcher; byte-bounded reads serialize candidate rows. |
| Kill the serving process at a deadline | Isolation | Other statements and pooled sessions die with the timed-out one. |
| Measure elapsed time and response bytes after execution | Resource ceiling | Expensive work and oversized responses complete before a refusal. |

## memory-vocabulary-schedules — Memory vocabulary and schedules are explicit

The five reserved relations assign no deployment-specific meaning. A schedule never alters the pass cursor.

| Option | Lost on | Cost |
| --- | --- | --- |
| Small core, declared extensions and operator-triggered default *(chosen)* | — | A deployment maintains migrations and schedules for its own relation types. |
| Open-ended relation strings | Validation | A typo becomes an edge type no other consumer recognizes. |
| Automatic pass on every commit | Cost | A bulk load launches repeated extraction before its source is complete. |

## memory-confidence-retention — Memory confidence and retention separate history from ranking

Decay changes ranking only; validity is history and ends only by expiry or erasure.

| Option | Lost on | Cost |
| --- | --- | --- |
| Validated calibration, hard history and explicit decay *(chosen)* | — | Sparse shape-and-predicate groups remain uncalibrated. |
| Treat model confidence as comparable | Calibration | Two outputs with the same number can have different outcome frequencies. |
| Decay validity intervals by default | History | A later rank policy erases what the workspace once concluded. |

## memory-reads-expose — Memory reads expose use and resolve ownership from enforced rows

A graph engine is a derived index; enforced store rows answer.

| Option | Lost on | Cost |
| --- | --- | --- |
| Ledger use, complete ownership and store-backed answers *(chosen)* | — | Claim-id logs consume storage; ownership answers grow with attachments. |
| Infer usage from citations | Observation | A claim can be read many times without another claim citing it. |
| Serve directly from a graph backend | Enforcement | Its grant, mask, zone and frontier rules become a second implementation. |

## traversal-walks-enforced — A traversal walks enforced edges under fixed bounds

An unreadable edge is never crossed, because the node beyond it discloses the edge. Only walkable relations are followed, so a dense relation such as `about` never fans a walk across the store.

| Option | Lost on | Cost |
| --- | --- | --- |
| Bounded breadth-first walk through the enforced session *(chosen)* | — | A dense neighbourhood returns incomplete; every walk resolves each reached edge's citations. |
| Unbounded walk cut by the byte ceiling | Resource ceiling | A hub entity expands the whole graph before the cut. |
| A caller-chosen hop count with no ceiling | Predictability | One call's cost follows the caller, not the declaration. |

Revisit: a graph arm seeded from the query's entities earns a place in fusion; truncation dominates native multi-hop cases.

## join-matches-availability — The as-of join matches on availability time, not valid time

A restated figure keeps its original valid time, so a valid-time match hands a backtest a value published after its decision. The join runs inside one statement, so restriction, masks and bounds compose as for any read.

| Option | Lost on | Cost |
| --- | --- | --- |
| Match on the declared availability instant *(chosen)* | — | Every right relation declares `available_time`; an undeclared one refuses. |
| Match on valid time | Leakage | A restatement published after the event joins as if known then. |
| Match on the transaction stamp | Backfill | A history written in one commit joins nothing before that commit. |

## sidecar-serves-past — A sidecar serves a past horizon by filtered traversal

A sweep over many past decision dates reads one sidecar rather than refusing or scanning the table once per date. The postings' availability timeline yields horizon statistics in one binary search.

| Option | Lost on | Cost |
| --- | --- | --- |
| Filter by per-entry availability *(chosen)* | — | Eight bytes per entry; a horizon admitting few entries walks most of the graph. |
| Refuse a sidecar indexing later rows | Recall | Every past horizon over a growing table refuses its ranking. |
| Fall back to the exact scan | Latency | A sweep of N dates scans the table N times. |
