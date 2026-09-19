# D02 — A plan is compiled, content-hashed data; no embedded runtime

**Status:** accepted

## Context

A journaled step replays correctly only against the definition that produced it. A definition interpreted at run time can differ between the original run and its replay, and a script runtime costs tens of megabytes in every profile.

## Decision

The authoring surface is a build-time compiler emitting a serialized, content-hashed plan, and a run pins against the plan hash. No profile links a script runtime.

- `run.compile` accepts a step body only as a connector reference; every dynamic decision sits inside a connector the plan names, so a raw network call or a clock read has no position in the graph.
- Control flow is declared: `branch` over a predicate, `parallel` for fan-out and a map for iteration. Data selects which declared arm runs, never which arms exist.
- `run.transform` rewrites a batch in place and never emits more rows than it consumed. Row-producing work reads landed rows in its own tier and writes back through the ordinary destination.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Compiled, content-hashed plan *(chosen)* | — | Every definition change is a compile; an expression a connector does not offer needs a new connector. |
| An embedded script runtime for step bodies | Determinism and footprint | Replay can run a different definition, and every profile carries the runtime. |
| A restricted expression language evaluated at run time | Determinism | Bounded or not, the replayed definition is whatever evaluates on replay. |
| Pipelines as Rust code in the tree | Authoring ergonomics | Every agent-authored definition requires a source change and a rebuild. |
| Row growth inside the chain | Journal granularity | A batch's output no longer maps onto its input, so cursor and dedup guarantees break. |

## Consequences

- A plan is diffable and inspectable as data, and its hash names exactly what ran.
- Deferred work that expands rows, such as transcription or vision, lives in `run.select` and its siblings, reading from the store rather than from a vendor mid-pull.
- A sandboxed connector is bound identically, since the host invokes it inside a recorded step.
