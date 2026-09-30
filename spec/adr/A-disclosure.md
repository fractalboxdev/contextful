# A-disclosure — Visibility, disclosure and accountability decisions

**Status:** accepted

## Mirrored authorization carries a freshness budget and refuses past it

`disclosure.sweep` advances `watermark_at` only on a run that read every governed resource, or from an event stream declared gap-detectable. `disclosure.bound-staleness` refuses a read past `max_acl_staleness` with `VisibilityAccessStale` (HTTP 503); `on_stale = public_only` serves only swept public rows, marked degraded. `disclosure.reach` raises `VisibilityClosureDepth` (HTTP 422) at `max_group_depth` or 10000 nodes rather than truncating; the reachable-set cache keys on subject, source epoch and directory epoch.

| Option | Lost on | Cost |
| --- | --- | --- |
| Coverage watermark, per-table budget, typed refusal, swept public-status narrowing *(chosen)* | — | A limping sweep takes a source offline; an expensive full run makes its budget hours; the public-status sweep is a second sweep shape to operate. |
| Serve past the budget with an advisory freshness field | Detectability | A revoked grant keeps answering, marked by a field nobody must read. |
| Return zero rows past the budget, or truncate the closure | Detectability | A shortened set reads as a quiet corpus indefinitely. |
| Newest observation anywhere sets the watermark | Worst-case coverage | One fresh resource vouches for grants observed days ago. |
| A time-to-live on the reachable-set cache | Invalidation by construction | A revoked grant answers for an undeclared second staleness window. |

Consequences: a revocation rotates the cache key; a sweep of one source rotates keys for tables sharing its epoch; a budget tighter than the sweep cadence fails at diagnose.
Revisit: a source offers sequenced delivery with gap reconciliation.

## Fidelity is bounded by what the source enforces

`disclosure.declare-fidelity` sets one level per table — `mirrored`, `coarse`, `federated`, `excluded` — bounded by the declared `family` at load (`VisibilityFamilyBound`). A source that computes access is queried live under the reader's delegated credential; mirroring its inputs raises `VisibilityComputedInputsMirrored`. `disclosure.pack` refuses an access-shaped key the mapping schema lacks with `VisibilityPackAssertsAccess`. A source with no permission data lands with no fidelity claim.

| Option | Lost on | Cost |
| --- | --- | --- |
| Per-table level, bounded by a declared family, enforced at load *(chosen)* | — | Every answer carries qualification; federated legs pay live latency and go dark with the source; broad faces lose sources with no permission endpoint. |
| One deployment-level permission-aware claim | Falsifiability | The weakest source sets the real guarantee with nothing detecting it. |
| Reviewer judgment on each level declaration | Failure visibility | A mistake surfaces as a disclosure months later. |
| Mirror a sharing engine's inputs and reproduce its rules | Drift | A second evaluator disagrees in exactly the restricting constructs. |
| An audience field in the pack for unmirrorable sources | Enforceability | No read-path step consults it. |

Consequences: one judgment — the family — per source; authorization has one source, the join over observed rows.

## Aggregate disclosure is enforced at build with noisy thresholding and an up-front budget reservation

`disclosure.release` stages no breaching cell: `disclosure.suppress` selects partitions by a noisy threshold over distinct-contributor counts, and a published figure carries noise whose strength follows the declared per-run spend. `disclosure.bound-cohort` admits exact figures from a `cohort` table only; a reader entitled to every contributing row reads them as an ordinary read. A release reserves per-unit spend for every contributing unit in one catalog transaction before reading, refusing with `DisclosureUnitBudgetExhausted`. `max_contributor_share` withholds a concentrated group. A table is `per-person` or `cohort`; `DisclosureCohortWidening` and `DisclosureSingletonCohort` guard cohorts.

| Option | Lost on | Cost |
| --- | --- | --- |
| Build-time enforcement, noisy partition selection, exact figures from cohort tables only, reservation up front *(chosen)* | — | A policy edit is a rebuild; per-person figures carry noise; an exhausted unit blocks every release naming it. |
| Suppress on exact counts, or a policy switch to exact figures | Privacy guarantee | Cell presence discloses whether a contributor crossed the threshold; whoever edits a policy removes the protection. |
| Debit spend after the release | Concurrency | Two concurrent releases each observe budget the other spends. |
| Skip exhausted units silently | Truthfulness | A result reads as complete while its shortfall reveals which units spent their budget. |
| A query-time aggregate evaluator | Survival across read paths | Reimplemented per surface; cannot recover per-contributor mass from a rollup. |

Consequences: every read path inherits the guarantee from the staged bytes; raising a unit's lifetime cap is an explicit grant edit.
Revisit: lookalike segment release settles its audience floor, readback rule and cross-party consent.

## A cross-owner release runs only when the boundary is enforced below the engine

`disclosure.set-mode` requires of a cross-owner store per-owner signed manifest subtrees, per-owner write prefixes enforced by the object store's access policy, and per-owner signing keys, else `DisclosureCleanRoomPreconditionUnmet`. A hashed join keys on a per-pair escrowed pepper rotated per join; a static pepper raises `DisclosureStaticPepper`.

| Option | Lost on | Cost |
| --- | --- | --- |
| Store-enforced preconditions, per-pair pepper *(chosen)* | — | Stores unable to express per-prefix writes are ruled out; pepper escrow is operational work. |
| Document the protections as operator responsibilities | Detectability | Failure arrives silently as corrupted data or a forged grant. |
| Accept two of three preconditions | Coverage | The third path stands fully open. |
| A static pepper shared across joins | Pepper compromise | One recovered pepper reverses every join, both directions. |

Consequences: a cross-owner join never reuses a pepper; whether segments release, and under which consent, stays open under `disclosure.release`.
Revisit: private set intersection replaces the pepper; an attested enclave join serves very-high-risk pairings.

## The hash chain is the attestable record

`disclosure.record` appends each read to a linked hash chain under group commit; rows return only after the covering fsync, else `AuditEntryUnpersisted`. The query digest is HMAC-SHA256 under the project audit key. A disagreeing digest, sequence gap, or absent chain beside a tip or root raises `AuditChainBroken`. `disclosure.attest` signs one root per 4096-entry segment and replicates roots off-node every 10 min. Telemetry is a projection with no durability obligation.

| Option | Lost on | Cost |
| --- | --- | --- |
| Linked chain under group commit, refuse on unpersisted entry, per-segment signed roots *(chosen)* | — | Audit storage is an availability dependency of reads; no field data on versions or errors. |
| Telemetry spans as the record | Detectability | A dropped span is indistinguishable from a quiet period. |
| Fail open with a gap marker, or buffer and retry | Evidential completeness | Rows leave before their entry exists. |
| An external transparency log as the primary record | Air-gapped operability | Some deployments cannot reach a third party. |
| A plain SHA-256 over the statement | Pseudonymity | A digest over a guessable statement reverses by dictionary. |

Consequences: a chain gap has one cause; a compromised node cannot truncate history replicated roots cover.
Revisit: audit writes replicate to a second store; group commit sustains less throughput than the reference read rate.

## An append group pays one sync, and the tip signs at segment close

**Status:** accepted

Context: an append that syncs its segment, a temporary tip file and the tip's directory, then signs the tip, pays three full syncs (`F_FULLFSYNC` on macOS) and one signature per read.
Decision: `disclosure.record.group-commit` holds the budget. Concurrent appends form a group; one data sync of the segment covers every member, which then releases its rows, or every member raises `AuditEntryUnpersisted`. Opening a segment adds one directory sync. The tip signs at segment close, on idle and at export.

| Option | Lost on | Cost |
| --- | --- | --- |
| One sync per group, tip at close, idle and export *(chosen)* | — | One failed sync refuses the whole group; the open segment's unsigned tail truncates undetected. |
| Three syncs and a signature per append | Syncs per read | Append latency grows with every durable write the tip needs. |
| Sync and sign the tip per group | Syncs per read | Three syncs per group protect a tail no replicated root covers yet. |
| Delay appends to fill larger groups | Lone-read latency | A single reader waits out the window. |

Criteria: rows never leave before their entry is durable, fixed; syncs per read decided it; truncation detectability.
Consequences: throughput scales with concurrency, not sync latency; the unsigned tail is bounded by segment close and idle. An implementation meets this decision once a test counts one sync per group and a p99 append benchmark on the reference target states its method and number.

## Audit format v1 is versioned, whole-entry canonical and Merkle-rooted

**Status:** accepted

Context: a v0 entry digests `seq`, `prev_hash` and its attributes, and a segment root is the last entry hash, so one entry proves membership only with its whole segment. Every released format verifies forever.
Decision: each v1 entry carries its format version, and its digest covers the whole entry under RFC 8785 canonical JSON. A chain header fixes the digest, SHA-256 or BLAKE3, and the segment size. A segment root is an RFC 6962 Merkle tree hash, so each entry has an inclusion proof. Roots sign through `SigningPort`, tagged by algorithm. A v0 chain verifies under v0 rules.

| Option | Lost on | Cost |
| --- | --- | --- |
| Versioned whole-entry digest, per-chain header, Merkle root *(chosen)* | — | Two verifier paths kept forever; canonical encoding per append; attribute integers within ±(2^53 − 1); proofs grow with segment size. |
| Keep v0 | Per-entry evidence | Membership needs the whole segment; header fields sit outside the digest. |
| One digest for every chain | Embedder fit | A verifier pinned to the other digest cannot read the chain. |
| Sign every entry | Syncs per read | One signature per read, which group commit amortizes away. |

Criteria: offline per-entry evidence decided it; every released version verifies; append cost; hardware-held keys.
Consequences: `disclosure.record.segment` and `disclosure.attest.broken-chain` restate over v1.

## An audit log opens under the key custody its caller holds

**Status:** accepted

Context: a read path holding no issuer key still appends, and a verifier needs no writer lock.
Decision: a log opens held, unanchored or read-only. Held signs roots and tip through `SigningPort`. Unanchored links entries under an unsigned tip, writes no root and refuses a chain already signed (`disclosure.record.unanchored-over-signed`). Read-only verifies without the lock and refuses appends. A held open over an unsigned tip refuses (`disclosure.record.unsigned-tip`); anchoring, an explicit act of the key holder, signs the missing roots and tip. A signed `chain.held` record or root marks a chain held for good, so a stripped tip over it breaks the chain rather than reading as unanchored.

| Option | Lost on | Cost |
| --- | --- | --- |
| Held, unanchored or read-only, with explicit anchoring *(chosen)* | — | A chain appended unanchored carries no signature until its owner anchors it. |
| Require a key for every append | Availability | A read path without issuer custody cannot record, so it cannot serve. |
| Anchor silently at the first held open | Tamper evidence | Stripping every signature and rewriting entries reads as an unanchored chain the next open signs. |
| Generate a local key on first append | Key custody | A read fabricates durable key material nobody chose. |

Criteria: tamper evidence decided it; availability of reads without custody; no fabricated keys.
Consequences: an unanchored interval is evident until anchored; a verifier runs beside the writer; deleting every root and `chain.held` along with the tip leaves a chain only a replicated root tells from an unanchored one.

## One append group holds the log at a time

**Status:** accepted

Context: `serve --http` and `mcp` run as separate processes on one project, and each answered read appends to the project's chain before its rows leave. A lock held for a process's lifetime admits one of them.
Decision: `disclosure.record.single-writer` holds `audit.lock` per append group. A group takes the lock, re-reads the last segment's tail when it grew since this handle's last write (`disclosure.record.foreign-tail`), links, syncs, then releases. The open, the idle signer and an export take the lock the same way.

| Option | Lost on | Cost |
| --- | --- | --- |
| Directory lock per append group *(chosen)* | — | A group waits while another process's group syncs; a tail re-read follows each interleaving. |
| Directory lock for a process's lifetime | Concurrent read processes | A second read process on one project cannot start. |
| One chain per process | Truncation detectability | A deleted chain leaves no gap in any other, so it goes undetected. |
| A daemon owning the log, appends forwarded over a socket | Scope | A third long-lived process, its lifecycle and its transport to build and operate. |

Criteria: one linear chain per project, fixed; several read processes per project decided it; one sync per group, kept.
Consequences: the lock serializes groups across processes, so throughput across processes is bounded by one sync at a time; a read-only handle still verifies without the lock.

## `audit_reads` is a view over the chain, and a refused read appends

**Status:** accepted

Context: an operator answers who read what over a window in SQL, and explanation replays refusals too.
Decision: `audit query` computes `audit_reads` from the chain's segments on each call, for the store owner alone, and no reserved table holds it (`disclosure.record.reads-view`). A read enforcement refuses appends an entry with `outcome` `refused` and the relations the guard parsed, releasing no rows (`disclosure.record.refused-read`).

| Option | Lost on | Cost |
| --- | --- | --- |
| A view computed per query from the segments *(chosen)* | — | Each query parses the whole chain; a longer chain is a slower query. |
| A reserved table landed beside the chain | One record | A second persisted state model drifts from the chain and needs its own erasure. |
| A projection in `derived.sqlite` rebuilt on append | One record | Every append pays a second write, and a crash between the two leaves them disagreeing. |
| A refused read leaves no entry | Typed refusals | A probe of ungranted tables stays invisible to the record and to explanation. |

Criteria: one record decided it; a refusal is typed and recorded (P2); a lookup over 24 h answers within 1 s.
Consequences: the chain carries refusals, so it grows with probes as well as serves; query cost scales with the chain's length rather than an index, which the projection-latency ledger entry tracks.

## Erasure is a forced rewrite, a bounded cascade and a measured receipt

`disclosure.erase` rewrites columnar files under the complement of the tenant grant filter; an undeclared subject column raises `ErasureSubjectUndeclared`. The cascade walks provenance to 16 hops, else `ErasureCascadeUnbounded` commits nothing; fact reads refuse with `ErasureRestagingRequired` until re-synthesis. Files holding erased rows are rewritten or collected within 24 h. A token-presented purge raises `PurgeRequiresOwner`. `disclosure.receipt` attests rewrite-and-exclude over the canonical store, names exclusions in `coverage`, and widens only with `receipt_version`.

| Option | Lost on | Cost |
| --- | --- | --- |
| Forced rewrite, bounded closure, measured receipt *(chosen)* | — | Cost scales with the table; fact reads go down until re-synthesis; a no-op re-run costs a full scan. |
| A one-hop cascade | Completeness | A fact derived from a derived fact outlives its evidence. |
| An unbounded transitive walk | Termination | Rests on provenance the store never proves acyclic. |
| Crypto-shredding a per-subject key | Fit with the substrate | Every write path holds a per-subject key forever. |
| A broad "data destroyed" receipt | Assertability | The object interface has no delete; the claim is false when signed. |

Consequences: replicas and the replication bucket sit outside the claim; the receipt's `tenant_hash` is salted SHA-256.
Revisit: the substrate gains a delete; 72 h request-to-last-replica erasure becomes an engine bound.
