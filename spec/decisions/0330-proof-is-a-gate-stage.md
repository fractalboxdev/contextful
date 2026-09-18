# 0330 — The proof stage type-checks the formal package ahead of the build and refuses a hole

**Status:** accepted 2026-09-18
**Decides:** `build.gate.refusal.formal-hole`

## Context

The tree carries a formal package whose theorems state properties the implementation is
built against. A theorem is only evidence if it is proved. A proof assistant accepts a
declaration whose proof is a hole — a placeholder standing in for an argument nobody has
written — so a package can be complete in shape, fully elaborated, and prove nothing.

That is the property that makes elaboration a misleading verdict. "The formal package
builds" is a statement about syntax and type formation. A package full of holes builds. If
that is what the gate checks, every theorem in the tree is cited as evidence for behavior
that has no argument behind it, and nothing in the run distinguishes a finished proof from
a stated one.

The placement question follows from what a hole is. A hole is a fact about the source, not
about the run: it is present in the file before anything executes, and no amount of
compilation changes whether it is there. So the check is answerable at the moment the
sources are available, which is before the workspace build. Placed after, a change carrying
a hole spends a full engine build — the most expensive stage in the run — before reaching
the cheapest check that could have refused it.

The proof toolchain is not the toolchain the rest of the tree uses, which is the cost side
of every arrangement here.

## Decision

The proof stage type-checks the formal package and raises `ProofHolePresent` ahead of the
build, naming the declaration that carries the hole. A hole is a fact about the source
rather than a fact about the run, and successful elaboration is no evidence a theorem is
proved.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A dedicated stage type-checking for holes, placed ahead of the build** *(chosen)* | The cheapest refusable condition is found first, costing no compilation. A theorem in the tree is a proved theorem. | The gate carries a stage whose toolchain differs from the rest of the tree, and a proof-package change reds a run that touched no Rust. |
| Running the proof check after the workspace build | Groups the slow toolchain-heavy work together; a contributor sees compile errors first, which are the more common failure. | Lost on cost: a source refusable without any compilation spends the run's most expensive stage before being refused. |
| Treating successful elaboration as the verdict | No extra check to write; the proof assistant's own exit code is the answer. | Lost on what it measures: a package full of holes elaborates, so the verdict is about syntax and reports proof. |
| Checking holes only at release | Zero per-change cost, and a release is where the evidence claim actually matters. | Lost on drift window: holes accumulate across a release cycle with no change to attribute each to, and the release then blocks on a backlog of unwritten arguments. |
| Refusing a hole by review convention rather than by a check | No toolchain in the gate at all; reviewers already read the proofs. | Lost on enforceability: a hole is a token in a file that reads as ordinary syntax, and a reviewer scanning a large package has no signal distinguishing the declaration that carries one. |

## Criteria

1. **Placement cost** — how much work a run performs before it can refuse a condition that
   was refusable from the source alone.
2. **What the check measures** — whether the verdict is about proof or about syntax.
3. **Drift window** — how long a hole can sit in the tree before something names it.
4. **Toolchain count in the gate** — how many distinct toolchains a run installs.

**Placement decides it.** The measurement criterion rules out elaboration-as-verdict on its
own, but among the arrangements that do check for holes, placement is what separates them:
a hole is knowable from the source, so any ordering that compiles first is paying for
information it already had. Toolchain count is the criterion the chosen option loses on,
and it is accepted.

## Consequences

A theorem the tree cites is a theorem with an argument behind it, and the citation is worth
what it claims. A change that introduces a hole is refused in the first minutes of a run,
naming the declaration, so the feedback is specific and immediate.

The cost accepted: the gate installs and runs a toolchain that nothing else in the tree
uses, which is setup cost in the container image and a second ecosystem for a contributor
to understand. And the coupling runs both ways — a proof-package change reds a run that
touched no Rust at all, so a contributor working only on proofs is gated by the same
pipeline as everyone else, and a contributor working only on Rust still pays for the proof
stage's presence in the image.

Reversing this is cheap: the stage is a stage, and moving or removing it is a scheduling
change. What is expensive to reverse is the claim — once theorems have been cited as
evidence in decision records and pins, weakening the check retroactively weakens every
citation that was made while it held.

## Revisit triggers

- The proof toolchain's presence in the container image becomes a material share of image
  size or setup time against the run's ceilings.
- The formal package grows large enough that type-checking it is no longer the cheap stage
  this placement assumes, at which point the ordering argument is about two expensive
  stages rather than a cheap one and an expensive one.
- A stronger per-theorem verdict becomes available — an audit over the axioms an elaborated
  proof reaches — making the hole check a subset of a check that measures more.
