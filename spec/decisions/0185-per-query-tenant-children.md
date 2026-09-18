# 0185 — A tenant-scoped credential is derived per query with a lifetime in minutes, not held per run

**Status:** accepted 2026-09-18
**Decides:** `authority.grant.limit.tenant-child-lifetime`

## Context

A consumer's service holds one broad parent credential and serves many tenants from it.
The narrowing that makes a given read safe is the tenant scope, and that scope is
derived locally: a holder appends a block and signs, with no round trip to the issuer.
The cost of deriving is therefore a signature, not a network call, which makes the grain
of derivation a free choice rather than a budget question.

What is not free is what happens to a credential once it exists. Work in this system
runs as durable runs, and a durable run persists its own record: the arguments a step
was called with, what it returned, what it carried between steps. A credential held
across steps is written into that record by the normal operation of the run machinery.
Records outlive runs. They are read back for tracing, for replay, and by anyone holding
a grant over the pipeline.

That is the shape of the problem. A credential's safety rests on an expiry, and an
expiry is an assumption about how long the bytes are reachable. A persisted credential
breaks that assumption by construction: it is reachable for as long as the record is
kept, which is a retention decision made by an entirely different part of the system for
entirely different reasons.

## Decision

A tenant-scoped child credential is derived per query and carries a lifetime of 15 min.
The consumer's service holds the broader parent continuously and mints the child locally
at the moment of the read. A credential that a durable run's record does persist has
already lapsed by the time anything reads that record back.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **One child per query, 15 min lifetime** *(chosen)* | What lands in a run record is already expired. The window between minting and lapse is bounded by the read itself. | A derivation on the consumer's hot path for every query, and a parent credential the consumer's service holds continuously in memory. |
| One child per run | One derivation per run instead of per query; the credential is naturally scoped to the unit of work. | Loses on persistence: a credential carried across steps is exactly what a run record writes down, so it is reachable for the record's whole retention, long after its expiry stopped meaning anything about who can read it. |
| One child per session | Fewest derivations. Simplest to reason about at the call site. | Loses on what a run record persists, at a coarser grain: the credential sits in every record for the whole retention, and the longer lifetime a coarse grain demands widens that window further. |
| Redact the credential out of the run record | Keeps the coarse grain and removes the persistence. | Loses on completeness of enforcement: the redaction has to hold at every write site the run machinery has, now and after every change to it, and one missed site restores the original failure with no signal. |
| Issue each child from the issuer rather than deriving | Central record of every scoped credential. | Loses on the offline property: a round trip per query puts the issuer on the read path, and the whole derivation model exists to keep it off. |

## Criteria

1. **What a durable run's record persists** — whether the credential is written into an
   artifact whose lifetime is governed by retention rather than by expiry. *This
   criterion decides.* A persisted credential outlives every assumption its expiry made,
   and unlike the other costs here, that one cannot be recovered by tuning a number.
2. **Cost on the read path** — derivations per query, and what the service holds.
3. **Completeness of enforcement** — whether the property holds by construction or by a
   rule applied at every site that could break it.
4. **Round trips at read time** — whether the issuer is on the hot path.

## Consequences

The consumer's service now holds a long-lived broad parent in memory continuously, which
concentrates the thing worth stealing in one place. That is a deliberate exchange: one
well-guarded parent in a process, in return for no long-lived scoped credentials
scattered through run records, logs and traces.

Run records become safe to read with an ordinary pipeline grant, since any credential in
them is inert. Tracing and replay stop being privileged operations for that reason.

The accepted cost is a signature per query on the read path, plus the operational
requirement that the service's clock be close enough to a checkpoint's that a 15 min
window is usable — clock skew that would be invisible at an hour's lifetime is a live
concern at this grain.

Reversing toward a coarser grain is expensive because the run-record property is what
downstream readers will rely on: making credentials in records live again would
retroactively reclassify every retained record.

## Revisit triggers

- Run records stop persisting step arguments, or gain a credential-aware write path that
  holds by construction rather than by rule, which removes the criterion that decided
  this.
- Derivation cost becomes measurable on the read path — visible as signature time
  appearing in query latency at the tail.
- Clock skew across checkpoints is observed producing refusals at 15 min that would not
  occur at a longer lifetime, which is an argument about the number rather than about
  the grain.
