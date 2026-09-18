# 0216 — One module owns the authorization decision, compiled twice from one source

**Status:** accepted 2026-09-18
**Decides:** `enforcement.compose.refusal.second-verifier`

## Context

The same authorization decision is reached in two places. A gateway sitting in front of
the engine refuses an unauthorized request early, so it does not travel the whole path
before being turned back. The engine reaches the same verdict again at the effect about to
act, because admission is a moment and authority is re-read. Both are deciding from the
same inputs: a credential's grants, a normalized subject tuple, a table name, an action,
and the declared policy.

The inputs are not simple. Grants carry action sets, table patterns with three resolution
forms, a tenant binding, template lists and row bounds. Narrowing legality is defined per
dimension, with different answers for broader, equal, narrower and absent. Timestamps
decode under one grammar, and a malformed one is refused identically wherever it is read.
Each of those is a place two implementations can agree on every example anyone thought to
write down and disagree on the input nobody did.

Disagreement here is not a bug that surfaces as a wrong answer to a well-behaved caller. A
gateway that is more permissive than the engine wastes work; a gateway that is more
permissive on a malformed credential and an engine that accepts it for a different reason
is a hole. The party who finds the discrepancy is the one probing for it, and they find it
by feeding both implementations inputs a test suite's author would call nonsense.

The gateway is also not necessarily written in the same language as the engine. That is
the fact that makes a second implementation the path of least resistance: writing a
verifier in the gateway's own language is a day's work, and sharing one means the gateway
has to host something.

## Decision

One module owns the decision. Where a gateway and the engine reach the same verdict they
execute that module, compiled native and compiled to WebAssembly from the same pinned
source. A second implementation of any part of the decision, in another language or
another crate, raises `EnforceSecondVerifier`. The two artifacts are build outputs of one
source rather than two programs held in agreement.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **One module, compiled native and to WebAssembly from one pinned source** *(chosen)* | Divergence on malformed or unanticipated input is not expressible; a fix lands once | The gateway hosts a WebAssembly runtime, the build produces two artifacts from one source, and any language the gateway is written in must host that runtime |
| A hand-written verifier in the gateway's own language | No runtime to host; idiomatic in the gateway's stack; fastest to write | Loses on divergence, which is the failure class this decision exists to remove: two implementations agree on the examples and disagree on the rest |
| The gateway performs no decision and forwards every request | Nothing to keep in agreement; one implementation by construction | Loses on cost: an unauthorized request travels the whole path before refusal, which is the work the early refusal exists to avoid |
| A shared written specification plus independent implementations and a conformance suite | Each side stays idiomatic; agreement is tested rather than assumed | Loses on coverage: a suite proves agreement on the cases it enumerates and says nothing about the rest, and the rest is where an attacker looks |

## Criteria

1. **The class of failure each arrangement admits.** *(decided it)* The others weigh
   implementation cost and runtime overhead, which are paid once and measurable. This one
   is about which failures remain possible at all. Two implementations disagree on
   malformed input, and the disagreement is discovered by whoever is probing rather than
   by whoever is testing; one module compiled twice removes the class rather than
   reducing its likelihood.
2. **Whether an unauthorized request is refused before it travels the path.** This is why
   forwarding blindly loses, and why the gateway decides at all.
3. **What a conformance suite proves.** Agreement on enumerated cases, and nothing
   beyond — which is the wrong shape for a security property.
4. **Runtime and build cost at the gateway.** Conceded, and the whole cost of this
   decision.

## Consequences

A change to grant resolution, narrowing legality or timestamp handling lands once and
reaches both decision points from the same commit, and no version skew between them is
expressible beyond the pinned source each artifact was built from. A test written against
the module covers the gateway and the engine simultaneously.

The cost accepted is a hosting requirement. The gateway carries a WebAssembly runtime,
the build produces two artifacts from one source, and any language the gateway is written
in has to host that runtime — which rules out gateway implementations in environments
where it cannot. Debugging crosses a boundary: a verdict reached inside the module is
inspected through the module's own interface rather than in the gateway's native
debugger. The performance of the WebAssembly artifact relative to the native one on the
decision path is unmeasured.

Adding a second implementation later is trivially easy and forfeits the property
completely, which is why the refusal names it rather than leaving it to review.

## Revisit triggers

- A gateway deployment target appears that cannot host a WebAssembly runtime and is
  otherwise required.
- The WebAssembly artifact's decision latency is measured as a material share of gateway
  request handling.
- The decision surface shrinks to something small enough that two implementations could be
  proven equivalent rather than tested for agreement.
