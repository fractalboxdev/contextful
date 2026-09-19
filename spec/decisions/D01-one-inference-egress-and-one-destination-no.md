# D01 — One inference egress and one destination; no vendor SDK, no sink plugin

**Status:** accepted

## Context

Every model vendor ships an SDK that brings its own HTTP stack, credential handling and retry policy. Every destination beyond the store duplicates a layout the store already holds, or promises delivery semantics a run cannot keep.

## Decision

Model access leaves the process through one operator-configured endpoint capability speaking OpenAI-compatible HTTP, and a pipeline lands into one destination, the local store.

- `connector.infer` owns the endpoint. No model-vendor SDK links into any crate, and a provider swap is a URL edit. The host owns the credential, the rate limit and the per-call span; a component owns its prompt template and response schema. The one provider-shaped step serializes a tool's parameter schema.
- `run.land` resolves the local store and refuses any other destination name at assembly. A synthesized artifact, whether a derived table, a summary or a model's output, writes back through the same destination and becomes queryable corpus. The destination interface has no host arm, so a guest cannot supply one.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| One compatible endpoint, one destination *(chosen)* | — | A vendor feature outside the compatible wire is unreachable; an export is a copy of the store's own layout. |
| A vendor SDK per provider behind an abstraction | Dependency custody | Each SDK links its own HTTP stack into every profile and dictates auth and retry semantics. |
| A plugin interface for backends or sinks | Attack surface | A loaded plugin holds credentials, and the destination set is no longer nameable. |
| A sidecar translating to each vendor's wire | Operational cost | A second deployable with its own credentials and failure modes. |
| A webhook destination | Delivery semantics | Delivery is at-least-once messaging, which the run's commit model cannot honor. |

## Consequences

- The dependency audit refuses a model-vendor SDK in any crate, and a second outbound path to a model.
- A provider change needs no rebuild.
- Every synthesized output is governed by the same grants, masks and retention as landed data.

## Revisit

- A capability a deployment needs that no OpenAI-compatible endpoint exposes.
