# 0323 — A generated artifact carries an enforced staleness check; a hand-written constant mirroring an engine constant is refused

**Status:** accepted 2026-09-18
**Decides:** `build.structure-tree.refusal.mirrored-constant`

## Context

A surface off the engine's request path cannot always call for what it needs. It holds a
type, a schema, a limit or a table of names that the engine also holds, and something has
to put that content on the surface's side. Two shapes do this and they are easy to confuse
at a glance, because the file that lands looks the same either way.

A derived artifact is produced from the engine's definition by a generator, and the
question is whether anything notices when the definition moves. If the generator runs on
demand and nobody re-runs it, the artifact is a snapshot with a generator's provenance —
indistinguishable from a hand copy in every way that matters, and slightly worse, because
its header claims it is current.

A hand-written constant mirroring an engine constant has the same failure with none of the
machinery. One number is written twice, in two languages, and the second copy is correct
until the first changes. Nothing connects them, so the divergence is discovered by a reader
noticing two different numbers, usually while debugging something else.

Drift detectability is therefore the property that separates these, and it is a property of
the gate rather than of the artifact. A derivation whose staleness reds the gate fails on
the next change that touches its source — loudly, at the moment the cause is on screen. A
derivation without that check fails whenever someone happens to look.

There is one duplicate worth keeping, and it is worth naming so it is not mistaken for the
shape being refused. A review-time check over committed text can mirror a build-time verb,
catching the same mistake earlier — at review rather than at build — which is the reason it
exists and not an oversight.

## Decision

A generated artifact declares the check that fails once its source moves, and that check
runs inside the gate; derivation without an enforced staleness check is a copy with extra
steps. A hand-written constant mirroring an engine constant raises `ContractRestated`,
naming both sites. One duplicate stands: a review-time check over committed text mirroring
a build-time verb, which states at its own site which verb it mirrors.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Derivation permitted, with an enforced staleness check; a mirrored constant refused** *(chosen)* | Drift in a derived artifact reds the gate on the next change to its source, and the one legitimate path onto a surface stays open. | Every derived artifact adds a gate step and a regeneration path to maintain, and the schema stage grows with the artifact count. |
| Allowing an uninstrumented derived artifact | No gate step, no regeneration path, and a generator's provenance in the header. | Lost on detectability: it drifts exactly as a hand copy does, and the generated header makes the staleness harder to suspect than an obviously hand-written file. |
| Forbidding derivation entirely | One rule with no exceptions, and no generated artifacts to keep current. | Lost on reachability: a surface off the engine's request path has no other route to the contract, so the rule would force a hand copy — the worse of the two shapes — or block the surface. |
| Checking staleness in a scheduled run rather than in the gate | Keeps the gate fast, and still catches drift eventually. | Lost on detectability at the moment it matters: the failure arrives detached from the change that caused it, on a run nobody is watching, which is most of the distance back to no check at all. |
| Refusing the review-time check that mirrors a build-time verb | Absolute consistency: no duplicate anywhere, no exception to explain. | Lost on when the mistake is caught. The duplicate exists to fail at review rather than at build, and removing it costs that earlier signal for a rule that is already stated. |

## Criteria

1. **Whether drift is detectable at review time** — whether a divergence surfaces on the
   change that causes it. *This criterion decided it.* Both a derived artifact and a hand
   copy are correct the day they land, so their whole difference is what happens on the day
   the source moves; a mechanism that does not answer that question is the copy it was
   introduced to replace.
2. **Reachability for a surface off the engine's request path** — whether the rule leaves a
   legitimate route.
3. **Whether the mistake is caught earlier or later** — the criterion the one admitted
   duplicate is kept on.
4. **Maintenance cost per artifact** — the gate step and the regeneration path. The
   accepted cost, outranked by detectability.

## Consequences

Review has a concrete test rather than a judgement: a derived artifact either names an
enforced check or is a copy. A number that exists in two languages is refused outright,
with both sites named, so the fix is regeneration or a call rather than an agreement to
keep them in step.

The accepted cost grows with the artifact count. Every derived artifact adds a gate step
and a regeneration path someone maintains, and the schema stage of the gate lengthens as
more surfaces derive. That is a per-artifact tax a hand copy does not pay, charged
deliberately to keep the copy from being the cheap option.

## Revisit triggers

- Staleness checks accumulate to where the gate stage's own duration is the constraint,
  forcing a cheaper check shape such as a single schema hash over all derived artifacts.
- A derivation is found whose source has no stable identity to hash or regenerate against,
  which leaves detectability unreachable and re-opens what the rule permits.
- The review-time duplicate is observed never to catch anything the build-time verb misses,
  which would remove the exception's justification.
