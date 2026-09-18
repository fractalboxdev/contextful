# 0311 — Recall is the one door a stored conclusion reaches an answer through

**Status:** accepted 2026-09-18
**Decides:** `console.plan-turn.refusal.planner-reached-memory`

## Context

A reading session accumulates conclusions the next session reads. Once an answer has
streamed, a distillation pass writes durable, plain-business-language entries shaped
`{subject, key, learning}`, each carrying the asking question as evidence. The engine
consolidates each one, stamps its provenance and decides its standing; the surface asserts
no standing of its own.

Those conclusions come back through recall. A turn opens by recalling the store's standing
conclusions at the session's vantage, ahead of the planner, and recall applies the checks
that make a conclusion safe to use: it is bounded by two clocks, so a belief anchored to a
past snapshot does not frame a latest-data answer; it is ordered by tier; and it carries the
evidence that produced it. Recalled conclusions enter the system text and the synthesis
grounding as their own labelled block.

Underneath, conclusions are rows in relations like any other. The planner receives each data
table's real columns and writes statements against those names, and the code path under a
failed search matches the question's tokens against whole rows. Either of those reaches the
memory relations if nothing stops it. A conclusion pulled in that way arrives with none of
the checks attached — no liveness, no tier ordering, no evidence gate — and it arrives
looking exactly like a source row, because in the relation it is one.

A distilled belief is not a source. It is the workspace's own conclusion about sources,
and treating it as one lets a conclusion cite itself into permanence.

## Decision

Planner scaffolding and every deterministic fallback cover data tables. Scaffolding naming a
memory relation raises `ConsolePlannerReachedMemory`. A conclusion reaches an answer through
the recall step, and no free-text path over the conclusion mirror exists on this surface.
The chip generator drops the same relations, so a reader is not invited to query them
either.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Recall is the only door; the planner and the code path cover data tables alone** *(chosen)* | Every conclusion entering an answer has passed liveness, tier ordering and its evidence gate, and it arrives labelled as a conclusion rather than as a row. | A question whose answer genuinely lives in a stored conclusion and nowhere else depends on recall having surfaced it, so a recall that returns the wrong subject cannot be rescued by the planner. |
| Expose memory tables to the planner with a liveness predicate documented in the prompt | The planner reaches everything the store holds; one prompt paragraph is the whole implementation. | Lost on the checks: a predicate the model is asked to write is a predicate it can omit, and the omission produces a plausible answer built on a retired belief with nothing marking it. |
| Expose a read-only view of live conclusions as an ordinary table | Liveness is enforced in the view rather than trusted to the model; the planner gets real reach. | Lost on distinguishability: a distilled belief then cites like a source and ranks against one, so the workspace's own conclusion competes with the evidence it was drawn from and can outrank it. |
| Let the code path scan memory when data tables come back empty | Rescues exactly the case the chosen option leaves unserved, and only that case. | Lost on the checks again: the code path applies no tier ordering and no evidence gate, so the rescue is the one path with the fewest guarantees, firing at the moment the answer rests entirely on it. |

## Criteria

1. **Whether a conclusion can enter an answer without its liveness, tier and evidence
   checks** — that is, whether the guarantees recall carries are guarantees or defaults.
   **This criterion decided.** A retired or superseded belief presented as current is worse
   than no answer, because it reads as the workspace's settled position; every rejected
   option admits that case through some path.
2. **Whether the reader can tell a conclusion from a source row** — a belief that cites like
   a source becomes self-reinforcing evidence.
3. **Planner reach over a store's real content** — how much of what a store holds the planner
   can write statements against. This is the criterion the decision concedes.
4. **Where the rule is enforced** — in code at a fixed point, or in text the model is asked
   to honor.

## Consequences

The memory contract has one consumer on this surface, so its bounds, its ordering and its
provenance stamping are exercised on every path that uses it and on no path that does not.
Conclusions render as their own labelled block, which is what lets a reader weigh them
differently from a row. Changing how conclusions are selected is a change in one place.

The cost accepted: the recall step is a single point of retrieval for everything the
workspace believes. A question whose answer lives only in a stored conclusion is answered
only if recall surfaced that subject, and the planner — which does see the question's actual
wording — cannot reach for it. A subject-matching miss at recall therefore reads to the
reader as the store holding nothing, and the plain-emptiness answer is indistinguishable
from the truthful one.

## Revisit triggers

- Recall misses are observed producing empty answers on questions whose conclusions are
  demonstrably stored, meaning subject matching is the binding constraint.
- The memory contract grows a retrieval form that carries its checks with it and could be
  offered to the planner intact.
- Conclusions become distinguishable from source rows at every point a reader sees them,
  which would remove the ground the read-only-view option lost on.
