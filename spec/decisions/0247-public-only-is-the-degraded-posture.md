# 0247 — The one availability posture past the budget serves re-probed public rows, and closes when the probe ages

**Status:** accepted 2026-09-18
**Decides:** `visibility.bound-staleness.refusal.aged-public-probe`

## Context

Past its budget, a bound table refuses. That is the right default and it is an expensive
one: a source whose sweep is limping takes the whole corpus offline for every reader, and
the operator's only levers are to repair the sweep or to loosen a number they declared
because they could defend it.

There is a real asymmetry inside the permission state that makes a middle path possible.
The expensive thing to keep fresh is the grant graph — every resource, every principal,
every level, every group edge — and the expensive thing is expensive because it is large.
Whether one resource is public is a single field, and re-reading that field for a bounded
set of resources is a request pattern that keeps working long after a full grant-graph
sweep has stopped completing inside its cadence.

The trap is in what "public" means across time. A resource marked public at the last
completed full run may have been made private since. Serving it on the strength of that
stale mark is not a narrowing at all — it is a stale allow with a reassuring name, and it
is precisely the failure mode the budget exists to stop. The only version of this posture
that narrows rather than exempts is one where the public status itself was re-read inside
the budget, which makes the posture a second, cheaper sweep rather than a bypass of the
first.

That second sweep can age too. When it does, the posture has nothing current to stand on,
and the question is whether it falls back to the refusal or keeps serving on its own
stale data — which is the original mistake, one level down.

## Decision

`on_stale` takes `refuse` or `public_only` and defaults to `refuse`, declared per table
beside the budget. Past the budget, `public_only` serves rows whose resource had its
public status re-read within budget by a single-field probe, marking the envelope and the
audit record as degraded. The posture removes rows from what a fresh read would return and
adds none. Where the probe itself sits past the budget, the narrowing posture closes and
the read raises `VisibilityAccessStale`. The opened path is never wider than the closed
one. Results produced under the posture are not retained.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Serve only rows whose public status a cheap probe re-read within budget, disclose the degradation, and close when the probe ages** *(chosen)* | A source stays partially answerable through a sweep outage on strictly current evidence, and the degraded path has a floor it cannot fall through. | A second sweep shape per source to build and operate, and a degraded answer a reader has to interpret. The default stays the refusal precisely because the degraded path is easy to leave switched on. |
| Serve all public-marked rows past the budget without a probe | No second sweep to build; the posture is a filter over data already mirrored, available on every source immediately. | Loses on whether the escape hatch narrows or exempts: a resource public at the last full run can have been made private since, so the path serves a stale allow under a name that suggests it cannot. |
| No availability posture at all — refuse and repair the sweep | One behavior past the budget, nothing to configure, nothing to leave switched on, and no degraded answers anywhere. | Loses on operability: a single-field probe keeps running when a full grant-graph sweep cannot, so a total outage is accepted in a situation where a narrow, current answer was available. |
| Keep serving on an aged probe once the posture is open | The posture survives a probe outage too, so availability never collapses. | Loses on the same criterion as serving unprobed rows, one level down: the opened path would then be wider than the closed one, which is the property that makes the posture defensible at all. |
| A per-request freshness override the reader supplies | Readers who need availability choose it; readers who need assurance keep the refusal. | Loses on where the decision belongs: the tolerable age of authorization over a corpus is the operator's declared posture, and a reader choosing to see more is the one party with an interest in the answer. |
| Cache results produced under the posture | The degraded path performs like the normal one. | Loses on lifetime: a narrowed snapshot of a moment would outlive the condition that justified it, and the posture's rows are the ones whose justification expires fastest. |

## Criteria

1. **Whether the escape hatch narrows what has to stay fresh or exempts it from staying
   fresh** — whether every served row rests on evidence inside the budget. *This criterion
   decides.* An availability posture that serves on stale evidence is not a weaker version
   of the guarantee; it is the absence of the guarantee, available under a name that reads
   as safe, and it would be reached for in exactly the situations where the mirror is
   least trustworthy.
2. **Operability under a degraded sweep** — whether a cheaper observation is available
   when the expensive one is not.
3. **Whether the degraded path can be mistaken for the normal one** — disclosure on the
   envelope and in the audit record.
4. **Cost to build and operate per source** — the second sweep shape.

## Consequences

An operator gains a real choice per table: assurance by default, availability where the
corpus and the audience make a public-only answer worth having. Both settings are visible
in the manifest and the degraded one is disclosed on every answer it touches, so a reader
is never silently on the narrow path.

The accepted cost is a second sweep shape per source, with its own cadence, its own
credential and its own failure modes, plus a degraded answer that a reader has to
interpret — one that is narrower than the truth while looking like an ordinary result
apart from its marking. The default stays the refusal precisely because this path is easy
to switch on during an incident and easy to forget afterward, and because it pays full
cost on every request, which keeps it uncomfortable.

Reversing toward serving unprobed public rows is cheap in code and removes the property
that makes the posture defensible, with no per-read signal that anything changed.

## Revisit triggers

- The probe's own sustainable cadence is measured and lands far enough below the full
  sweep's that a separate, tighter budget for the probe becomes worth declaring.
- A source is found whose public marking is enforced at read time by the source itself,
  making the mirrored mark self-verifying and the probe redundant for that source.
- Deployments are observed running with the posture switched on for long periods rather
  than through incidents, which would mean the default is being routed around rather than
  used.
