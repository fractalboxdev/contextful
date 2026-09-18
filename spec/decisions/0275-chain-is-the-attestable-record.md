# 0275 — The hash chain is the attestable record and telemetry is its projection

**Status:** accepted 2026-09-18
**Decides:** `accountability.attest.refusal.broken-link`

## Context

Each read leaves two artifacts carrying the same attributes. One is a span, exported to a
collector and queried by an operator asking which agent read which table under which policy.
The other is an entry appended to a local segment file, carrying a sequence number, its
predecessor's hash, and a digest over both plus the attributes. Two records of one event, and
an attestation has to rest on exactly one of them.

They differ in what a compromise can do to them undetected. A span leaves the process and its
fate is the collector's: dropped at ingest, expired by retention, filtered by a rule, deleted
by whoever administers it — and in every one of those cases, the remaining spans are perfectly
consistent. Nothing in a set of independent records reveals that a record was removed from it.
The operator querying the projection sees a smaller answer and no indication that it is
smaller than it should be.

Linkage is what changes that. Each entry names its predecessor's digest, so removing an entry
or reordering two of them leaves the next entry pointing at a hash nothing produces. The
genesis entry's predecessor is a literal zero hash, which anchors the sequence, and
verification walks forward checking three things per entry — its sequence number, its link, and
its recomputed digest — returning the index of the first that fails any of them. Deletion and
reordering become arithmetic facts rather than suspicions.

A signature without linkage does not get there. Signing each entry proves who wrote it and that
its contents are unaltered, and says nothing whatever about whether entries between two signed
ones were removed. Signatures are orthogonal to completeness; that is why roots are signed over
the tip hash of a segment rather than over entries individually.

The remaining question is what a locally held chain survives. An actor with the node can
truncate the tail, and no purely local structure prevents that. Signed segment roots reaching a
replication bucket on a cadence carry history off the node that produced it, so a truncation
below a committed root is visible to anyone holding the root. An external append-only
transparency log strengthens that further — but a deployment that must run air-gapped cannot
depend on one, and a record that only works when a third party is reachable is not the record
this engine can require.

## Decision

The chain is the attestable record, authoritative on whether anything was altered; telemetry is
the queryable projection an operator searches. A recomputed digest disagreeing with the stored
one, or a gap in the sequence, raises `AuditChainBroken` carrying the index of the earliest
such entry. Verification runs against a pinned issuer public key with no network call and no
credential, and a root carries no algorithm field — the pinned key's scheme decides how the
signature is checked. Signed roots reach the replication bucket on a cadence, carrying history
off the node that produced it. An external transparency log accepts those roots, is off by
default, and no deployment depends on it.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A linked chain as the record, signed roots replicated off-node, telemetry as a projection** *(chosen)* | Deletion and reordering are detectable from the data itself, and history survives the node that wrote it without requiring a third party to be reachable. | Two records to reason about, with the chain carrying the obligation to be right; signed roots depend on issuer key custody staying live. |
| Telemetry spans alone | One artifact, already exported, already queryable; no second storage path. | Lost on detectability: a collector drops a span with nothing in the remaining data proving it, so the record cannot distinguish a quiet period from a deletion. |
| A signed log without linkage | Authenticity and per-entry integrity, with simpler verification and no chain state to carry. | Lost on the same criterion: signatures over independent entries do not reveal a deleted entry, because each surviving signature is still valid. |
| An external transparency log as the primary record | The strongest tamper evidence available — a third party holds what the operator cannot rewrite. | Lost on air-gapped operability: a record that requires a reachable third party is not a record some deployments can keep. It is retained as an optional layer over the chosen one. |
| A chain held only locally, with no replicated roots | One storage path, no custody obligation, no cadence to tune. | Lost on surviving a compromised node: an actor holding the node truncates the tail and the shortened chain verifies cleanly against itself. |

## Criteria

1. **Detectability of deletion and reordering** — whether removing or moving a record leaves
   evidence in the data. **This criterion decided.** Alteration of a record's contents is
   caught by a digest and every candidate here does that; what separates them is whether a
   record's absence is visible, and an attestation over a record that can be silently shortened
   attests to nothing an adversary did not choose to leave.
2. **Survival of a compromised local node** — whether history outlives an actor with the disk.
3. **Air-gapped operability** — whether verification and recording work with no network.
4. **Credential-free offline verification** — whether an auditor checks a claim with a pinned
   key and identifiers already in hand.
5. **Number of artifacts to maintain** — the chosen option carries two. This is the cost.

## Consequences

An auditor's question has one answer: the chain says whether anything was altered, and the
projection is a convenience that carries no evidential weight. That split also settles what the
projection may lose — spans dropped at a collector leave entries intact, and an unreachable sink
does not stop the chain from growing. Verification is self-contained: the payload is checked
against its digest, the digest against the signature and the pinned key, both steps without a
network call. A chain whose earliest segment is absent reports the lowest sequence it holds and
checks forward, which distinguishes a truncated archive from an altered one instead of
conflating them.

The cost accepted: two records exist for one event, and they can disagree. The chain carries the
obligation to be right, which means attribute parity between the two is an invariant somebody
maintains rather than a property that holds by construction. The second cost is custody: signed
roots are only as good as the issuer key's protection, and a key that leaks lets an actor sign a
rewritten history that verifies against a pinned public key auditors already hold. Nothing in
this decision bounds that; it is handed to the custody model, and the exposure window between a
key compromise and its discovery is unmeasured.

## Revisit triggers

- Attribute drift between spans and entries is observed, meaning parity is not holding as an
  invariant and the two records have begun to disagree.
- Issuer key custody proves unable to keep a key sealed in a deployment profile the engine must
  serve.
- A deployment set emerges that is uniformly online, where a transparency log could become the
  primary record rather than an optional layer.
