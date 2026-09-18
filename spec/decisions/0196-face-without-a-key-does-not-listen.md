# 0196 — A face configured with no issuer key declines to start and prints the command that fixes it

**Status:** accepted 2026-09-18
**Decides:** `authority.issue.refusal.missing-issuer-key`

## Context

A served face admits one thing, a verified capability credential, and verification needs
pinned issuer key material. With no key configured, a checkpoint can check nothing. Every
request that arrives is refused, and refused for a reason no caller can act on — the
credential they hold is fine, the face has nothing to check it against.

That produces a specific failure shape. The process binds its port, answers a liveness
probe, appears in an orchestrator as healthy, and serves an authentication error to every
request. The gap between "the process is up" and "the process can admit anybody" is exactly
the gap a deployment pipeline reads as success.

The misconfiguration itself is ordinary: a key reference that was never set, an environment
variable dropped between a template and a deployment, a first start before provisioning ran.
It is a common state, not an exotic one, and the operator fixing it needs one fact — which
command provisions the key — at the moment they read the failure.

## Decision

A face configured with no issuer key raises `IssuerKeyMissing`, declines to start, and
prints the command that fixes it. The process does not bind a port it cannot serve from.
A set-but-unresolvable reference refuses the same way rather than falling back onto a
fabricated local key.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse at startup, naming the fixing command** *(chosen)* | Misconfiguration surfaces at the one moment an operator is watching, and the health signal keeps meaning what it says | A pipeline that starts the face before provisioning its key fails at boot rather than coming up degraded, so ordering between the two steps becomes a real dependency |
| Listen and answer an authentication error on every request | The process is reachable, and a probe confirms the binary runs | Lost on observability: a health check reads the process as up while no caller can ever be admitted, and the diagnosis lands on callers rather than on the operator |
| Generate a key on first start when none is configured | Always starts; local development needs no provisioning step | Lost on credential-population coherence: the key differs per start and per replica, so credentials minted against one instance verify nowhere else, and a restart silently invalidates everything outstanding |
| Start with no key and admit everything until one is configured | No failed starts at all | Lost outright on threat — it is an unauthenticated owner path with a timer on it, and the window has no upper bound |
| Refuse, without printing a fixing command | Same failure behavior, less to maintain | Lost on time to repair: the state is common enough that the operator reading the error is usually one command from done, and the error is where they are looking |

## Criteria

1. **Whether the operational signal matches the operational state** — whether a process
   reporting healthy can serve anyone. *This criterion decides.*
2. **Coherence of the credential population** — whether a credential verifies at every
   instance it should.
3. **Time to repair from the error text.**
4. **Tolerance for start-order mistakes in a deployment pipeline.**

Criterion 1 decides because the alternative is not a worse error message but a false one. A
face that listens and rejects is indistinguishable, to everything watching it, from a face
that works — the rollout goes green, the alarm stays quiet, and the outage is discovered by
whoever tries to use the system. Criterion 4 is the cost, and it is the kind that fails
loudly at the moment it is introduced.

## Consequences

Provisioning becomes an explicit prerequisite of running, and an operator who gets it wrong
reads the fix in the failure. A liveness probe keeps its meaning: a face that is up admits
correctly formed credentials.

The cost accepted is rigidity in deployment ordering. A pipeline that starts the face and
provisions its key concurrently now fails, and the fix is real sequencing work rather than a
retry. Restart loops are the visible symptom, which is noisier at boot than a silent
degraded state — deliberately so.

Reversing toward start-and-reject is a one-line change and would restore a class of outage
that no monitoring signal reports.

## Revisit triggers

- A supported platform makes key material available only after the process is listening,
  which would make refusal at startup unsatisfiable.
- A pre-flight configuration check runs before start in every supported deployment shape,
  which would move this refusal earlier and leave the face's own check redundant.
- Local development is found to need a running face before any issuer key exists, in a shape
  the exchange provisioning path does not already cover.
