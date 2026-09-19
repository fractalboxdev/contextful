# D33 — The control document is CAS-versioned, validated per entry, and fails static

**Status:** accepted

## Context

One hand-edited control document carries every scheduled entry, armed unattended. The armed set carries no edge between entries, so one entry's defect says nothing about another's.

## Decision

The blast radius of a fault matches the fault, and every version has one writer and one predecessor.

- `surface.apply` claims a version by compare-and-swap on an engine-assigned version; a loser raises `ManifestVersionConflict`, reloads the winner and reapplies its edits onto it. No claimed version is overwritten.
- The engine owns the control-state model. An unconfigured owner raises `ConfigOwnerUnconfigured`, an uninitialized store `StoreNotInitialized`, and a backend without strong conditional replacement `ConditionalWriteUnsupported`; no local writer substitutes.
- `surface.arm` validates each entry before it joins the armed set. An unreadable schedule or a guardrail refusal holds back that entry alone, by name; a malformed store-registry entry drops alone, and an unparsable registry payload falls back to the built-ins.
- A failed poll in `surface.reconcile` keeps the armed set in place, unchanged, until a parsable snapshot arrives.
- `surface.dispatch` starts one instance per due unit that starts from the store alone; a step of a dependent run is refused as a unit.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| CAS on an engine version, per-entry validation, fail-static poll *(chosen)* | — | A losing operator's edits land on a document they never read; an entry can sit unarmed with only a diagnostic; a permanently silent control plane leaves an old cadence running. |
| Last-write-wins on the pointer | Immutability | An applied version would be superseded by one that never saw it, with no event to observe. |
| A lock or lease in front of the document | Writer count under failure | A lease expiring mid-write would admit a second writer nobody can order. |
| Abort the whole reconcile on one bad entry | Blast radius | One typo would stop every unrelated schedule. |
| Arm empty, or fall back to local schedules, on a failed poll | Visibility of the failure | A transient read error would stop ingestion, or run a cadence nobody applied, while the process reports healthy. |

## Consequences

- An operator provisions credentials before the first edit; a surface re-derives engine-owned state on every read.
- Entries authored as a chain run as one unit under the head entry's cadence.
- Staleness from a control-plane outage is visible only in diagnostics.
