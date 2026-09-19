# D07 — Coordination is probed compare-and-swap with fences the storage enforces

**Status:** accepted

## Context

Advancing a cursor, compacting a table and pushing a store each need one writer. A lease holder can pause past its expiry and resume believing it holds the lease; a fence it compares before committing passes, and it commits after its successor. A compaction owner named in local configuration carries no fence at all.

## Decision

- `topology.coordinate` rests on one linearizable conditional write behind the `Catalog` port; a backend without it refuses at open with `ConditionalWriteUnsupported`.
- `store.probe` decides whether an object store performs atomic conditional writes by a live sentinel inside the configured prefix. An inconclusive probe raises `SyncProbeInconclusive`, counts as not demonstrated, and a declared `cas` push stops with `SyncCoordinationUnproven`.
- `store.lease` acquisition increments a fence the storage enforces. A leased run commits by conditionally creating the next `cursors/<pipeline_id>/<seq>.json` carrying its fence; a table publishes by conditional replace of `_pointer.json`; a catalog update is predicated on the fence. Losing any of these to a higher fence raises `LeaseFenced`. Release sets `holder` to null and keeps the object, so the fence never resets.
- A run meeting a held lease raises `LeaseHeld`, naming holder and expiry, and is retried next tick; nobody waits on a lease or frees another's.
- `store.fold` compacts a table only under its compaction lease, stamping the fence into the snapshot manifest and the pointer.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Storage-enforced fences; per-table compaction lease *(chosen)* | — | One conditional write per commit, and a lease object per pipeline and table that is never deleted. |
| Compare the fence in the writer before commit | Pause safety | Check-then-act: a holder pausing between check and write commits after its successor. |
| A compaction owner named in configuration | Divergent configuration | Two machines naming themselves both compact, and neither commit is fenced. |
| Wall-clock expiry checked at commit | Clock trust | The paused machine is the one whose clock is least trustworthy. |
| An embedded consensus cluster | Operational cost | A second system to run for a low request rate. |

## Consequences

- Exclusion survives a pause: a predecessor lands nothing after a successor acquires.
- A backend without demonstrated conditional writes runs single-writer by refusal, never by silent degradation.

## Revisit

- A backend whose access policy enforces a generation bound for every writer.
