# 0316 — The formal package declares no dependencies and pins its own toolchain

**Status:** accepted 2026-09-18
**Decides:** `formal.model.refusal.declared-dependency`, `formal.model.refusal.toolchain-drift`

## Context

A mechanized model stands beside the policy core: one Lean package, a set of theorems over
it, and a gate that reads axiom dependencies off the elaborated proofs. The gate runs on
every change. That cadence is the whole point — a proof check that runs weekly tells a
reader which week a claim was true, and a check that runs per change tells them the tree in
front of them holds.

Per-change cadence is a cost question. A full elaboration from cold has to complete inside
a per-change budget, which the corpus fixes at 60 s of wall time and 512 KiB of artifacts,
and a recheck from pinned source has to fit a second elaboration inside 600 s. A package
that fetches and builds a general mathematics library does not come close to either
number: the library's own elaboration dominates by orders of magnitude, and caching it
moves the cost rather than removing it, at the price of making a verdict depend on what a
cache held.

What the theorems actually range over is small. A layer is a total function from a row
identifier to a boolean; composition is a conjunctive fold over a list; an allow-set is a
decidable predicate over a placement value; the floor is a pointwise intersection. The
proofs are inductions over lists and case analyses over finite enumerations whose decidable
equality derives. The lemmas a library would supply are the ones these proofs write in a
few lines each.

The second half of the problem is what decides the toolchain. A verdict that depends on
which build image ran it is not a verdict about the commit. Two people checking the same
tree, and the same person checking it twice a month apart, need the same elaborator, and
the elaborator's behavior on a proof is not stable across releases.

## Decision

The package declares zero dependencies: a manifest with no `require` stanza and one library
target, and a stanza reaching any external library raises `FormalPackageDependency`, naming
it. The toolchain comes from a file at the package root naming one exact release string,
which the version manager resolves rather than taking whatever the build image carries; a
build whose resolved string differs from the pin raises `ProofToolchainDrift`, printing
both. Elaboration from a bare checkout fetches nothing and opens no outbound connection.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Zero dependencies, toolchain pinned in the tree** *(chosen)* | A full elaboration fits the per-change budget from cold, the recheck needs no network at all, and two machines reach the same verdict on the same commit. | Every lemma is hand-rolled. A proof needing real algebra or measure theory re-opens both the dependency question and the gate's position. |
| Depending on a general mathematics library | Standard lemmas, idiomatic proofs, and a much shorter path for any future theorem outside list folds and finite case analysis. | Lost on build cost inside a per-change gate: the library's elaboration alone exceeds the budget, so the check moves to a cadence of its own. What it would add to the present theorems is small — they need list induction and decidable equality on finite enumerations. |
| A vendored subset of such a library in the tree | The lemmas that are actually used, with no fetch and no resolution step. | Lost on cost again, at a smaller multiple, and on maintenance: a vendored subset is a fork nobody upstreams, and its provenance has to be audited exactly as the rest of the package is. |
| Taking the toolchain from the build image | One less file, and the toolchain moves when the platform moves. | Lost on reproducibility: a verdict then depends on which image ran it, so a red gate and a green local run can both be correct and neither is about the commit. |

## Criteria

1. **Elaboration cost from cold against the per-change budget** — whether the full check
   fits 60 s and 512 KiB. *This criterion decided it.* Cadence is what the proofs are for:
   a check that cannot run on every change stops being a gate and becomes a report, and no
   convenience in writing the proofs compensates for that. Declaring a dependency is
   therefore not only a build-time cost but a relocation of the check itself.
2. **Reproducibility of a verdict across machines** — whether the same commit yields the
   same result anywhere. Decided by the toolchain pin, and the reason the pin is a file in
   the tree rather than a property of the environment.
3. **Proof-writing effort** — how much a library would save. Real, and outranked, given
   what the present theorems need.
4. **Network reach of the check** — whether the environment has to be trusted to fetch
   correctly. With nothing to fetch, the absence of outbound traffic is a property of the
   package rather than a policy applied to the environment.

## Consequences

The check stays per-change and credential-free, and the recheck's no-network property costs
nothing to enforce. A bare checkout elaborates, so a reader reproducing a verdict needs the
tree and the pinned release and nothing else.

The accepted cost is that every lemma is written here. A theorem that needs real algebra,
measure theory or a nontrivial order theory has no cheap path: it re-opens the dependency
question, and with it the gate's position, since declaring a dependency changes the
elaboration cost and moves the check to its own cadence. Bumping the toolchain is a
deliberate edit that may break proofs, rather than something that happens quietly when an
image is rebuilt.

## Revisit triggers

- A required theorem's proof needs mathematics outside list folds and decidable finite case
  analysis, making the hand-rolled path longer than the elaboration it saves.
- Cold elaboration approaches the per-change budget from the package's own growth, which
  forces a cadence decision without any dependency being declared.
- A toolchain release changes elaboration behavior on the existing proofs, which is exactly
  what the pin is there to surface and what a bump has to be judged against.
