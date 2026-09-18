# 0307 — A turn with no tool result answers that the store holds nothing rather than answering from the model

**Status:** accepted 2026-09-18
**Decides:** `console.ground.refusal.ungrounded-answer`

## Context

Every sentence of an answer on this surface is built from what a tool call returned. The
model's own knowledge grounds nothing, each round-trip renders as a step a reader can open,
and the turn closes with a numbered source list derived from that turn's own grounding.
That is the whole proposition: a reader is looking at what their workspace holds, not at
what a general assistant recalls.

A turn can nonetheless end with nothing. The planner writes statements against the store's
real columns and they return no rows; a replanning round produces nothing usable; the code
path matches the question's content tokens against whole rows and finds none. A store that
genuinely holds nothing on a subject reaches this state on every question about that
subject, and an empty store reaches it on every question at all.

At that moment the model is still present and entirely capable of composing a fluent,
plausible paragraph about the subject from its own training. That paragraph would be
indistinguishable in register, confidence and citation shape from a grounded one. The
reader has no way to tell, and neither does a screenshot of the reader's screen, which is
the form these answers travel in.

The surface also has no error to raise here. An empty store is a normal state, not a fault —
a deployment is expected to start empty and fill.

## Decision

A turn holding no tool result answers that the store carries nothing on the question. Any
path that would compose prose from the model alone raises `ConsoleUngroundedAnswer`. The
plain emptiness is the answer, and the code path's exhaustive match over whole rows removes
a degraded search from the set of explanations behind it.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Answer that the store holds nothing; refuse any model-composed prose** *(chosen)* | Every sentence a reader sees came from their data, so the distinction between a stored fact and a recollection needs no attention from the reader. The refusal is exercisable with no live model. | A reader with a genuinely thin store gets a short answer where a general assistant would have produced a long plausible one, so the surface reads as less capable on exactly those questions. |
| Let the model answer from its own knowledge under a disclaimer | The reader gets something useful rather than nothing, and the disclaimer marks the difference. | Lost on distinguishability: a disclaimer travels less far than the sentence it qualifies. The paragraph gets quoted, pasted and screenshotted; the qualifier does not, and neither does the reader's memory of it a week later. |
| Return an error page on an empty turn | Unambiguous; no prose to misread. | Lost on the normality of the state: an empty store is not a fault, and treating the ordinary opening condition of every new deployment as an error makes the surface unusable during exactly the period an operator is filling it. |
| Fall back to the public-web leg unconditionally | Covers the gap with real, citable material rather than recollection. | Lost on distinguishability for a store whose operator configured no search key — there is no leg to fall back to, so the gap remains for exactly the deployments most likely to hit it — and it silently changes which sources ground the turn without the reader having asked for that. |

## Criteria

1. **Whether a reader can tell a stored fact from a model recollection** — with no special
   attention, and in whatever form the answer travels. **This criterion decided.** The
   surface's value is that its output is attributable to the workspace; an answer that might
   not be removes that property from every answer, not only the ungrounded one, because the
   reader can no longer assume it.
2. **Usefulness on a thin store** — whether the surface stays workable while a store is
   being filled. Plain emptiness is workable; an error page is not.
3. **Testability without a live model** — whether the behavior can be exercised
   deterministically. A refusal on the composition path can be; a disclaimer's adequacy
   cannot.
4. **Perceived capability against a general assistant** — this points the other way and is
   the accepted cost.

## Consequences

Every answer the surface emits is attributable, so a reader can treat the absence of a claim
as information: if the console does not say it, the store does not hold it. The sources
block is always derivable, since there is never an answer with nothing behind it. The code
path's role sharpens — it exists so that "nothing found" means the store is empty on the
subject rather than that the search was weak.

The cost accepted: on a thin store the surface is visibly less capable than a general
assistant, and a reader who compares the two will notice. That comparison is worst at the
start of a deployment and improves as the store fills, which is the opposite of the curve a
product would choose. The plain emptiness also gives a reader no next step beyond asking
something else.

## Revisit triggers

- Readers routinely take an empty answer as a fault of the product rather than as a fact
  about the store, indicating the copy is not carrying the distinction.
- A grounding source becomes available to every deployment by default, so the fallback's
  coverage gap closes.
- Provenance marking becomes strong enough to survive quoting and screenshotting, which
  would remove the reason the disclaimer option lost.
