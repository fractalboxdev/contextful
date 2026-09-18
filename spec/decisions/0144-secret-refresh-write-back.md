# 0144 — A refused write-back fails the run closed rather than continuing on the superseded version

**Status:** accepted 2026-09-18
**Decides:** `secret.rotate.refusal.write-back-denied`

## Context

The OAuth mechanism divides work between the host and the customer's manager. The host drives
the refresh loop from the connector's declared auth policy, ahead of the declared expiry
rather than waiting for a rejected vendor call, and writes the resulting token into the
manager as a blind versioned put. The manager holds versioned ciphertext and learns neither
the provider nor the lifetime.

That put can be refused. The engine's identity may hold read access and not write, a policy
may have tightened, a version may conflict. When it is, the engine is holding two things: a
fresh token that works, and a token in the store that is now the superseded one. Continuing
the run on either is possible. The run completes, the rows land, and nothing is visibly wrong.

What that conceals is the state of the rotation. A deployment that believes rotation is
working has no signal that its write path is broken. The next run refreshes again, is refused
again, and continues again. This repeats until the vendor revokes the version the store still
holds — at which point every source bound to that name fails at once, at the vendor, with an
error that says the token is expired and nothing at all about a write that was refused weeks
earlier.

Retrying the put does not help. The conditions that refuse it — a missing grant, a policy —
do not clear on their own, and an unbounded retry converts a fast failure into a hung run
against the same wall.

## Decision

A manager refusing the versioned put raises `SecretRotationWriteDenied`, and the run fails
closed rather than continuing on the superseded version. A leased credential, which re-mints
with no write-back anywhere, is unaffected: a re-mint that fails leaves the source with no
credential rather than an older one.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Fail the run closed on a refused put** *(chosen)* | A broken write path is named at the moment it breaks, at the component that refused | A deployment granting the engine read-only access to its manager cannot run the OAuth mechanism at all, and learns this at the first refresh rather than at configuration |
| Continue on the superseded version and warn | No run fails on a condition that does not stop the current request | Lost on concealment: the failure surfaces later as a vendor rejection with no link to the refused write, and the deployment believes a rotation happened when it did not |
| Continue on the fresh token, held in memory only | The current run gets the newest credential and completes | Lost on the same concealment, plus divergence: the store and the engine now hold different versions and no later reader can tell which is current |
| Retry the put indefinitely | Survives a transient conflict or a brief policy propagation | Lost on bounding: a permissions gap does not clear on retry, and the run hangs instead of reporting |
| Refuse at configuration by probing write access | The deployment learns before the first run | Lost on effort and on fidelity: a probe writes something to prove it can, and a manager may permit a probe under a policy that refuses the real put later |

## Criteria

1. **Whether a deployment can end up believing a rotation happened when it did not** —
   whether the failure is visible at the time it occurs. *This is the criterion that decided
   it.* A run continuing on the old version hides a broken write path until the vendor
   revokes, and a failure discovered by revocation is discovered at the worst moment, in the
   wrong component, by every source at once.
2. **Bounded failure** — that a refusal reports rather than hangs. Rules out unbounded retry.
3. **State agreement between engine and store** — that what the engine uses and what the store
   holds are the same version. Rules out the in-memory-only variant.
4. **Feedback timing** — whether the deployment learns at configuration or at first refresh.
   The chosen option loses here; a probe would win it and fails the fidelity test, so the cost
   is accepted rather than traded.

## Consequences

A refused write is a named failure at the moment it occurs, attributable to the manager and to
the logical name, so the fix is a grant rather than an investigation. The store's current
version and the credential the engine uses never diverge.

The accepted cost: a deployment granting the engine read-only access to its manager cannot run
the OAuth mechanism at all, and learns this at the first refresh rather than at configuration.
That first refresh may be days after the source was set up and running, so the failure arrives
as a regression in something that had been working. A read-only posture is a reasonable thing
for a security team to want, and the answer is that OAuth sources need write access or a
different mechanism — a static key an operator rotates, or a leased credential that writes
back nowhere.

A second cost: a transient write conflict, which would have cleared on the next run, fails a
run that need not have failed.

## Revisit triggers

- Refused puts are observed dominated by transient version conflicts rather than by permission
  gaps, making bounded retry the better first response.
- A manager offers a write-capability check that does not require writing, removing the
  fidelity objection to refusing at configuration.
- A deployment demonstrates a read-only posture it cannot change, with OAuth sources it cannot
  move to another mechanism, making the mechanism unavailable rather than merely constrained.
