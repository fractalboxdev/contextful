# 0092 — A plan step's body is a connector reference, and no profile embeds a scripting runtime

**Status:** accepted 2026-09-18
**Decides:** `pipeline.compile.refusal.step`, `pipeline.compile.invariant.scripting-runtime`

## Context

A compiled plan is a flat list of nodes with explicit predecessor edges, and `step` is the node
kind that does work. The question is what a step body may hold: a reference to a named connector,
or authored code the engine executes.

Replay is what decides it. A journaled step runs its effect exactly once — the first call under
its key enters the effect and durably writes what it produced, and every later call under that
key returns the written value without entering the effect again. That property is the
determinism boundary: a run body replays faithfully when every observable side effect passes
through a recorded step, a cursor commit or an awakeable, and the work between them is pure.

Authored code inside a step breaks the arrangement from inside. A body that reads the clock,
opens a socket, or consults the filesystem produces an output that is not a function of what the
journal holds, so a resumed run and its first attempt diverge while both report success. The
journal cannot detect the divergence; it records what it was handed.

The second pressure is the deployment profile. An embedded interpreter is a permanent dependency
in every build of the engine — binary size, a sandbox to configure, a language surface to version
— on a product whose premise is a local binary a small team runs.

## Decision

A `step` body that is anything but a connector reference raises `PipelineInlineStepBody`. No
build profile embeds a scripting runtime; the compiled plan holds data, and every dynamic
decision sits inside a connector the plan names. A raw network call and a clock read therefore
have no position in the graph. The rule binds a sandboxed connector identically, since the host
invokes one from inside a recorded step and the recording is what the guarantee rests on.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A step body is a connector reference; no embedded runtime** *(chosen)* | Every step is a recordable unit whose output is a function of the journal; the binary carries no interpreter. | Genuinely dynamic logic moves inside a connector, so a one-line branch becomes an authored artifact with its own build and its own pin. |
| An embedded scripting surface for step bodies | A transform or a branch is written where it is used, with no separate artifact. | Loses on replay determinism: arbitrary code is not a recordable step, since its output is not a function of what the journal holds. Loses again on binary footprint, which would be reason enough on its own. |
| An embedded runtime restricted to pure expressions | Inline authoring with determinism preserved by construction. | Loses on enforcement cost: purity has to be proven against the surface's whole standard library, and every release of that library reopens the proof. The restricted surface also covers little more than the declarative chain already covers. |
| A sandboxed per-batch transform interface | Arbitrary per-row work with the host controlling the boundary. | Deferred rather than rejected: it is coherent, since the host invokes it from inside a recorded step, but the declarative chain covers the shapes seen so far, so it loses on timing rather than on principle. |

## Criteria

1. **Replay determinism** — whether a step's output is reproducible from the journal on a
   resumed run. *This is the criterion that decided it.* Every other property of the run path —
   exactly-once effects, crash resume, a recorded pull that replays without reaching a vendor —
   is stated relative to the journal being the only boundary. An unrecordable step body makes
   those statements conditional on what an author wrote, which is to say untrue.
2. **Deployment footprint** — what every build carries whether or not a project uses it.
3. **Inspectability of the plan** — whether a reader determines what a run will do by reading
   the compiled plan. A connector reference is a name; a code body is not.
4. **Authoring convenience** — how much ceremony a small piece of dynamic logic costs. This is
   the criterion the chosen option loses on.

## Consequences

A plan is data end to end, which makes it hashable, diffable, and emittable by something other
than a human — the plan JSON is the authoring floor an agent writes directly. Lowering onto a
durable substrate stays total, since every node kind has a fixed shape. The binary stays free of
an interpreter and its sandbox.

The cost accepted lands on the author with a small dynamic need. A three-line decision that would
have been a step body becomes a connector: a named artifact, built, versioned, referenced, and
pinned. For a one-off that is a poor trade and it will be felt as one. The sandboxed transform
interface remains available as an answer and is deliberately unbuilt; its cost against the
declarative chain is unmeasured.

## Revisit triggers

- Connector-per-branch proliferation becomes visible — a project carrying many single-purpose
  connectors that exist only to hold a predicate.
- A shape appears that the declarative chain genuinely cannot express and that a recorded,
  host-invoked transform could, which is the deferred option's trigger.
- A deterministic, sandboxable expression surface exists whose purity is enforced by its
  runtime rather than argued about, removing the enforcement-cost objection.
