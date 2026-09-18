# 0111 — The host verifies a bound credential's granted scopes before the first read and refuses an unverifiable answer

**Status:** accepted 2026-09-18
**Decides:** `connector.declare-capability.refusal.scope-exceeded`, `connector.declare-capability.refusal.scope-unverified`

## Context

A manifest declares the host access a connector reaches for, and the host decides that
grant from the file. What the file cannot state is what the vendor actually issued. An
operator mints a token at a vendor console, pastes a reference to it into a binding, and
the engine holds a credential whose real authority is whatever the console's checkboxes
were left on. Vendor consoles default generously: the read-only scope an operator meant to
grant arrives alongside write, admin, or full-account access more often than not.

The engine cannot narrow a credential it did not mint. It can only observe what it holds
and decide whether to use it. Many vendors expose an identity endpoint that answers with
the granted scopes in a response header, which makes the observation a single call — one
the host can make with the bound credential ahead of the first read, before any material
has crossed in either direction.

That call has three outcomes, not two. The scopes match expectation; the scopes exceed it;
or the answer carries no scopes header at all. The third is the one that decides the shape
of this record. A vendor that stops emitting the header, an endpoint behind a proxy that
strips it, a response served from a cache — each produces an answer that is neither a pass
nor a fail, and whichever way it is resolved becomes the probe's real behavior, since it is
the outcome that recurs.

## Decision

A manifest may declare an identity endpoint, the response header carrying granted scopes,
and the full grant it expects. The host calls that endpoint with the bound credential ahead
of the first read. A granted scope falling outside the declared expectation raises
`ConnectorScopeExceeded` and the session does not open. A probe response carrying no
granted-scopes header raises `ConnectorScopeUnverified`. Cannot-verify is not verified. The
probe carries the bearer, so its host sits on the allowlist and its scheme is TLS or
loopback.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the session on excess and on an unreadable answer** *(chosen)* | The excess access is corrected at the vendor, where it can actually be removed, before any run holds it. | A vendor that stops emitting the header breaks a working pipeline until the manifest drops the probe. |
| Warn on an over-scoped token and run anyway | No pipeline is ever blocked by a console default. | Lost on duration of exposure: the excess access stays live for every run, and the warning recurs until it is filtered. |
| Treat a missing scopes header as a pass | Resilient to vendor drift; no false refusal. | Lost on the meaning of the control: it makes the probe advisory, and a vendor dropping the header silently disables verification for every connector using it. |
| Probe lazily, on the first failure | No round trip on the happy path. | Lost on position relative to the material: the first failure is observed after reads have already run under the over-scoped credential. |
| Require the probe of every connector | Uniform posture; no manifest decides its own verification. | Lost on vendor coverage: many vendors expose no identity endpoint, so the requirement would make them unreachable. |

## Criteria

1. **Whether cannot-verify may read as verified.** *This criterion decided it.* An
   over-scoped token is an operator mistake with a fix at the vendor console; the failure
   mode that has no fix is a control that reports success when it performed no check. A
   verification layer whose degraded case is silent approval is worse than none, because
   the operator believes it ran.
2. **Duration of exposure.** How long excess authority stays usable after it is observed.
3. **Position of the check relative to the first read.** Whether material crosses before
   the credential is judged.
4. **Vendor coverage.** Whether the rule is satisfiable against vendors that expose no
   identity endpoint.

## Consequences

An operator learns about an over-scoped token at session open, with a diagnostic naming the
scope that exceeded expectation, rather than from an incident report. The declared
expectation in the manifest becomes a second, reviewable statement of intent beside the
allowlist: what the connector expects to be able to do at the vendor, not merely which host
it may reach.

The accepted cost: the engine's availability is now coupled to a vendor's response header.
A vendor that stops emitting it takes every pipeline declaring the probe down at once, and
the remedy — editing the manifest to drop the probe — is a change that weakens the posture
under time pressure, which is the worst moment to be making it.

The probe is opt-in per manifest, so a connector against a vendor with no identity endpoint
runs unverified and nothing marks it as such at the run record. Whether that absence should
itself be disclosed is not settled here.

## Revisit triggers

- A vendor drops the granted-scopes header in a way that takes running pipelines down,
  which would put the cost of failing closed on the record rather than in the abstract.
- A vendor exposes scope information in a response body rather than a header, which the
  declared header name cannot reach.
- The disclosure surface gains a per-run statement of which credentials went unverified,
  which would change whether an absent probe may stay silent.
