# 0296 — A read surface that cannot verify refuses to open

**Status:** accepted 2026-09-18
**Decides:** `control.serve.refusal.unconfigured-gateway`, `control.serve.refusal.issuer-key`

## Context

The read path is two hops. A routing isolate terminates the request, verifies the presented
credential, routes and caches, and runs no query; a warm container runs the full retrieval
profile with enforcement inside it ahead of any row leaving. Both hops depend on
configuration that arrives from outside the process: the gateway on a store registry that
says which stores exist and where their origins are, and the engine on a verification key
that the credential is checked against.

The deployment holds no authentication secret of its own. The verifying key is public and
the credential is whatever the caller brought, so the key is not a secret to protect but an
input whose correctness decides whether the surface can distinguish a legitimate caller
from any other. It arrives as a file, or as a published key set fetched over the network,
and each of those fails in ways that produce something rather than nothing — a truncated
file, a corrupt encoding, a fetch that returns an error document.

Both misconfigurations have a fail-open shape that is genuinely tempting. A gateway with no
store configuration could open onto a default store and serve. An engine with no usable
verification key could generate one and come up. In both cases the process starts, the
health route is green, and the failure is deferred.

The deferred failures differ in who sees them. A gateway opened onto an unnamed origin
serves rows — the wrong rows, to whoever arrives first, with no record of which store they
came from. A generated verification key is worse to diagnose than to suffer: every
legitimately minted credential fails verification, so the fault appears at the caller, in
their client, as a rejection of a credential they know is valid. The process that is wrong
reports itself healthy while every party talking to it is told they are the problem.

## Decision

A gateway with no store configuration serves `503` on every route and never opens, raising
`GatewayUnconfigured` rather than opening onto an unnamed origin. The engine's HTTP face
refuses to start without a verification key that both resolves and parses, raising
`IssuerKeyUnusable` and naming the input that failed; a truncated key, a corrupt key file,
and a published key set that cannot be fetched refuse alike, and none of them falls through
onto a generated key. Neither state is recoverable by waiting: the health route separates
this from a store that is warming, which resolves into readiness on its own, so a reader of
the health surface can tell a surface coming up from a surface that opens for no caller.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse to open, naming the input that failed** *(chosen)* | The fault is reported at the process that has it, at startup, with the failing input named, and no request is ever served under an unverifiable identity. | A truncated key file shuts the read path entirely rather than degrading it, so key distribution sits on the critical path of every deploy. |
| Generate a verification key when none resolves | The process always starts; no deploy is ever blocked on key distribution; development needs no setup. | Lost on where the fault appears: the process comes up healthy and rejects every legitimately minted credential, so the symptom lands on the caller and the healthy-looking process is the last place anyone looks. |
| Open the gateway onto a default store | A misconfigured deployment still answers, and an operator sees data rather than an error. | Lost on fail-open behavior: an unnamed origin serves rows to whoever arrives first, with no record of which store answered, which is a disclosure rather than a degradation. |
| Start and serve `503` until configuration arrives, with no refusal | Tolerates configuration delivered after startup; a restart is not needed once it lands. | Lost on diagnosis: an indefinite `503` is indistinguishable from a warming store on the data routes, so the one state that never becomes ready looks like the one that always does. |

## Criteria

1. **Whether a misconfiguration fails open or closed** — whether a process that cannot
   verify still serves rows. **This criterion decided.** The read path's entire purpose is
   to let rows out only to callers whose grants cover them, so a configuration that
   defeats verification defeats the surface; serving under it is not a degraded read but a
   different product.
2. **Whether a healthy-looking process can reject every legitimate caller** — the generated
   key produces exactly this, and it is the hardest failure in this set to attribute.
3. **Distinguishability of unavailable states** — warming resolves on its own,
   unconfigured never does, and the health route has to separate them.
4. **Deploy resilience** — whether a missing input blocks a release. The chosen option is
   worst here.

## Consequences

A running read surface is one whose verification input resolved and parsed, so "the process
is up" and "the process can authenticate" stop being independent facts. A failure is
attributable to the deployment that owns it, at startup, naming the input, rather than
arriving as a support question from a caller whose credential is fine. The two unavailable
states stay separable on the health route, which makes a `503` actionable rather than
ambiguous.

The cost accepted: key distribution is on the critical path of every deploy. A key file
truncated by a copy, an object store that is briefly unavailable when a published key set is
fetched, a secret that did not propagate — each takes the read path down entirely rather
than degrading it, and the recovery is a fixed input plus a restart rather than a retry.
For a deployment whose key arrives over the network at startup, the read path's availability
is bounded by that fetch's availability, and this decision accepts that bound rather than
weakening it with a cached or generated fallback.

## Revisit triggers

- Key-set fetch failures at startup are observed causing read-path outages disproportionate
  to the underlying fault, indicating a verified cache of the last key set that verified
  is worth its own decision.
- A deployment shape emerges where store configuration legitimately arrives after the
  gateway starts, making startup refusal the wrong instant for the check.
- The health route's separation of warming from unconfigured is found insufficient for an
  operator to act, suggesting the states need distinct surfaces rather than distinct
  labels.
