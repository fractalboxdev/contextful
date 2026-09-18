# 0005 — Model access leaves the process through one operator-configured endpoint capability

**Status:** accepted 2026-09-18
**Decides:** `topology.compose.refusal.inference-egress`

## Context

Several parts of the system reach a language model: memory synthesis, the analyst surface,
derive steps that transcribe or summarize. Each of them could plausibly hold its own client
and its own credentials, and each vendor publishes an SDK that makes that the path of
least resistance.

Two constraints push the other way. The first is placement. Operators run this engine where
their data is allowed to be, and inference is the step most likely to move bytes across a
boundary that a residency requirement cares about. An operator needs to point inference at
a specific endpoint — their own gateway, a regional deployment, a self-hosted server on the
same network — and that choice belongs to the deployment, not to a crate.

The second is footprint. Three build profiles carry compressed and idle-resident budgets,
and each vendor SDK brings its own HTTP stack, its own retry and backoff semantics, its own
auth model and its own transitive dependency set into the dependency audit. Several of them
multiply all of that.

The OpenAI-compatible HTTP shape is what every serious gateway, local server and hosted
provider already speaks, which makes it usable as the one wire format without asking any
operator to run a translation layer they would not otherwise run.

## Decision

Model access leaves the process through one operator-configured endpoint capability
speaking OpenAI-compatible HTTP. No model-vendor SDK links into any crate, and there is one
outbound path to a model rather than one per caller. Exactly one step is provider-shaped:
serializing a tool's parameter schema into that provider's envelope. A crate declaring a
model-vendor SDK, or a second outbound path to a model, raises `VendorSdkLinked` in the
dependency audit, naming the crate and the dependency.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **One endpoint capability speaking OpenAI-compatible HTTP** *(chosen)* | The operator chooses placement, including a self-hosted or regional endpoint, with no code change. One egress to audit, one credential to scope, one retry policy. | A vendor feature with no OpenAI-compatible expression — a native tool-call format, a provider-specific caching control — is unreachable. |
| A vendor SDK behind an abstraction layer | Idiomatic access to that vendor's full feature set, including anything the compatible surface omits. | Lost on placement: the SDK still links, still dictates auth and retry semantics, and still assumes the vendor's own endpoints. The abstraction hides the call, not the constraint. |
| Several vendor SDKs behind a trait | Every vendor's full feature set, selected per deployment. | Lost on footprint and on the dependency-audit surface it creates: each SDK brings an HTTP stack and a transitive set, and the audit can no longer state a single outbound path. |
| A plugin interface for inference backends | Third parties add backends without touching the tree. | Lost on attack surface and on the same placement criterion: a loaded backend holds credentials and an outbound socket outside the audited graph. |

## Criteria

1. **Placement** — whether an operator points inference at an endpoint of their choosing,
   including inside a residency boundary. **This criterion decided.** Inference zones and
   residency requirements are the constraint deployments actually carry, and only an
   operator-chosen endpoint satisfies them; every SDK-based option leaves the endpoint
   partly in the library's hands.
2. **Footprint** — bytes and transitive dependencies added to each profile's budget.
3. **Vendor lock-in** — how much of the tree would change to move from one provider to
   another. With one compatible endpoint, the answer is configuration.
4. **Feature reach** — access to provider-specific capability. The chosen option is the
   weakest here and lost this criterion knowingly.

## Consequences

Auditing egress is one question: does any crate declare a vendor SDK, and is there a second
outbound path. Credential scoping and rate limiting apply at one place. An operator can put
a gateway in front of the endpoint and get logging, caching and routing without the engine
knowing.

The cost accepted: anything a vendor exposes only through its native surface is out of
reach. A provider-specific caching control, a native tool-call encoding, a streaming
extension outside the compatible shape — none of those are available, and a deployment that
wants one has no supported way to get it. The one provider-shaped step, serializing a
tool's parameter schema into the provider's envelope, is where that pressure lands first.

Reversing this is moderate: adding an SDK is mechanically easy, but every downstream
assumption about one auditable egress, one credential and one retry policy would need
revisiting.

## Revisit triggers

- A capability the product depends on exists in no OpenAI-compatible form across the
  providers deployments actually use.
- The provider-shaped serialization step grows into a family of per-provider branches,
  meaning the compatible surface is no longer carrying the variation.
- Providers diverge such that "OpenAI-compatible" stops predicting whether a given endpoint
  works, and compatibility must be tested per provider anyway.
