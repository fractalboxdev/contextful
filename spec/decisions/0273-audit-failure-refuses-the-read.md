# 0273 — A read whose audit entry cannot be persisted is refused

**Status:** accepted 2026-09-18
**Decides:** `accountability.record.refusal.unpersisted-entry`

## Context

Every read produces two artifacts: a span for querying and an entry for proving. The entry
carries a sequence number, a link to its predecessor, the read's attributes and a digest over
all three, and it appends to a numbered segment file. The genesis entry's predecessor is a
literal zero hash, and removing or resequencing any entry after it breaks the linkage at that
point. That linkage is the entire value of the record: an auditor who recomputes digests and
walks sequence numbers can say whether anything was altered or removed.

A chain with a hole says nothing. If a read can be served while its entry fails to persist,
then a gap in the sequence has two possible meanings — tampering, or a disk that was full on a
Tuesday — and no third party can tell which. The mechanism is not degraded by that; it is
finished. An auditor asked to attest over a chain whose gaps are routine has nothing to attest
to.

So the question is not how likely a persist failure is. It is whether a missing entry is
permitted to exist at all. Disk full, permission denied, bucket unreachable — the specific
cause does not change what the resulting chain can prove.

The mitigations that preserve availability all write their evidence with the path that just
failed. A gap marker is an entry, appended to the same chain, through the same write path: at
the moment the write path is broken, the marker is exactly as likely to be lost as the entry
it was standing in for, and its absence is indistinguishable from the tampering it exists to
rule out. Buffering and retrying in the background moves the failure without removing it — the
rows have already been returned by the time the retry gives up, and the read they covered is
now uncovered with no way to recall it.

The failure also has to leave the process and the disk agreeing. An in-memory tip that
advanced past a failed persist would link the next entry onto a predecessor no segment file
holds, producing a break on the next successful write.

## Decision

A read whose entry cannot be written — a full disk, a denied permission, an unreachable
bucket — is refused with `AuditEntryUnpersisted`, and the caller receives no rows. No path
returns rows without a durable entry covering them, and that refusal is the sole outcome when
the write path fails. A failed persist pops the in-memory tip, leaving the process state and
the on-disk segment in agreement about the last accepted sequence number. The telemetry sink is
separate and carries no such obligation: an unreachable collector does not stop the local chain
from growing, and spans dropped at the collector leave the entries intact.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the read; no rows without a durable entry** *(chosen)* | A gap in the chain has exactly one possible cause, so an auditor's verdict on the record is worth something. | Audit storage becomes a first-class availability dependency of the read path: a full disk or an unreachable bucket takes reads down. |
| Fail open and record a gap marker | Reads keep serving; the record admits its own hole rather than hiding it. | Lost on evidential completeness: the marker is written by the path that just failed, so its absence and a deletion look the same, and the chain stops distinguishing them. |
| Buffer entries and retry in the background | Absorbs transient failures with no user-visible effect; the common case costs nothing. | Lost on the same criterion: rows are already returned by the time the retry fails, and the uncovered read cannot be recalled. |
| Degrade to telemetry-only on audit failure | Some record survives; the operator can still see what was read. | Lost because telemetry is a projection, not the attestable record — it makes the record optional at the exact moment it matters, and a collector drops spans with nothing in the data proving it. |
| Replicate the audit write to a second store and refuse only if both fail | Keeps the guarantee and removes most of the availability cost. | Lost on scope rather than on principle: it is a durability design for the chain's storage, and it composes with this decision instead of replacing it. |

## Criteria

1. **Evidential completeness** — whether a gap in the chain has one cause or several. **This
   criterion decided.** The chain's only product is the ability to say that nothing was removed
   or altered, and that ability is binary: one permitted benign gap makes every gap ambiguous
   forever. Availability of the read path is the criterion it was weighed against, and it lost
   because a highly available record that proves nothing is not a cheaper version of the
   mechanism — it is a different, useless one.
2. **Availability of the read path** — whether audit storage can take reads down. The chosen
   option is the worst here.
3. **Where the evidence for a failure is written** — whether the artifact recording a failure
   depends on the path that failed.
4. **Recoverability after rows leave** — whether a deferred failure can be repaired once the
   caller holds the result.
5. **State agreement across a failure** — whether the process and the segment file still agree
   on the last accepted sequence number.

## Consequences

An auditor reading a chain gap knows it means tampering or truncation, and the partial-history
walk distinguishes a truncated archive — earliest segment absent, verify forward from the
lowest sequence held — from an altered one. Every returned row is covered, so coverage is a
property of the design rather than a statistic.

The cost accepted: audit storage is now on the critical path for every read. A deployment whose
chain lives in an object store inherits that store's availability as a read-path availability
ceiling, and a local deployment inherits its disk. Operators will encounter reads failing for a
reason that has nothing to do with the data they asked for, and the remedy is storage capacity
and permissions rather than anything about the query. The separation of the two surfaces keeps
that cost bounded to one of them: the telemetry sink resolves unset by default and drops to a
no-op, so a collector outage is not a read outage.

Reversing this is a one-line change and it retroactively devalues every chain the deployment
has ever produced, because an auditor cannot know which version wrote a given segment.

## Revisit triggers

- Audit-storage unavailability becomes a material share of read-path downtime in a real
  deployment, which would make a replicated-write design urgent rather than optional.
- A durability arrangement is adopted under which a persist failure is provably distinguishable
  from a deletion by an auditor holding only the chain.
- A deployment profile appears whose reads must serve under conditions where durable local
  storage is not available at all.
