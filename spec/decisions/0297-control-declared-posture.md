# 0297 — A published hostname's posture is declared in a descriptor and probed against it

**Status:** accepted 2026-09-18
**Decides:** `control.serve.refusal.posture-mismatch`, `control.serve.refusal.descriptor-unknown-field`

## Context

The read surface and the operator portal are published at hostnames whose intended openness
differs. One sits behind an identity gate and redirects an anonymous visitor to a login.
One authenticates its own callers with capability tokens and answers anonymous requests
directly, correctly. One is open on purpose, sent to readers outside the team, and a login
in front of it would be the fault.

A worker's own configuration records none of these three gates. It records a route and a
script. So a hostname whose protection was never attached, and a hostname whose protection
is deliberately absent because it checks callers itself, are the same bytes in the place
where enforcement lives. No check reading provider state can separate them, and neither can
a person reading it.

The provider's own policy model widens the gap rather than closing it. A wildcard
application over the deployment's domain protects no hostname by itself: it carries a
reusable bypass policy for the hostnames that authenticate their own callers, and a
reusable bypass policy takes precedence over an application's allow policy whatever
precedence numbers the two carry. A hostname is protected by an application of its own or by
nothing — which means a hostname can be detached from its own application by an unrelated
edit and keep answering, inside the wildcard, exactly as it did before.

The descriptor that would record intent has its own failure. Provider configuration is
emitted from it, and a key the emitter does not model is dropped by default. The dropped key
is the restrictive one often enough to matter — the flag marking a store loopback-only is
the concrete stake — so the default publishes the thing the key existed to withhold.

## Decision

Every published hostname carries a descriptor naming its worker, its hostname, the contract
version it was written against, and a required gate valued `access`, `adminToken`, or
`public` with an explicit acknowledgement. A deploy asks each declared hostname what an
anonymous `GET /` receives and judges the answer against that gate: `access` accepts a `302`
whose location carries the owning team's login prefix compared literally, `adminToken`
accepts any non-`5xx` answer carrying no login location, and `public` accepts a `200`
carrying none. A hostname answering outside its gate, and a declared hostname that cannot be
reached at all, raise `HostnamePostureMismatch` and fail the deploy. The probe table and the
descriptors name the same pairs, asserted before the deploy runs. A descriptor decodes with
excess properties refused and raises `DescriptorUnknownField` rather than dropping an
unlearned key, and the escape hatch for an unmodelled configuration key is checked against a
fixed reserved set rather than against whatever keys a descriptor emitted.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A descriptor per hostname, probed anonymously before publication** *(chosen)* | Open-on-purpose becomes an artifact with a name on it, and an application attached later reds the deploy instead of silently closing a surface its readers depend on. | Adding a surface means writing a descriptor and widening a closed literal, and a hostname outside the declared set is not probed at all. |
| Deriving posture from worker configuration | No second artifact to maintain; the check reads the same state the enforcement does. | Lost on distinguishability: a worker records none of the three gates, so deliberately open and accidentally open are identical there and no derivation can separate them. |
| A probe list kept beside the descriptors with no equality assertion | Probing without coupling two artifacts; either can be edited alone. | Lost on silent drift: a hostname probed under an undeclared gate and a declared hostname left unprobed both pass, so the list decays into a description of an older deployment. |
| Dropping an unmodelled descriptor field rather than refusing | A descriptor written against a newer contract still deploys; no release is ever blocked on the emitter catching up. | Lost on fail-safe emission: the flag marking a store loopback-only disappears from the emitted configuration and the surface publishes wider than its descriptor says. |

## Criteria

1. **Whether open-on-purpose and open-because-nothing-is-attached are distinguishable.**
   **This criterion decided.** Both failures under consideration — a surface that lost its
   gate, and a surface whose gate was never meant to exist — reduce to this one question,
   and every option that relies on inference fails it identically, because the state being
   inferred from does not carry the distinction at all.
2. **Whether a hostname gains or loses a gate without a reviewable artifact** — a policy
   attached or detached outside the deploy path has to red something.
3. **Fail-safe behavior on emission** — whether an unmodelled key is dropped or refused.
   Refusal costs releases; dropping costs exposure.
4. **Authoring cost per surface** — a descriptor and a literal per hostname. The chosen
   option is the most expensive here.

## Consequences

The three gates become a readable property of the deployment: a reader can enumerate which
hostnames answer anonymously and see that someone acknowledged each one. The probe table
cannot drift from the descriptor set, so a hostname added to one and not the other fails an
assertion rather than being probed for a posture nobody declared. The wildcard
application's real behavior — that it gates nothing by itself — stops being folk knowledge
and becomes something the descriptors and the probe together make operationally visible.

The cost accepted: strictness blocks releases, and coverage is exactly the declared set. A
descriptor carrying a key the emitter has not learned stops a deploy until the model catches
up, which turns a provider's new feature into a release blocker. A hostname nobody wrote a
descriptor for is not probed at all, so the check's coverage is bounded by an authoring
step that is easy to skip when a surface is added in a hurry. Changing a posture is two
coordinated edits, and a forgotten one is a red deploy — correct, and still a stop.

Drift between deploys stays uncovered. A policy detached an hour after a publication is not
noticed until the next one runs.

## Revisit triggers

- Posture drift is observed between deploys often enough that a publication-time probe is
  the wrong cadence, making a continuous external check worth its own decision.
- The reserved-key set needs frequent extension, indicating the descriptor model is
  chronically behind the provider surface rather than occasionally.
- A provider begins recording gate intent in configuration a check can read, which would
  remove the distinguishability gap this decision exists to close.
