# 0321 — An executable reference model differentially tested against the engine's decision functions is the standing link, with refinement scoped to the pure decision functions

**Status:** accepted 2026-09-18
**Decides:** `formal.scope-claim.refusal.refinement`, `formal.differential-test.refusal.disagreement`, `formal.differential-test.refusal.discarded-counterexample`

## Context

A theorem about a model says something about the model. The binary anyone runs is a
different artifact, and something has to connect them or the proofs are a parallel
literature. Two mechanisms are available and they fail in opposite directions.

Refinement — translating the engine's decision functions into the proof language and
proving the translation equivalent — yields the stronger statement. Its trust chain is also
four links long: the compiler's lowering, the translator itself, the hand-written models
standing in for external definitions, and the production build configuration. The
translator is unpinned and is itself a trusted dependency. When one of those links is
wrong, the refinement proof does not fail; it succeeds about the wrong object, and there is
no artifact left behind to inspect.

Differential testing — driving an executable rendering of the same decision functions and
the engine's own functions over the same generated case, and comparing field by field —
yields the weaker statement and leaves evidence. It depends on no external translation
toolchain, so its evidence holds whether or not anything is translated. What it cannot do
is rule out a case the generator never emits, which is a limit that has to be stated rather
than hoped over.

Where a translation-based statement is worth having, its reach is also a decision. The
property being established lives in the pure decision functions — inputs are values,
outputs are decisions, nothing is read and nothing is written. Cryptography, parsing
adapters, database calls and concurrency have no equivalent statement available: modelling
them enlarges the trusted hand-written remainder and produces no guarantee at the end of it.

A disagreement, when one appears, is the most valuable output the system produces. It is
also the easiest to lose — reported on a console, fixed, and never replayed, so the same
class of divergence returns under a different seed.

## Decision

An executable reference model, driven against the engine's decision functions over the same
generated case, is the standing link. A case on which the two decisions differ is shrunk
until removing any further field makes them agree, and raises `ReferenceModelDrift`
printing the minimized case and both decisions; a run reporting a disagreement and writing
no corpus entry raises `CounterexampleDiscarded`. Every minimized case replays ahead of
freshly generated cases on each later invocation. Translation covers the pure decision
functions — inclusion and grant narrowing — and reaching cryptography, a parsing adapter, a
database call or concurrency raises `RefinementScopeExceeded`, naming the module.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A differentially tested executable reference model, with refinement scoped to the pure decision functions** *(chosen)* | Evidence about running code that depends on no unpinned toolchain, a minimized case whenever the two disagree, and a corpus that keeps every past divergence in the suite. | The harness produces evidence about running code, not an equivalence proof, and it cannot rule out a case the generator never emits. Two artifacts — the reference model and the engine's decision functions — are kept in step. |
| Refinement proof alone | The strongest available statement: the code and the specification are the same object, for every input rather than for sampled ones. | Lost on survivability of the evidence. Its trust chain includes a translator that is itself trusted and unpinned, so a failure there leaves no artifact at all — the proof passes about the wrong object and nothing is written down. |
| Nothing but the model | No harness, no reference binary, no corpus, no second artifact to keep in step. | Lost on relevance: a theorem about a model says nothing about the binary anyone runs, which is the entire gap the link exists to close. |
| Translating cryptography, parsing adapters, database calls and concurrency | A translated statement over the whole request path rather than over its decision core. | Lost on yield: the pure decision functions are where the property lives, and the rest enlarges the hand-written model set without producing a guarantee about anything. |
| Reporting a disagreement without retaining it | A shorter run and no corpus to bound or evict from. | Lost on regression: a divergence fixed and not replayed returns under a different seed, and the run that would have caught it is the one that stopped keeping cases. |

## Criteria

1. **What the evidence survives** — whether a failure in the mechanism leaves something a
   reader can inspect. *This criterion decided it.* Assurance is only worth what remains
   when a component is wrong, and a refinement proof whose translator is broken produces a
   green result and no artifact, while a differential run produces a minimized case that is
   informative whichever side is at fault. The stronger statement is worth less than the
   one that fails legibly.
2. **Relevance to the running binary** — whether the statement reaches the artifact that
   executes.
3. **Yield per unit of modelling** — whether extending the translated surface adds a
   guarantee or only a trusted model.
4. **Regression retention** — whether a past divergence stays in the suite.
5. **Strength of the statement** — whether the claim is for every input or for sampled
   ones. Genuinely stronger under refinement, and outranked by survivability.

## Consequences

The harness stands on its own: its evidence holds independently of whether the pure
decision functions are ever translated, and it records the generator seed so a case
sequence reproduces exactly. The generator emits malformed inputs, boundary values and
well-formed successful requests in the same run, since a generator producing successes
alone exercises one third of the surface. A change to one artifact alone surfaces as a
disagreement on the next run rather than as a silent divergence.

The accepted cost is honest weakness plus maintenance. The report states the case classes
it drew from, because a case outside them falls outside what any green run covers. The
reference model and the engine's decision functions are two implementations of the same
decisions that have to move together, which is real duplicated work — and is also exactly
what makes a unilateral change visible.

## Revisit triggers

- A pinned, reproducible translation toolchain becomes available, which removes the trust
  link that decided against refinement.
- A divergence class is found in production that the generator's three case classes could
  not have produced, which is evidence the case space is mis-specified rather than
  under-sampled.
- Keeping the two artifacts in step is observed to cost more than the divergences it
  catches, which would argue for generating the reference model from the engine's source
  and accepting the weaker independence.
