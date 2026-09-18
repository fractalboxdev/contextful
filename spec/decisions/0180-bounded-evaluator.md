# 0180 — The evaluator runs a bounded fragment, and input past a ceiling refuses

**Status:** accepted 2026-09-18
**Decides:** `authority.profile.refusal.evaluator-bound`

## Context

Admission runs the evaluator on every request, at every checkpoint, before anything else
happens. Its input is a credential, which is attacker-supplied and which any holder may
have appended blocks to offline. The evaluator is therefore an expression interpreter
fed by whoever is knocking, sitting in front of every read the engine serves.

The underlying language offers more than the engine needs. Third-party blocks let a
credential reference authority signed by a party the checkpoint would have to go and
reach. External functions let it call out of the evaluator entirely. Recursion and
regular-expression predicates let a small credential describe a large amount of work —
a few hundred bytes of rules that expand into an evaluation nobody sized, on the request
path, before admission has decided anything.

Two properties are at stake and they are separate. One is cost: evaluation has to finish,
in a time that does not depend on how clever the credential is. The other is determinism:
the same credential has to admit or refuse identically at a gateway and at the engine,
which is what lets one pinned profile module serve both, compiled natively and to
WebAssembly. A bound expressed as elapsed time satisfies neither — it depends on the
hardware, so two checkpoints disagree, and it converts an expensive credential into a
slow request rather than a refused one.

## Decision

The evaluator runs with no third-party block, no external function, no recursion and no
regular-expression predicate, under declared authorizer fact and iteration ceilings. Input
past a ceiling raises `ProfileEvaluationBudget` rather than being evaluated to completion.
The ceilings are counts the profile declares, so every checkpoint reaching the same
credential reaches the same verdict.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A restricted fragment under declared fact and iteration ceilings** *(chosen)* | Evaluation cost is bounded by counted work rather than by elapsed time, so a gateway and the engine decide identically | A restriction wanting a pattern is written as an enumerated allowlist, which is verbose and is re-minted when a table is added |
| The full language under a wall-clock timeout | Every upstream feature available; one simple bound to implement | Lost on determinism: the same credential admits on fast hardware and refuses on slow, so two checkpoints disagree, and an expensive credential becomes a slow request rather than a refusal |
| Regex predicates on a backtracking-free engine | Pattern matching on table names, with linear-time matching | Lost on determinism across runtimes: the profile module compiles natively and to WebAssembly, so the two would have to carry the same matcher with identical semantics — the engine choice stops being this engine's to make |
| Permit third-party blocks | Expresses delegation across organizations, which the format supports natively | Lost on read-path independence: verifying a third-party block needs key material from a party the checkpoint would have to reach, which puts a network call on the admission path |
| Permit recursion under a depth ceiling | Expressive rules, bounded by a number like the other ceilings | Lost on predictability of the bound: depth does not bound work, since breadth at each level does, so the number that would have to be declared is one nobody can set from the credential's shape |

## Criteria

1. **Determinism across checkpoints** — whether a gateway and the engine reach the same
   verdict on the same credential.
2. **Bounded cost on the request path** — whether evaluation work is bounded by something
   a credential cannot inflate.
3. **Read-path independence** — whether evaluating a credential requires reaching anyone.
4. **Expressiveness of a restriction** — what a grant can say about which rows and tables
   it reaches.

Criterion 1 decided it, over criterion 4. Bounded cost alone would have been satisfied by
a timeout, which is by far the cheapest option here and covers the denial-of-service case
completely. Determinism is what rules it out: where both a gateway and the engine decide
the same question, they execute one pinned profile module rather than two verifiers, and
a bound that differs by hardware makes that module's answer a property of where it ran.
A credential that admits at the gateway and refuses at the engine is an outage; one that
admits at the engine and refuses at the gateway is a gate nobody can rely on.

## Consequences

Admission cost is a function of the credential's counted shape, so the worst case is
reachable by reading the profile's ceilings rather than by measurement. One profile
module serves the gateway and the engine, which is why the two cannot drift. The absence
of external functions and third-party blocks also means evaluation is a pure function of
the credential and the engine-supplied facts, which is what makes the mapping onto
effective authority modelable at all.

The cost accepted is expressiveness. A restriction that would naturally read as a pattern
over table names is written as an enumerated allowlist, so adding a table means minting
new credentials rather than matching a prefix — and for a deployment whose tables are
created by its own pipelines, that is recurring work rather than a one-time cost.

The headroom the ceilings carry is unmeasured. They are declared numbers, and what a real
credential's fact count and iteration count reach at the maximum delegation depth the
engine admits is unknown; whether they sit comfortably above that or narrowly above it has
not been established.

## Revisit triggers

- A legitimate credential at real delegation depth approaches either declared ceiling,
  which would mean the numbers were set without the shape they need to cover.
- Enumerated table allowlists are observed being re-minted frequently enough that the
  expressiveness cost outweighs the determinism criterion 1 buys.
- A pattern matcher with identical semantics guaranteed across native and WebAssembly
  compilation becomes available, which would satisfy criterion 1 for regex predicates.
- Cross-organization delegation becomes a requirement, which weighs third-party blocks
  against criterion 3.
