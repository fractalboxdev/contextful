# 0326 — An absence assertion carries a presence check ahead of it, and an exclusion over an empty collection is refused

**Status:** accepted 2026-09-18
**Decides:** `build.test.refusal.vacuous-exclusion`

## Context

A large share of this system's behavior is absence. Enforcement removes rows a caller may
not see. Redaction removes spans from text before it is committed. A refusal returns
nothing rather than something wrong. A must-abstain case answers with zero rows. Each of
those is tested by asserting that something is not present.

An absence assertion over an empty collection holds. It holds when the guard works, it
holds when the guard is removed, and it holds when the fixture that was supposed to produce
candidate rows silently produced none — a renamed table, a connector that fetched nothing,
a policy that filtered everything ahead of the code under test. The test is green in all
four cases and distinguishes none of them.

This failure is invisible in review. A vacuous exclusion and a real one are the same line
of code; what separates them is the state of the collection at the moment the assertion
runs, which is somewhere else in the file or somewhere else entirely. A reviewer reading a
diff sees an assertion whose name states exactly the property everyone wants, and the name
is true of the intent rather than of the run.

The consequence compounds because these tests are the evidence behind the fail-closed
defaults. A suite of vacuous exclusions reports that access control, redaction and
abstention are all verified, and the number of passing tests grows while the amount of
measured behavior stays at zero.

## Decision

A test whose central claim is an absence constructs the condition it names, verifies that
the condition obtains, and then asserts over it. An exclusion assertion evaluated over an
empty collection raises `VacuousAssertion`. A guard is validated by watching it fire: the
fixture that motivated it passes, and the state it stands against fails.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A presence check inside the test, ahead of the absence assertion** *(chosen)* | The assertion runs over a collection known non-empty at that moment, so a green result is evidence. Presence and absence cannot drift apart, because one test holds both. | Every absence test grows a setup phase and a second assertion, and an expensive fixture is constructed and inspected rather than just constructed. |
| Trusting the assertion's name and reviewing for vacuity by eye | No cost per test; relies on reviewers who already read the diff. | Lost on vacuity: a vacuous assertion is textually identical to a real one, so the reviewer has no signal to react to. |
| Asserting a non-empty collection as a separate test | The property is stated once, and each test stays small. | Lost on coupling: the two tests can drift — one fixture changes, one does not — and the exclusion goes vacuous again with a green sibling standing beside it. |
| A mutation run that deletes each guard and requires a red suite | Measures the property directly rather than by proxy, across every guard at once. | Lost on cost per run: a mutation pass over a tree that links the bundled SQL engine is orders of magnitude past a gate's per-change budget, and it answers after the fact rather than at the assertion. |
| A lint over test source looking for exclusion assertions with no preceding setup | Cheap, static, catches the shape without running anything. | Lost on precision: the collection's emptiness is a run-time fact, and the setup that fills it is frequently a helper several calls away, so the lint both misses and misfires. |

## Criteria

1. **Vacuity** — whether a green test can hold with no evidence behind it.
2. **Coupling** — whether the presence fact and the absence fact can drift apart over
   later edits.
3. **Cost per test** — setup work and fixture construction added to each absence test.
4. **Cost per run** — time added to a gate run.

**Vacuity decides it.** It is the only criterion that bears on whether the suite measures
anything at all; the other three trade against a property that is already worthless if this
one is unmet. Coupling then eliminates the separate-test option, which satisfies vacuity at
the moment it is written and stops satisfying it later.

## Consequences

A green absence test means a populated collection was inspected and the named thing was not
in it. The guard-fires-both-ways construction extends that to guards: a guard is evidenced
by a recorded failure against the state it exists to reject, not by the absence of a
failure against a state nobody built.

The cost accepted: every absence test grows a setup phase and a second assertion, and a
fixture that is expensive to construct is paid for twice — once to build it and once to
verify it is non-empty. For a fixture that ingests a corpus through the real store, that is
not a rounding error against the suite's wall clock.

Reversing this is cheap mechanically and expensive in trust: the checks can be dropped in a
commit, but every absence assertion in the tree then reverts to being unfalsifiable, and
nothing distinguishes the ones that were genuinely verified from the ones that were not.

## Revisit triggers

- A fixture's presence verification dominates a suite's wall clock, measured against the
  per-stage ceiling rather than argued about.
- A mutation-testing pass becomes cheap enough to run on a cadence the drift window
  tolerates, making the per-test construction redundant for the guards it covers.
- An assertion library appears whose exclusion primitive refuses an empty collection at the
  call site, moving the check out of the test body and removing the per-test cost.
