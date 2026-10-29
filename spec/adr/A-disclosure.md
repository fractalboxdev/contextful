# A-disclosure — Visibility, disclosure and accountability decisions

**Status:** accepted

## mirrored-authorization-carries — Mirrored authorization carries a freshness budget and refuses past it

A mirrored grant is only as current as the oldest resource its sweep observed. The narrowing posture `on_stale = public_only` serves only swept public rows, marked degraded.

| Option | Lost on | Cost |
| --- | --- | --- |
| Coverage watermark, per-table budget, typed refusal *(chosen)* | — | A limping sweep takes a source offline; the public-status sweep is a second shape to operate. |
| Serve past the budget with an advisory field | Detectability | A revoked grant keeps answering, marked by a field nobody reads. |
| Zero rows past the budget, or a truncated closure | Detectability | A shortened set reads as a quiet corpus. |
| A time-to-live on the reachable-set cache | Invalidation by construction | A revoked grant answers for an undeclared second staleness window. |

Revisit: a source offers sequenced delivery with gap reconciliation.

## fidelity-bounded-what — Fidelity is bounded by what the source enforces

The family is the one judgment per source. A source that computes access answers live under the reader's delegated credential; a source with no permission data lands with no fidelity claim.

| Option | Lost on | Cost |
| --- | --- | --- |
| Per-table level bounded by a declared family *(chosen)* | — | Federated legs pay live latency and go dark with the source. |
| One deployment-level permission-aware claim | Falsifiability | The weakest source sets the real guarantee undetected. |
| Mirror a sharing engine's inputs and reproduce its rules | Drift | A second evaluator disagrees in exactly the restricting constructs. |
| An audience field in the pack | Enforceability | No read-path step consults it. |

## grants-rows-under — Grants are rows under `_visibility`, and the closure is a rebuilt projection

Authorization has one source, the join over observed rows, and a replica answers offline.

| Option | Lost on | Cost |
| --- | --- | --- |
| Grant rows via the land path, closure in `derived.sqlite` *(chosen)* | — | A replica rebuilds the closure after each pull. |
| A separate grant store beside the catalog | One write path | A second commit protocol, lock and erasure path. |
| A closure synced as files | One authorization source | Closure bytes on a replica drift from their rows. |
| Openings as manifest policy | Enforceability | A key no sweep observes answers without expiry or record. |

## aggregate-disclosure-enforced — Aggregate disclosure is enforced at build with noisy thresholding and an up-front budget reservation

Every read path inherits the guarantee from the staged bytes.

| Option | Lost on | Cost |
| --- | --- | --- |
| Build-time enforcement, reservation up front *(chosen)* | — | A policy edit is a rebuild; an exhausted unit blocks every release naming it. |
| Suppress on exact counts, or a switch to exact figures | Privacy guarantee | Cell presence discloses a threshold crossing; a policy edit removes the protection. |
| Debit spend after the release | Concurrency | Two concurrent releases each spend budget the other observes. |
| A query-time aggregate evaluator | Survival across read paths | Reimplemented per surface; a rollup loses per-contributor mass. |

Revisit: lookalike segment release settles its audience floor, readback rule and cross-party consent.

## exact-integer-noise — Exact integer noise spends the declared budget across released statistics

An auditable privacy spend needs finite sensitivity and a sampler with no floating-point tails.

| Option | Lost on | Cost |
| --- | --- | --- |
| Exact geometric noise, bounded metrics, no lookalike release *(chosen)* | — | Every noised sum needs bounds; an activation product has no release path. |
| Floating-point Laplace noise over raw sums | Auditable sensitivity | The privacy spend is unverifiable. |
| Exact grouped figures behind a noisy threshold | Figure privacy | A published sum reveals contributors even with the key hidden. |
| Lookalike release beside aggregate figures | Consent | An activation handle reaches a party under no consent rule. |

Revisit: a consent and readback contract admits activation handles.

## model-declares-spend — The model declares the spend, the project the cap, and the ledger stays on the machine

One cap per unit decided it. No spend is undeclared, and no contributor key rests outside the store.

| Option | Lost on | Cost |
| --- | --- | --- |
| Spend on the model, cap on the project, local ledger *(chosen)* | — | Two machines releasing one project each hold a full cap. |
| A cap per model | One cap per unit | A unit named by two models spends two lifetimes. |
| A default spend when none is declared | Author intent | Every unit pays a budget nobody chose. |
| The ledger inside the store root | Replica safety | A push carries per-contributor balances to every replica. |

Revisit: a project releases from more than one machine.

## cross-owner-release — A cross-owner release runs only when the boundary is enforced below the engine

A protection the engine only documents fails silently, as corrupted data or a forged grant.

| Option | Lost on | Cost |
| --- | --- | --- |
| Store-enforced preconditions, per-pair pepper *(chosen)* | — | Stores without per-prefix write policy are ruled out; pepper escrow is operational work. |
| Protections as operator responsibilities | Detectability | Failure arrives silently. |
| Two of three preconditions | Coverage | The third path stands open. |
| A static pepper shared across joins | Pepper compromise | One recovered pepper reverses every join. |

Revisit: private set intersection replaces the pepper; an attested enclave join serves very-high-risk pairings.

## hash-chain-attestable — The hash chain is the attestable record

Audit storage becomes an availability dependency of reads. A compromised node cannot truncate history its replicated roots cover; telemetry is a projection with no durability obligation.

| Option | Lost on | Cost |
| --- | --- | --- |
| Linked chain under group commit, signed segment roots *(chosen)* | — | A read fails when its entry cannot persist. |
| Telemetry spans as the record | Detectability | A dropped span reads as a quiet period. |
| Fail open with a gap marker, or buffer and retry | Evidential completeness | Rows leave before their entry exists. |
| An external transparency log as primary | Air-gapped operability | Some deployments cannot reach a third party. |

Revisit: audit writes replicate to a second store; group commit sustains less than the reference read rate.

## append-group-pays — An append group pays one sync, and the tip signs at segment close

A per-append sync of segment, tip and directory, plus a signature, costs three full syncs (`F_FULLFSYNC` on macOS) per read. Syncs per read decided it; throughput then scales with concurrency.

| Option | Lost on | Cost |
| --- | --- | --- |
| One sync per group, tip at close, idle and export *(chosen)* | — | One failed sync refuses the whole group; the unsigned tail truncates undetected. |
| Three syncs and a signature per append | Syncs per read | Latency grows with every durable write the tip needs. |
| Sync and sign the tip per group | Syncs per read | Three syncs protect a tail no replicated root covers yet. |

## audit-format-v1 — Audit format v1 is versioned, whole-entry canonical and Merkle-rooted

A v0 entry proves membership only with its whole segment, and its header fields sit outside the digest. Offline per-entry evidence decided it; every released format verifies forever.

| Option | Lost on | Cost |
| --- | --- | --- |
| Versioned whole-entry digest, Merkle root *(chosen)* | — | Two verifier paths kept forever; attribute integers within ±(2^53 − 1). |
| Keep v0 | Per-entry evidence | Membership needs the whole segment. |
| One digest for every chain | Embedder fit | A verifier pinned to the other digest cannot read the chain. |
| Sign every entry | Syncs per read | One signature per read, which group commit amortizes away. |

## audit-log-opens — An audit log opens under the key custody its caller holds

A read path holding no issuer key still appends, and a verifier needs no writer lock. Tamper evidence decided it: a signed `chain.held` marks a chain held for good.

| Option | Lost on | Cost |
| --- | --- | --- |
| Held, unanchored or read-only, explicit anchoring *(chosen)* | — | An unanchored chain carries no signature until its owner anchors it. |
| A key for every append | Availability | A read path without issuer custody cannot serve. |
| Anchor silently at the first held open | Tamper evidence | A stripped, rewritten chain reads as unanchored and the next open signs it. |

## append-group-holds — One append group holds the log at a time

`serve --http` and `mcp` run as separate processes on one project, and both append to its one linear chain.

| Option | Lost on | Cost |
| --- | --- | --- |
| Directory lock per append group *(chosen)* | — | Groups across processes serialize at one sync at a time. |
| Directory lock for a process's lifetime | Concurrent read processes | A second read process cannot start. |
| One chain per process | Truncation detectability | A deleted chain leaves no gap in any other. |
| A daemon owning the log | Scope | A third long-lived process and its transport to operate. |

## audit-reads-view — `audit_reads` is a view over the chain, and a refused read appends

One record decided it, and a refusal is typed and recorded (P2). A lookup over 24 h answers within 1 s.

| Option | Lost on | Cost |
| --- | --- | --- |
| A view computed per query from the segments *(chosen)* | — | Query cost scales with the chain's length; probes grow the chain. |
| A reserved table beside the chain | One record | A second state model drifts and needs its own erasure. |
| A projection rebuilt on append | One record | A crash between two writes leaves them disagreeing. |
| A refused read leaves no entry | Typed refusals | A probe of ungranted tables stays invisible. |

## explanation-reads-exchange — An explanation reads the exchange policy and the chain, and keeps no store

An explanation carries no row (P5) and qualifies every negative (P2).

| Option | Lost on | Cost |
| --- | --- | --- |
| Current grants decide, the chain replays *(chosen)* | — | A decision states the grants in force at the call, not those a past read held. |
| A store snapshotting grants per read | One record | A second state drifts from the chain. |
| Replay the read itself to decide | No rows | Evaluating the relation touches rows. |

## erasure-forced-rewrite — Erasure is a forced rewrite, a bounded cascade and a measured receipt

One authenticated frontier spans every affected table, and readers and writers fence against it so an erased row never returns through an older batch. A failed audit publication leaves the original frontier selected.

| Option | Lost on | Cost |
| --- | --- | --- |
| One authenticated publication record with release fencing *(chosen)* | — | Every reader and writer validates the frontier; collection rewrites retained artifacts. |
| Independent table pointer commits | Atomicity | A reader observes partial cross-table erasure. |
| Response-time tombstone filters alone | Physical removal | Retained files and sidecars preserve erased bytes. |

## forced-rewrite-measured — Forced rewrite and measured receipt

A purge deletes no bucket object, so the receipt claims rewrite-and-exclude over the canonical store. Replicas and the replication bucket sit outside the claim.

| Option | Lost on | Cost |
| --- | --- | --- |
| Forced rewrite, bounded closure, measured receipt *(chosen)* | — | Cost scales with the table; fact reads go down until re-synthesis. |
| A one-hop cascade | Completeness | A fact derived from a derived fact outlives its evidence. |
| An unbounded transitive walk | Termination | Provenance is never proved acyclic. |
| Crypto-shredding a per-subject key | Fit with the substrate | Every write path holds a per-subject key forever. |
| A broad "data destroyed" receipt | Assertability | The claim is false when signed. |

Revisit: a purge deletes bucket objects; 72 h request-to-last-replica erasure becomes an engine bound.

## re-synthesis-after — Re-synthesis after erasure replays the extraction cache

An erased row reaches derived state through its claims, its batch's extraction-cache entry and any dead-letter row. A batch with no erased row replays without a model call only at its original boundaries under an unchanged template, model and cutoffs.

| Option | Lost on | Cost |
| --- | --- | --- |
| Collect affected entries; replay untouched batches *(chosen)* | — | Replay needs each batch's recorded membership, else restaging re-extracts. |
| Flush the whole cache on any erasure | Cost | Every restaging re-extracts the full table. |
| Leave entries to their own retention | Physical removal | A response quoting the erased row outlives the deadline. |
| Restage under the erasing credential | Attribution | Restored claims carry the forget grant. |

## drive-permission-coverage — Drive permission coverage includes every selected file

A refused permission page does not prove that a file's prior grants remain complete.

| Option | Lost on | Cost |
| --- | --- | --- |
| Complete per-file permission reads *(chosen)* | — | A refused file stalls coverage and can close reads past the budget. |
| Treat a refused file as grantless | Truthfulness | Empty grants conceal an unread resource. |
| Advance after the first permission page | Revocation coverage | Later pages can hold the revoked principal. |
