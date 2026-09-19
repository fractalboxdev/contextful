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

`disclosure.release` stages no breaching cell: partition selection is a noisy threshold over contributor-bounded counts. A release reserves per-unit spend for every contributing unit in one catalog transaction before reading, refusing with `DisclosureUnitBudgetExhausted`. `max_contributor_share` withholds a concentrated group. A table is `per-person` or `cohort`; `DisclosureCohortWidening` and `DisclosureSingletonCohort` guard cohorts.

| Option | Lost on | Cost |
| --- | --- | --- |
| Build-time enforcement, noisy partition selection, reservation up front *(chosen)* | — | A policy edit is a rebuild; published figures carry noise; an exhausted unit blocks every release naming it. |
| Suppress on exact counts, then add noise | Privacy guarantee | Cell presence discloses whether a contributor crossed the threshold. |
| Debit spend after the release | Concurrency | Two concurrent releases each observe budget the other spends. |
| Skip exhausted units silently | Truthfulness | A result reads as complete while its shortfall reveals which units spent their budget. |
| A query-time aggregate evaluator | Survival across read paths | Reimplemented per surface; cannot recover per-contributor mass from a rollup. |

Consequences: every read path inherits the guarantee from the staged bytes; raising a unit's lifetime cap is an explicit grant edit.
Revisit: lookalike segment release settles its audience floor, readback rule and cross-party consent.

## A cross-owner release runs only when the boundary is enforced below the engine

`disclosure.release` against a cross-owner store requires per-tenant signed manifest entries, per-tenant write prefixes enforced by the object store's access policy, and per-tenant signing keys, else `DisclosureCleanRoomPreconditionUnmet`. A hashed join keys on a per-pair escrowed pepper rotated per join; a static pepper raises `DisclosureStaticPepper`. A segment releases identifier, size, coarse cells and an activation handle; member readback refuses. A pool over other tenants' end users requires a recorded consent contract.

| Option | Lost on | Cost |
| --- | --- | --- |
| Store-enforced preconditions, per-pair pepper, activation-only segments, pool-keyed posture *(chosen)* | — | Stores unable to express per-prefix writes are ruled out; pepper escrow is operational work; a requester cannot audit the segment it activates. |
| Document the protections as operator responsibilities | Detectability | Failure arrives silently as corrupted data or a forged grant. |
| Accept two of three preconditions | Coverage | The third path stands fully open. |
| A static pepper shared across joins | Pepper compromise | One recovered pepper reverses every join, both directions. |
| Posture keyed on the scoring method | Boundary crossing | Identical mathematics over a different pool changes who discloses to whom. |

Consequences: a segment is size-gated and opaque, not differentially private; the engine verifies a consent contract's binding, not its legal validity.
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
