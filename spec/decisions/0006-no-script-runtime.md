# 0006 — The authoring surface is a build-time compiler emitting a serialized, content-hashed plan

**Status:** accepted 2026-09-18
**Decides:** `topology.compose.refusal.script-runtime`

## Context

Pipelines have to be authorable by people who are not modifying the engine, and
increasingly by agents generating definitions rather than writing them by hand. The
familiar answer is an embedded script runtime: the definition is a program, the engine
evaluates it, and anything expressible in the language is expressible in a pipeline.

Three properties of this system make that answer expensive. Replay is the first. The run
path journals steps and resumes a crashed run from the journal, which requires that
re-executing a step from the same inputs produces the same effects. A definition that is
evaluated at run time can consult the clock, the environment, the filesystem or the
network between one execution and the next, and replay stops being reproducible without
anything in the system noticing.

Footprint is the second. Three profiles carry compressed and idle-resident budgets, and a
script runtime is tens of megabytes against all three, including the read replica that
never authors anything.

Isolation is the third. The tree already operates one sandbox — a component host mediating
every capability a connector holds. A script runtime is a second sandbox with a different
escape surface, a different capability model and a separate body of hardening work, and the
two would both need to be right.

## Decision

The authoring surface is a build-time compiler that emits a serialized, content-hashed
plan. Nothing interprets a script language at run time, and a JavaScript runtime linked
into any profile raises `ScriptRuntimeLinked`. A run pins against the plan hash, so a
journaled step replays against exactly the definition that produced it.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A build-time compiler emitting a content-hashed plan** *(chosen)* | Replay is reproducible against a hash; no runtime bytes in any profile; one sandbox to secure. Authoring stays ergonomic for generated definitions. | Anything the compiler cannot express at build time is not expressible at all, and a dynamic pipeline shape needs a rebuild and a new plan hash. |
| An embedded script runtime for pipeline authoring | Maximum expressiveness; dynamic shapes; a familiar authoring language. | Lost on all three criteria at once: tens of megabytes against every profile budget, non-deterministic replay, and a second sandbox to secure beside the component host. |
| A Rust-only authoring surface, pipelines as code in the tree | Full type checking, zero new formats, deterministic by construction. | Lost on authoring ergonomics for the agent-authored case: every definition requires a compiler toolchain and a rebuild of the engine, which is not a surface an agent or an application author can drive. |
| A restricted expression language evaluated at run time | Dynamic shapes with a bounded escape surface. | Lost on determinism: bounded or not, evaluation at run time means the definition a replay sees is produced again rather than pinned, and the hash no longer identifies the plan. |

## Criteria

1. **Determinism** — whether a journaled step replays against the same definition it
   originally ran against. **This criterion decided.** Journal replay is what makes the run
   path durable, and a content-hashed plan is both what makes replay reproducible and what
   a run pins against; every run-time-evaluated option breaks that identity.
2. **Footprint** — bytes added to each profile's compressed and resident budgets.
3. **Attack surface** — the number of distinct sandboxes the tree operates and hardens.
4. **Authoring ergonomics** — whether an application author or an agent can produce a
   pipeline without building the engine. The chosen option is mid-range here; the Rust-only
   option lost on it outright.

## Consequences

A plan is data: hashable, diffable, storable, and comparable across runs. A run record can
name the exact plan it executed. No profile pays for a language runtime, which is part of
what keeps the read replica inside its budget. Connector sandboxing is the tree's one
isolation problem rather than one of two.

The cost accepted: expressiveness is bounded by the compiler. A pipeline whose shape
depends on data discovered at run time cannot be written; it must be restructured into
something the compiler can emit, or it waits for the compiler to learn the construct.
Changing a pipeline means a rebuild and a new plan hash, so the edit-to-run loop is longer
than an interpreted surface would give, and that friction is felt most by the people
iterating on a new definition.

Reversing this is expensive in a specific way: adding a runtime later would not just add
bytes, it would invalidate the claim that a plan hash identifies what replayed.

## Revisit triggers

- Pipeline definitions repeatedly need a construct the compiler cannot express, and the
  restructuring each time is judged worse than the definition it replaces.
- A run-time evaluator appears that is deterministic by construction and small enough to
  sit inside the profile budgets.
- The component host's sandbox grows to cover a general evaluation capability anyway,
  making the second-sandbox cost no longer additional.
