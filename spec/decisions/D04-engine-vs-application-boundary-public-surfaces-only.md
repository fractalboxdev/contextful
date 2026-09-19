# D04 — The engine and its consumers meet only at public surfaces

**Status:** accepted

## Context

The read path runs on an edge replica without the run path. Applications built on the engine, including the commercial layer, bring vocabulary and needs of their own. Every private crossing is a place a capability enters unreviewed and a place the open engine stops being whole.

## Decision

- `topology.compose` admits exactly three crossings between read path and run path: the connector interface world, the columnar-part-plus-manifest layout, and capability tokens. Each carries its own version and a conformance suite both sides run; a new member passes security review.
- The engine owns reusable ingestion, storage, memory, query and policy contracts and no domain. An application names its vocabulary in a per-store lexicon, and an undeclared category renders neutral. A behavior becomes a shared engine abstraction once a second application needs it under the same invariants.
- `topology.bound-application` gives the commercial layer the surfaces every client uses: the tool protocol, the manifest and plan format, the token format and the sync protocol. A paid feature is net-new; no existing data-plane capability sits behind a license check.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Three versioned crossings; public surfaces only *(chosen)* | — | A feature needing a fourth crossing is a design question, and the commercial layer waits for each public surface it needs. |
| A general internal RPC surface between halves | Review surface | Every call becomes a place a capability can enter. |
| Direct dependencies from read path onto run path | Composability | The run path stops being droppable and the edge profile is lost. |
| A privileged internal API for the commercial layer | Wholeness | The open engine becomes a subset of itself. |
| The first application's vocabulary as engine defaults | Inheritance | Every later application receives a schema and meanings it did not choose. |

## Consequences

- An operator scripts anything the commercial layer does against the bare engine.
- These boundaries have no run-time trigger, so `assurance.gate` enforces them over the package graph and source rather than as request-time refusals.
- The operator view derives its pipeline graph from configured pipelines, with application nodes as an overlay.
