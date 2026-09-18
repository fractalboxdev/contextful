# 0011 — Every published hostname declares its gate in a descriptor, and the deploy probes that hostname anonymously

**Status:** accepted 2026-09-18
**Decides:** `topology.publish-hostname.refusal.posture-probe`, `topology.publish-hostname.refusal.probe-table`, `topology.publish-hostname.refusal.descriptor-field`

## Context

A deployment publishes several hostnames, and they are not meant to be equally open. Some
sit behind an identity gate, some authenticate their callers with capability tokens, and
some are open on purpose because they are sent to people outside the team. A worker's own
configuration records none of that: an open surface that was intended to be open and an
open surface whose protection was detached or never attached look identical in it. Both are
a hostname with a route and no policy.

Provider-side policy makes this worse rather than better. A wildcard application over a
domain protects no hostname on its own; it carries a reusable bypass policy for hostnames
that authenticate their own callers, and a reusable bypass policy wins over an
application's own allow policy whatever precedence numbers the two carry. So a hostname can
sit "inside" a wildcard application and answer anonymously, correctly, by design — and a
hostname that was protected can lose its application to an unrelated edit and answer
anonymously too.

The other failure is quieter. Configuration is emitted from a declaration, and a
declaration key the emitter does not model is dropped by default. The dropped key is
usually the restrictive one — the flag marking a store loopback-only is the concrete
stake — so the silent default publishes exactly what the key existed to withhold.

## Decision

Every published hostname carries a descriptor naming its worker, its hostname, the contract
version it was written against, and a required gate valued `access`, `adminToken`, or
`public` with `acknowledged: true`. The deploy emits the hostname's provider configuration
from that descriptor and asks the hostname what an anonymous `GET /` returns, judging the
answer against the declared gate. A `200`, `401`, `403` or `5xx` under `access`, and an
unreachable hostname under any gate, raise `HostnamePostureMismatch` carrying the hostname,
the declared gate and the observed response; an application attached in front of a `public`
hostname reds the deploy rather than quietly closing that surface. The probe table is
derived from the descriptors, and any hostname present on one side and missing from the
other raises `ProbeTableDrift`. A descriptor decodes with excess properties rejected, and an
unmodelled key raises `DescriptorUnknownField` naming the key and the contract version.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A descriptor per hostname, probed anonymously at deploy** *(chosen)* | Open-on-purpose becomes a reviewable artifact with an explicit acknowledgement, and drift in either direction reds before publication. | A posture change is two edits, and a descriptor field the emitter does not model fails the deploy rather than being dropped, so an unlearned key blocks a release. |
| Deriving posture from the provider's configuration | Nothing to maintain; the truth is where the enforcement is. | Lost on recoverability: provider configuration records no intent, so open-on-purpose and open-by-accident are the same bytes and no check can tell them apart. |
| Probing with no declaration | Catches unreachable and obviously wrong responses with zero authoring. | Lost on the same criterion: there is nothing to judge the response against, so a `200` is either correct or catastrophic and the probe cannot say which. |
| Declaring with no probe | Intent is recorded and reviewable; no deploy-time network dependency. | Lost on drift: a policy attached or detached later opens or closes a surface and nothing reds. The declaration describes the deployment as it was imagined. |
| A periodic external posture monitor instead of a deploy-time probe | Catches drift between deploys, not just at them. | Lost on timing: the check runs after publication, so the window it leaves open is the one that matters. It composes with the chosen option rather than replacing it. |

## Criteria

1. **Recoverability of intent** — whether a later reader can tell a deliberately open
   surface from an unprotected one. **This criterion decided.** Provider configuration
   records no intent, so without a declaration the two states are literally identical
   bytes; every other property depends on first having something to compare against.
2. **Check timing** — whether the verdict lands before the surface is published.
3. **Drift coverage** — whether a change made outside the deploy path is caught.
4. **Fail-safe defaults on emission** — whether an unmodelled key is dropped or refused. The
   refusal costs releases; dropping costs exposure.
5. **Authoring cost** — two edits per posture change. The chosen option is the worst here.

## Consequences

Posture becomes reviewable in the same place as the rest of a deployment: a reader can see
which hostnames are open and that someone acknowledged each one. The probe table cannot
drift from the descriptor set, so a hostname added to one and not the other fails a check
rather than being probed for a posture nobody declared. The escape hatch for a
configuration key the descriptor does not model is checked against a fixed reserved set
rather than against whatever keys a descriptor happened to emit.

The cost accepted: strictness on emission blocks releases. A descriptor carrying a key the
emitter has not learned fails the deploy, so a provider feature that is new to the
deployment stops a release until the descriptor model catches up. Changing a posture is two
coordinated edits, and forgetting one of them is a red deploy rather than a wrong
deployment — which is the trade, but it is still a stop.

Drift between deploys remains uncovered: a policy detached an hour after a deploy is not
noticed until the next one.

## Revisit triggers

- Posture drift is observed between deploys often enough that a deploy-time probe is not
  the right cadence.
- The reserved-key set requires frequent extension, indicating the descriptor model is
  chronically behind the provider surface.
- A provider begins recording gate intent in its own configuration in a form a check can
  read, removing the recoverability gap that this decision exists to close.
