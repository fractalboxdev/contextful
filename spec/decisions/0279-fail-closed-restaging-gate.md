# 0279 — An armed restaging gate refuses every fact read until synthesis re-runs

**Status:** accepted 2026-09-18
**Decides:** `accountability.erase.refusal.stale-derived-read`

## Context

A subject erasure tombstones direct rows and invalidates the derived facts whose provenance
references the subject. Invalidation removes a fact from the store; it does not reconstruct
the facts that would have been synthesized had the evidence never been there. Synthesis runs
on its own cadence over the run path, and until it runs again, the store's derived layer is
a picture built partly from evidence that no longer exists.

The residue is not always the erased rows themselves. A summary, a belief, an entity profile
or a preference can encode what the removed evidence said without containing any of its
values, so the cascade correctly invalidates what it can identify and correctly leaves
standing what was shaped by the subject without citing them. Only a re-synthesis over the
post-erasure store produces a derived layer that is a function of the surviving evidence
alone.

The gap between the cascade completing and that re-synthesis is therefore a window in which
fact reads are answerable and answering them is wrong. The window has no natural end: nothing
in the erasure path schedules synthesis, and the next scheduled run may be a day away or may
be an entry an operator disabled.

Replicas inherit the problem. A replica consumes materialized snapshots read-only and would
otherwise keep serving derived facts built before the erasure, out of reach of any control
applied at the canonical store.

## Decision

`--fail-closed` writes a `{subject_hash, executed_at}` object at the store root once the
cascade completes. While that object stands, every fact read — recall, command line and tool
surface alike — is refused with `ErasureRestagingRequired`, which names the re-synthesis the
operator owes and the verb that clears it. The marker holds no rows and belongs to the set of
root objects a replica receives while a table is withheld, so a replica inherits the gate in
place of serving pre-erasure derived facts. `--clear-stale` removes the object after
synthesis has run over the post-erasure store, and the operator's attestation is what the
removal records.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A root marker that refuses every fact read until an attested re-synthesis** *(chosen)* | No derived fact built over erased evidence is served after the cascade, anywhere, including on replicas. The obligation is visible as an outage rather than as a log line. | Fact reads are down for the whole deployment between the cascade and the operator's attestation, and the gate is released by a human action rather than automatically. |
| Serve derived facts with a staleness warning attached | Availability is unbroken, and the caller is told. | Lost on whether the gate is a control or a notice: a warning in a response is not a control. The value is consumed, the warning is dropped by the first program in the chain, and the erasure did not take effect on the read path. |
| Invalidate silently and serve empty results | No refusal to handle, no new state, no operator action. | Lost on boundedness of the window: an empty answer reads as an absence of data rather than as a gate, so an operator sees a working store with nothing in it and no reason to re-synthesize. Nothing ever clears. |
| Defer the gate to the next scheduled synthesis | Self-clearing; no human in the loop. | Lost on boundedness of the window: the window is unbounded — it depends on an entry's cadence and on whether that entry is enabled at all — and the period of wrong answers is exactly what the gate exists to close. |
| Gate only the facts the cascade touched | Availability is preserved for unrelated reads. | Lost on whether pre-erasure derived state can still be served: the facts the cascade could identify are the ones already removed. The residue is precisely the set that cannot be enumerated, so a targeted gate covers everything except what it is for. |
| Block the erasure verb until synthesis can run inline | One operation, no intermediate state. | Lost on independence of the erasure deadline: synthesis is a run-path workload with model calls in it; binding an erasure's completion to it makes the deadline-bearing operation depend on the slowest, least reliable path in the system. |

## Criteria

1. **Whether pre-erasure derived state can still be served** — through any surface, on any
   machine. The warning, the empty result and the deferred gate all fail this.
2. **Whether the gate is a control or a notice** — a control changes what the caller
   receives; a notice changes what a careful caller might do about it.
3. **Whether a replica inherits it** — a copy that keeps answering defeats a gate held only
   at the canonical store.
4. **Boundedness of the window** — whether the period of wrong answers has an end the
   operator sets rather than one a cadence sets.
5. **Read availability** — whether unrelated fact reads keep working. This is the criterion
   the chosen option loses on, and it loses it completely.
6. **Independence of the erasure deadline** — whether completing an erasure depends on the
   run path's slowest and least reliable workload.

Fail-closed on derived state decides it over read availability. The two are in direct
opposition and only one of them can be satisfied; an erasure whose effect is advisory on the
read path is not an erasure, while an outage is loud, bounded by an action somebody is
already performing, and visible to exactly the operator who owes the work.

## Consequences

An erasure driven with `--fail-closed` makes the re-synthesis obligation unavoidable: the
deployment does not serve facts again until somebody attests that synthesis ran over the
post-erasure store, and that attestation is what the marker's removal records. The scope is
deliberately coarse — every fact read, every surface, every replica — so there is no path
around it to find.

The accepted cost is a self-inflicted outage of the fact surface across the whole deployment
for one subject's erasure, ended only by a human action. A deployment that runs erasures
frequently and synthesizes slowly will spend a material fraction of its time gated, and the
flag exists precisely so that a deployment can decline that trade and accept the residue
instead.

Reversing this is cheap in the direction of leniency and expensive in the other: a marker
already written must still be cleared by the verb, and any relaxation has to decide what to
do with gates standing at the time.

## Revisit triggers

- Synthesis becomes incremental over an evidence set, so a re-synthesis confined to the
  affected derivations is both possible and fast enough to run inside the erasure.
- The derived layer records provenance complete enough that residue-without-citation stops
  existing, which would make the cascade sufficient on its own.
- Deployments are observed running erasures without `--fail-closed` as a matter of routine,
  which would mean the gate's cost is being refused in practice rather than weighed.
- A per-subject gate becomes expressible, bounding the outage to the readers that would have
  seen the residue.
