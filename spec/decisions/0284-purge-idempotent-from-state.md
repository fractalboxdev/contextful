# 0284 — A repeated purge derives idempotence from state and returns a freshly signed equivalent artifact

**Status:** accepted 2026-09-18
**Decides:** `accountability.receipt.refusal.replayed-artifact`

## Context

A tenant purge is long. It rewrites every reachable file — committed run files, compaction
snapshots, each model build, then the memory mirror — holding the per-model write lock across
each table's sweep. An operation of that length meets client timeouts, and a caller that
times out has no way to know whether the rewrite completed, so it retries. Retrying a
destructive operation is either safe or it is a second full rewrite of the whole store.

What the caller needs on the retry is not merely safety. They need the artifact. A purge whose
retry is safe but returns nothing leaves the caller having done the work twice and holding no
evidence either time, which is the same position as never having called.

There is durable state to answer from. The ledger holds one row per request with a completion
timestamp, keyed on the tenant digest, and it survives a catalog rebuild. A re-run can read
the last completed request for that hash before opening its own.

The artifact is a signed claim with a timestamp in it, and the timestamp means something
specific: when the check ran. That the store held zero rows of this tenant is a statement about
an instant, and the signature is what makes a reader trust the instant.

## Decision

A re-run over an already-purged tenant reads the last completed request for that hash before
opening its own, removes nothing, and returns an equivalent artifact: a fresh signature and
timestamp over the same claim, zero counts, and `prior_request_id` naming the completed
request. No request-deduplication cache stands between a caller and a re-run; equivalence is a
property of the claim rather than of the bytes. Returning the byte-identical earlier artifact
in place of a freshly signed one raises `ReceiptReplayed`, since a replayed timestamp
misstates when the check ran. A re-run walks every reachable file to establish that zero rows
match, and the zero counts it reports are measured rather than assumed. `prior_request_id`
links each artifact to the completed request before it for that hash, giving a tenant's
erasure history as a single traversable line.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Idempotence derived from ledger state; a freshly signed equivalent artifact each time** *(chosen)* | A caller that timed out retries and holds a valid, current, verifiable artifact, and the line of `prior_request_id` links shows the whole history. | A re-run still walks every reachable file to establish zero matches, so a no-op purge costs a full scan. |
| A request-deduplication cache keyed by an idempotency token | Cheap: the second call returns immediately with no store work at all. | The entry expires. A retry after the window turns a safe repeat into a second full rewrite, and the caller cannot see which side of the window they are on. |
| Return the byte-identical earlier artifact | Obviously idempotent, trivially cheap, and the signature is already valid. | The timestamp then claims a check ran at an instant when it did not. A holder presenting it asserts current emptiness on the strength of an old measurement. |
| Refuse a second purge for an already-purged tenant | Unambiguous: the operation happened, and repeating it is a client error. | A caller that never saw the first response has no path to evidence. The refusal is correct and useless. |
| Return the equivalent artifact without re-scanning, taking the zero counts from the ledger | Cheap and returns a current, correctly timestamped artifact. | The timestamp then attests a check that was not performed. Nothing rules out a write landing between the purges, so the zeros would be assumed rather than measured. |
| Skip the artifact on a re-run and return the prior `request_id` | Cheap; the caller can fetch the stored artifact under that identifier. | It hands back a pointer where the caller asked for evidence, and the stored artifact carries the old timestamp anyway. |

## Criteria

1. **What the caller holds after a timeout and a retry** — a valid, current, verifiable
   artifact, or something less. The cache (after expiry), the refusal and the pointer all fail
   this.
2. **Truthfulness of the timestamp** — whether the instant in the artifact is an instant at
   which the check actually ran. Replay and ledger-derived zeros both fail this.
3. **Truthfulness of the counts** — whether zero was measured or assumed.
4. **Safety of an unbounded retry** — whether repeating the call at any later time is still
   safe. The cache fails this at exactly the moment the operator stops watching.
5. **Cost of a no-op** — whether a purge over an already-empty tenant is cheap. This is the
   criterion the chosen option loses on.

What the caller holds after a timeout and a retry decides it. State-derived idempotence is the
only option that still yields a valid verifiable artifact on the retry, and that outcome
outranks cache simplicity, because the timeout-and-retry path is the common path for an
operation of this length rather than an edge case. Truthfulness of the timestamp is what
eliminates replay specifically: a signature over a stale instant is a stronger false statement
than the same words unsigned.

## Consequences

Retry is unconditionally safe and unconditionally useful: a caller may repeat the purge any
number of times, at any interval, and each response is a currently-true, freshly signed claim.
The chain of `prior_request_id` links reads as one traversable history per tenant, so an
auditor can see how many times emptiness was confirmed and when.

The accepted cost is that the cheapest possible operation — purging a tenant with no rows —
costs a full walk of every reachable file, and a caller retrying aggressively pays that cost
per attempt with no rate limit in the engine. A deployment facing an automated retry loop has
to bound it outside the verb.

Reversing this toward a cache is cheap in code and expensive in meaning: artifacts already
issued carry measured zeros, and later ones would carry assumed zeros with no field
distinguishing them.

## Revisit triggers

- A cheap negative test — a per-tenant row count maintained by the catalog, or a
  sketch — becomes available and is itself trustworthy enough to attest, which would make the
  full scan unnecessary without assuming the result.
- Purge duration is measured as routinely exceeding client timeouts, which would make the
  retry path the normal path and price the no-op scan against it.
- The artifact gains a distinction between a measured and an inherited claim, at which point a
  cheap re-run could return an honestly weaker one.
