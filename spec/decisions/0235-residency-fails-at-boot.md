# 0235 — A configured resource outside the declared residency set halts the runtime at startup

**Status:** accepted 2026-09-18
**Decides:** `enforcement.reside.refusal.region-mismatch`

## Context

The data plane belongs to the deploying organization: columnar files, the manifest and the
catalog database live in a bucket that organization controls, and no vendor-operated data
path exists. A residency policy narrows that further by naming a set of regions. The set is
either empty, meaning unconstrained, or it lists the regions placement is permitted in, and
the check reads the declared set and the candidate region and consults nothing else.

Residency is unlike the other placement controls in one respect. An inference zone governs
where a result is processed and is resolved per request; residency governs where bytes sit,
and bytes sit somewhere continuously, whether or not a request is in flight. A bucket in a
forbidden region is a violation from the moment the process can reach it, not from the
moment a caller asks.

The region a resource resolves to is also not a property of the configuration. A bucket
keeps its name while its region changes — through a provider-side move, a rename, a
redirect, a replication target promoted to primary. So a check performed when the
configuration is written examines a fact that can change without the configuration
changing, and passes forever afterwards.

That leaves the question of what the runtime does when it finds a resource outside the set,
and when it looks.

## Decision

A configured resource resolving to a region the policy omits raises `EnforceRegionMismatch`
at startup, and the runtime does not begin serving. The check runs over every configured
resource before the process accepts a request, and a failure halts the whole runtime rather
than disabling the offending resource.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Resolve every configured resource at boot and halt on a mismatch** *(chosen)* | No row from a misplaced resource reaches a caller, because the process never reaches the state where it serves one. The violation surfaces at the moment the operator is already watching a deployment. | A region rename or a provider-side move takes the deployment down rather than degrading it, and recovery is an operator correcting the declaration, not the system healing. |
| Warn at boot and enforce per query | The deployment stays up; the violation is recorded; reads from the misplaced resource are stopped individually. | Loses on ordering: the warning sits in a log nobody reads at the moment it matters, and the per-query check is the one that fires — after the process has been serving for however long it took someone to notice. |
| Refuse the offending resource and run degraded | Partial service beats none; the rest of the store stays readable. | Loses on answer integrity: a data plane missing one region's resources returns answers that silently omit that region's rows, and a caller cannot tell an incomplete answer from a complete one. |
| Check at configuration time alone | Cheapest possible boot; the violation is caught where it is written. | Loses on the resource's independence from the configuration — a region changes without the declaration changing, so the check certifies a fact that has since stopped being true. |
| Check at boot and again per query | Catches the mid-run move the boot check cannot see. | Loses on cost and on redundancy at the common grain: a per-query region resolution is a remote fact consulted on every read, and it fires after the boot check has already passed. |

## Criteria

1. **Whether any row can cross the boundary before the violation is noticed** — the
   ordering between the check and the first served read. *This criterion decides.*
   Residency is a statement about where bytes are permitted to be, and a control that fires
   after the first read has already left the misplaced resource has not enforced it; the
   cost of halting is bounded and the cost of a crossing is not.
2. **Integrity of the answers served** — whether a running deployment can return a result
   that silently omits rows.
3. **Freshness of the fact checked** — whether the check reads the region now or certifies
   one read earlier.
4. **Availability under a provider-side change** — the criterion the chosen option loses on.

## Consequences

A running deployment is one whose every configured resource sits in a declared region, so
no request-time reasoning about residency is required anywhere downstream.

The cost accepted is availability. A region rename, a provider-side move or a promoted
replica in an undeclared region takes the deployment down at its next start, and it stays
down until an operator corrects the declaration. There is no degraded mode, deliberately:
the degraded mode would serve incomplete answers indistinguishable from complete ones.

The boot check also does not cover a move that happens while the process runs. A resource
relocated mid-run keeps serving until the next start, which is an unbounded window this
decision does not close — the per-query option that would close it was rejected on cost,
so the exposure is real and its duration is however long the process lives.

Reversing toward a warn-and-continue posture is expensive because downstream surfaces
assume residency holds for anything they can read; making it per-request would put a
residency check into every read path.

## Revisit triggers

- A mid-run region change is observed in a deployment, which turns the uncovered window
  from a known gap into an incident with a measured duration.
- Provider-side moves become frequent enough that boot halts are a recurring availability
  event rather than a rare one.
- A cheap change-notification path appears from the object store, making a continuous check
  affordable without paying region resolution per query.
