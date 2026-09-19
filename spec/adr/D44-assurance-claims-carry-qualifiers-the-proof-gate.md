# D44 — Assurance claims carry their qualifiers, and the proof gate audits axioms

**Status:** accepted

## Context

An assurance sentence is quoted out of context — into a questionnaire, a customer's documentation, a sales deck. A proof checker exits zero on a hole, and a source-text scan misses a hole inside parentheses. The gap between a theorem's name and its statement is where an auditor relies unprotected.

## Decision

Every claim travels with its limits, and the gate decides on the elaborated environment of the commit's own source.

- `assurance.scope-claim` states three resolving lists: named authorization decisions, named specifications, stated translation and runtime assumptions. A claim wider than its decisions raises `ClaimBeyondNamedDecisions`; the at-rest guarantee appears only beside its qualifier.
- `assurance.prove` publishes each theorem with the statement it leaves open, in its inventory row. The composition theorem covers the one order the engine applies; a commutation claim refuses.
- `assurance.audit-axioms` matches a hand-maintained inventory of constants, statements and admitted axioms against each constant's transitive footprint, over a three-entry allowlist.
- `assurance.recheck` rebuilds from pinned source in a credential-free environment; the package declares no dependencies and pins its toolchain in the tree.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Inventory plus transitive axiom audit, qualified claims, zero-dependency package *(chosen)* | — | Every lemma is hand-rolled; the inventory is maintained by hand; the claim is narrower than a buyer would prefer. |
| A source pattern over package files | Catching a proof of `False` | A parenthesized hole or a respelled tactic would pass. |
| Build and read the exit status | Catching a proof of `False` | A hole elaborates as a warning and exits zero. |
| An inventory generated from the declarations | Independence | A weakened statement would carry its inventory with it. |
| Claiming the enforcement engine is verified | Checkability | The models define no credential bytes, SQL semantics or attacker observations. |

## Consequences

- A proof of `False`, a hole, a deleted theorem and a weakened statement each fail loudly.
- Two machines reach the same verdict on one commit, offline.
- A theorem needing real algebra or measure theory costs a dependency decision.

## Revisit

- A mathematics library elaborates inside the per-change budget.
