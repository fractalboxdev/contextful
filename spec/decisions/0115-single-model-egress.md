# 0115 — Model calls leave through one operator-configured OpenAI-compatible endpoint and no vendor SDK exists in the engine

**Status:** accepted 2026-09-18
**Decides:** `connector.infer.refusal.vendor-sdk`

## Context

Inference is one of the few places the engine hands ingested material to a party outside it.
The material is the consumer's own context — notes, documents, records pulled from their
vendors — and the question of who holds it and where it goes is the question the whole
authority contract exists to answer. A model call is therefore an egress decision first and
a capability second.

A vendor SDK is not a thin thing. It brings its own HTTP client, its own retry policy, its
own credential handling, its own telemetry, and a dependency tree the engine did not choose.
Material flows through all of it. Every one of those layers is a place where a prompt can be
logged, a credential can be cached, or a request can be routed somewhere the engine's own
allowlist never judged. The host owns the credential, the rate limit and the per-call span
precisely so that none of those are properties of library code.

The chat-completion wire is, in practice, one shape. Enough providers — hosted vendors,
gateways, and local runtimes alike — answer an OpenAI-compatible endpoint that the set of
providers reachable through that one wire covers the deployments this engine targets,
including the entirely local ones. A local-first system whose inference path only works
against a hosted vendor is not local-first.

Exactly one step in the call is provider-shaped: serializing a tool's parameter schema into
that provider's tool envelope. Nothing else in the path knows which provider answered.

## Decision

Every model call resolves to one operator-configured OpenAI-compatible HTTP endpoint
declared as a capability. A crate importing a model-vendor SDK raises `ConnectorVendorSdk`,
and the gate enforces the absence across the workspace. A provider swap is a URL edit, with
no conditional branch anywhere in the engine. The host owns the credential, the rate limit
and the per-call span; a component owns the prompt template and the response schema and
observes no material.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **One OpenAI-compatible endpoint, no vendor SDK** *(chosen)* | A provider swap is a URL edit; material never enters library code the engine did not choose; the same path serves a local runtime. | Only the lowest-common-denominator chat-completion wire is reachable, so provider-specific features are unavailable, and the operator supplies and runs the model. |
| A vendor SDK per provider | Every provider's full feature surface, with its own ergonomics and its own streaming. | Lost on what a provider change touches: swapping becomes a code change across the engine, and material reaches each vendor's client, retry and telemetry layers. |
| A thin abstraction over several SDKs | Provider-specific features behind one interface. | Lost on dependency and custody: the abstraction still carries every vendor's dependency tree and auth model, so the custody problem is unchanged and the surface is larger. |
| A sidecar process translating to each vendor's wire | The engine stays clean; features stay reachable. | Lost on operational cost against benefit: it is a second deployable with its own credential custody, which is the problem being solved, relocated. |

## Criteria

1. **Custody of material at the egress point.** Which code holds the prompt and the
   credential between the engine and the network. *This criterion decided it.* The engine's
   claim about where a consumer's context goes has to be checkable by reading the engine.
   A dependency tree the engine did not choose makes that claim unverifiable, and an
   unverifiable claim about data custody is the one thing this system cannot ship.
2. **Blast radius of a provider change.** How much of the engine a swap touches.
3. **Reachability of a fully local deployment.** Whether the inference path works with no
   hosted vendor at all.
4. **Feature ceiling.** Which provider capabilities become unavailable.

## Consequences

The engine has one egress point for inference, one credential, one span, and one place to
apply the data fence that separates ingested values from the operator's template. A gate
check over the workspace keeps it that way mechanically rather than by review habit.
Pointing the engine at a locally running model is the same configuration as pointing it at
a hosted one.

The accepted cost is a real feature ceiling. Provider-specific extensions — extended
reasoning controls, native structured-output modes, vendor-specific caching semantics,
multimodal envelopes that do not fit the common wire — are unavailable. Where a provider's
differentiator lives outside the chat-completion shape, this engine cannot reach it. The
operator also carries the burden of supplying and running the model, which for a hosted
vendor means configuring a gateway rather than dropping in a client.

How much capability is actually forgone shifts with the ecosystem and is not measured here;
the common wire has been widening, but that is an observation, not a guarantee.

## Revisit triggers

- A capability the engine needs — a structured-output guarantee, a caching semantic with
  real cost impact — exists only outside the chat-completion wire across every provider an
  operator would choose.
- Compatible endpoints stop converging, so that "OpenAI-compatible" ceases to name one wire
  and provider-conditional handling reappears by necessity.
- The single provider-shaped step grows past tool-envelope serialization into a second or
  third branch, which would mean the abstraction is leaking and the single-endpoint claim
  is no longer true of the code.
