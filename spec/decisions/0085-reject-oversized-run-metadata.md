# 0085 — A metadata write past the serialized bound is rejected and the snapshot stands unchanged

**Status:** accepted 2026-09-18
**Decides:** `run.project.limit.metadata-size`, `run.project.refusal.oversized-metadata`

## Context

The wire snapshot of a run carries an application metadata map alongside the run status and
the step list. Unlike the status and the steps, that map is free-form: whatever the running
application decides to publish about its own progress — a phase name, a counter, a
vendor-side job handle. Nothing in the engine constrains its shape, so nothing in the engine
bounds its size unless a bound is stated.

The map sits on the hot side of the projection. Every fold produces a complete snapshot, the
per-run broadcast holds 256 entries, and a burst coalesces to one snapshot per window — so a
single oversized map is resident once per retained snapshot and again per connected
subscriber, on a path whose whole guarantee is that emission never blocks a run. The
projection is best-effort and in-memory; a process restart discards it. Memory spent there
buys nothing durable.

The write is also the only place the size is knowable. Serialization happens at the write
site; by the time the map is inside a folded snapshot on its way to a socket, the caller who
could have reshaped it has returned.

## Decision

Application metadata on a snapshot is bounded at 16384 B serialized, enforced where the write
is made. A write whose serialized form exceeds the bound raises `RunMetadataTooLarge` carrying
the serialized size and the bound, and the snapshot's metadata stands exactly as it was. No
key is dropped, no value is shortened, and no partial map reaches a subscriber. An application
that needs to publish more than the bound reshapes what it publishes or writes the payload
where payloads live and publishes the reference.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Bounded, rejected at the write site** *(chosen)* | The author learns at the write that the value did not land, with the size and the bound in hand; the hub's per-run memory has a stated ceiling. | The write site carries the enforcement and the error, and an application whose progress genuinely exceeds the bound restructures it. |
| Truncate to the bound | Every write succeeds; no error path in the application. | Loses on silence — the clipped value reads downstream exactly like a complete one, and the author never learns a write was clipped. A truncated serialization is also frequently unparseable, so the reader sees corruption rather than a short value. |
| Evict oldest keys until the map fits | Keeps the newest facts, stays under the ceiling. | Loses on silence for the same reason, and worse: which facts survive depends on insertion order, so two runs publishing the same keys report different maps. |
| Leave the map unbounded | No bound to pick, no error to handle. | Loses on the memory ceiling — the snapshot has no stated size at all, and the ring multiplies it by 256. A run that publishes a batch payload as metadata degrades the hub for every subscriber on the machine. |

## Criteria

1. **Silence** — whether the bound can be exceeded without the writer learning it. *This is
   the criterion that decided it.* A bound that is enforced by quietly altering the value
   converts an author's mistake into a reader's wrong answer, and the reader has no signal
   that anything happened. Every option that keeps the write succeeding fails here, which
   outranks their advantage on ergonomics: an error at the write site is recoverable in the
   application, while a clipped value read as authoritative is not recoverable anywhere.
2. **Memory ceiling of the live hub** — whether the resident cost of a run's projection is
   stated. The bound times the ring depth is the number that answers it.
3. **Enforcement locality** — whether the size can be measured where the caller can still act
   on the answer. Only the write site satisfies this.
4. **Author burden** — how much an application changes to stay inside the rule.

## Consequences

Reading a run's metadata needs no defensive handling for a partially present map: a value
either landed whole or never landed. The hub's worst-case per-run footprint is a
multiplication of two stated numbers rather than an open question.

The cost accepted is on the writing application. A progress map that outgrows 16384 B has no
graceful degradation: the write fails, and the application either narrows what it publishes or
moves the bulk into a payload and publishes a reference to it. That is a real edit at an
awkward moment, since the size is discovered at runtime rather than at compile time. The bound
itself is a judgment, not a measurement — no distribution of real metadata sizes is available
to place it against, so its only defense is that it is far above any progress map that is
genuinely progress and far below anything that would strain the ring.

## Revisit triggers

- An application publishes a metadata map that is legitimately a progress description and
  exceeds the bound, and reshaping it produces a worse description rather than a better one.
- Metadata stops being projection-only and becomes part of the durable run record, which
  changes the memory argument entirely and reopens the tier question.
- The projection gains a reference tier for large values the way journaled outputs have one,
  at which point an oversized write has a destination other than an error.
