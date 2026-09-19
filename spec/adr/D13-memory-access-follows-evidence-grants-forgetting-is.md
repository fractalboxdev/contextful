# D13 — Memory access follows evidence grants; forgetting is a separate privilege

**Status:** accepted

## Context

A synthesized claim is derived content: its text carries the restricted value whether or not the reader can open the row it came from. Masks, grants and erasure move independently of the zone the claim inherited at write time. Evidence lists are unbounded, and a replica can pull between two local commits.

## Decision

A distilled conclusion never outlives the reach of its rows, and retiring it is a privileged, atomic act.

- `read.recall` serves a claim only when every evidence row resolves and reads through the caller's own enforced session. An unreadable, masked or unknown row, or malformed lineage, suppresses the claim with `MemoryEvidenceUnresolved` without failing the recall; a claim naming more than 256 evidence entries is suppressed with `MemoryEvidenceOverflow`.
- `disclosure.erase` sits outside the default grant set: a write grant alone does not carry it, and a caller without the forget grant raises `ErasureUngranted`. Tombstones and cascade markers commit in one local snapshot, and the verb returns after that commit.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Per-row resolution at recall, suppress on failure, 256 cap; separate forget grant with same-commit markers *(chosen)* | — | A claim whose source table is renamed goes silent with no reader-facing signal; erasure needs a second credential and a heavier commit. |
| Serve the claim with an unresolved-evidence marker | Containment | The marker discloses the conclusion the gate exists to hold. |
| Resolve evidence once at write time and cache the verdict | Revocation latency | A later mask, grant change or erasure never reaches the cached verdict. |
| Forget inside the write grant | Blast radius | A routine synthesis credential could erase curated claims. |
| Markers on the next ordinary commit | Marker durability | A replica pulling in between would resurrect the forgotten rows. |

## Consequences

- Revocation takes effect on the next recall with no invalidation sweep.
- Every returned claim pays a per-row evidence join, bounded per claim by the cap; total recall cost is unmeasured.
- Synthesis states narrow evidence rather than every row it read.

## Revisit

- Measured recall latency dominated by the evidence join.
- A suppression channel that names a withheld claim without naming its content.
- Evidence lists clustering at the 256 cap.
