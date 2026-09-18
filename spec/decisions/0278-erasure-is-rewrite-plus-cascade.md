# 0278 — Erasure is a forced rewrite plus a one-hop lineage cascade under one audited verb

**Status:** accepted 2026-09-18
**Decides:** `accountability.erase.refusal.undeclared-subject-column`

## Context

The store is append-only over columnar files. A commit adds files; nothing in the ordinary
write path removes a row, and the object interface the store is built on exposes get, put
and list and no delete. Compaction rewrites files, but on a cadence chosen for read
performance, and a cadence carries no deletion commitment — it will get to a file eventually
and cannot say when.

A person's data does not sit only in the rows that were ingested about them. The run path
synthesizes derived facts whose provenance lists the evidence that produced them, a memory
mirror holds a derived copy of subject-keyed shapes, and a self-directed run files rows of
its own accord. Removing the ingested rows and leaving the derived ones standing removes the
record while preserving the conclusions drawn from it, which is the outcome a right to
erasure exists to prevent.

The system also cannot address a person at all without being told how. A table's rows carry
whatever columns its source produced; nothing in the engine knows which of `user_id`,
`author`, `email` or `account` identifies a natural person, and the three plausible guesses
in a given table point at three different populations. Guessing wrong deletes somebody
else's rows and reports success.

Finally, erasure is evidence. The operation has to leave a record an auditor can check, and
that record has to say what was narrowed if anything was, since a partial erasure that reads
as a complete one is worse than a refused one.

## Decision

Erasure runs as one audited verb over a declared column. A table names its subject-identifier
column in its manifest, and a subject erasure against a table declaring none raises
`ErasureSubjectUndeclared` naming the table. The subject path writes tombstones over every
row about that person in the subject-keyed shapes, then invalidates, in the same operation,
every derived fact whose provenance references the subject key or any identifier that direct
pass tombstoned. The cascade travels one lineage hop. The tenant path rewrites the columnar
files instead. Both append to the same chain the read path writes, and narrowing the subject
path to direct rows is recorded there, leaving an incomplete erasure visible to the next
auditor.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Forced rewrite plus a one-hop cascade under one declared column** *(chosen)* | No reachable path returns the rows: run files, snapshots, model builds and the memory mirror are all addressed by the one operation, and derived conclusions fall with their evidence. | The cost scales with the table rather than with the erased slice, and superseded objects stay in the bucket until collection. A table that declares no subject column has no subject-erasure path until it does. |
| Tombstone only, with no rewrite | Cheap, instant, and the read path already honours markers. | Lost on reachability: the values stay in the bytes on disk, so a credential over the object store reads them. The marker constrains a query, not the disclosure. |
| Crypto-shredding a per-subject key | Erasure becomes a key deletion, constant time regardless of volume. | Lost on reachability: the store encrypts per table and per snapshot, not per subject. A per-subject key would have to be introduced into every write path and held for every subject forever, and key custody then becomes the erasure guarantee. |
| Wait for natural compaction to remove tombstoned rows | No new machinery; the rewrite already exists. | Lost on reachability: compaction's cadence is chosen for read performance and carries no deadline. A regime asking when the data left the store gets no answer. |
| Delete the underlying objects outright | Conceptually the strongest claim. | Lost on implementability on the substrate: the object interface has no delete. The claim would be unimplementable on the substrate the store is specified over. |
| Infer the subject column by name convention | Every table gets an erasure path with no authoring work. | Lost on addressing correctness: a wrong inference erases a different population and reports success, and the mistake is invisible in the output. Erasure is exactly the operation that cannot be best-effort. |
| Cascade to full transitive closure rather than one hop | Catches a fact derived from a fact derived from the evidence. | Lost on addressing correctness: provenance is recorded one hop deep, so a deeper walk would be reconstructed rather than read, and its termination depends on data the store does not guarantee is acyclic. |

## Criteria

1. **Reachability** — whether a later read returns the erased rows through any path the
   system offers: a pin, a held build, a snapshot, a derived fact, the memory mirror.
   Tombstone-only and compaction-waiting both fail here.
2. **Addressing correctness** — whether the operation acts on the rows it was asked to act
   on. Name-convention inference fails here.
3. **Operator obligation** — whether honouring a request is one command rather than a
   runbook. A multi-step procedure has a step somebody skips.
4. **Auditability of a narrowing** — whether a partial erasure is visible to the next
   auditor rather than indistinguishable from a complete one.
5. **Cost proportionality** — whether erasing one person costs work proportional to that
   person's rows. This is the criterion the chosen option loses on.
6. **Implementability on the substrate** — whether the claim an option makes can be carried
   out by the object interface the store is specified over.

Reachability decides it. An erasure that leaves any reachable copy has not erased anything;
it has added a filter. Proportionality is real and is paid, and addressing correctness is
what forces the declaration rather than a guess, since an operation that cannot be verified
after the fact must be correct before it runs.

## Consequences

The question "does this store still hold anything about this person" has one answer reached
by one command, and the chain says when it was reached and whether it was narrowed. Derived
facts cannot outlive the evidence under them by more than the one operation.

The accepted cost is threefold. A purge or erasure rewrites every reachable file, so a
request touching one row in a large table costs a full-table rewrite. Superseded objects
remain in the bucket until the expedited collection schedule reaches them, which is why the
receipt's claim is rewrite-and-exclude rather than destruction. And a table with no declared
subject column is simply outside the subject path, which surfaces as a refusal at request
time — when the deadline is already running — rather than at authoring time.

Reversing the one-hop bound is cheap; reversing the declared-column requirement is not, since
every deployment's manifests would then carry a declaration the engine had stopped needing,
and inference would have to agree with them.

## Revisit triggers

- Provenance records more than one hop for every derived shape, making a transitive cascade
  readable rather than reconstructed.
- Per-subject encryption keys become part of the write path for an independent reason, at
  which point shredding is a real option rather than a new subsystem.
- The object interface gains a delete, changing what the receipt can truthfully claim.
- Rewrite cost is measured as the dominant term in meeting the propagation bound on a
  realistic table, rather than replication lag.
