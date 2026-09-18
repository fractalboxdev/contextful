# 0135 — The lease provider leads the chain for declared names and never falls through

**Status:** accepted 2026-09-18
**Decides:** `secret.lease.refusal.shadowed-name`, `secret.lease.refusal.empty-scope-set`, `secret.lease.refusal.unknown-scope`

## Context

The resolver assembles a chain of adapters and hydration stops at the earliest one that
answers a name. That rule — first hit wins — is what lets a developer shadow a managed
credential with an environment variable, and what lets a deployment move a name between
backends by re-pointing rather than editing declarations. It is ordinary precedence, and
everywhere it applies, the cost of a shadow is a stale or wrong value, discovered when a
request fails.

A run-time lease is not a value with a different source. It is a different posture: material
with a clock on it, obtained from a provider that grants scopes, so that a compromise of the
engine is bounded by an expiry and a revocation of the mint credential severs every leased
source at the next window. An operator declares which logical names carry that posture.

Precedence and posture pull apart here. A leased name that reaches a stored credential —
because an environment variable survived from development, or a keychain entry was left
behind on a laptop, or a managed-store entry predates the switch — produces a run that works
perfectly. Nothing fails. The vendor accepts the long-lived credential, the rows land, and
the deployment believes it is running on short-lived grants while holding a standing one.
The failure is silent and it is exactly the failure the posture was adopted to prevent.

The same reasoning binds the provider's own miss. A `404` for an undeclared scope, treated
as "this adapter does not answer that name", is a fall-through into the same outcome.

## Decision

For a declared name the lease provider answers and no adapter behind it is reached, whatever
the ordinary precedence would give; falling through on a declared name is not available to
it. An adapter behind the lease provider that also answers a declared name raises
`SecretLeaseShadowed` at first hydration, naming the logical name and the adapter that
shadowed it. A `404` raises `SecretLeaseScopeUnknown` and fails closed as a configuration
fault. Selecting the lease backend while declaring no scope raises `SecretLeaseScopesEmpty`
at startup. An undeclared name leaves the lease provider with no outbound request at all, so
precedence among the remaining adapters is untouched by the lease posture being on.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Lease provider at the head, no fall-through for declared names** *(chosen)* | A declared name is served by the lease provider or the run fails; the posture a deployment declares is the posture it runs | One adapter leads out of declared order, which reads as an inconsistency each time the chain is tidied |
| Place the lease provider last, consistent with first-hit-wins | The chain has one rule and no exception to explain | Lost on posture: a leftover environment variable or an old keychain entry wins, the mint endpoint is never called, and the run proceeds on a long-lived credential while the deployment believes it holds a short-lived one — silently, and indefinitely |
| Make the whole chain lease-only when the lease backend is selected | No precedence question survives at all | Lost on bootstrap: the mint credential would itself need a lease, and nothing mints the first one |
| Let a `404` fall through to the adapters behind | An operator who mistypes a scope still gets a working run | Lost on the same posture criterion, by the same silent route — a typo in a scope name downgrades the posture without any signal |
| Warn on a shadowed name and serve the lease anyway | No run breaks on a leftover entry | Lost on what a shadow indicates: a second copy of a credential the deployment meant to retire is live somewhere, and a warning in a log is not how that gets removed |

## Criteria

1. **What a shadowed name costs** — whether a wrong precedence outcome is recoverable and
   visible, or silent and posture-destroying. *This is the criterion that decided it.*
   Everywhere else in the chain a shadow costs a stale value and announces itself the first
   time a vendor rejects the call. For a declared leased name a shadow costs the posture
   itself and announces nothing, because the stale credential works.
2. **Uniformity of the chain rule** — that a reader learns one precedence rule and applies
   it everywhere. The chosen option spends this deliberately.
3. **Termination** — that hydrating the mint credential cannot re-enter the lease provider.
   The lease-only option fails this outright.
4. **Failure timing** — that a misconfiguration surfaces at startup rather than at first
   vendor call. The empty-scope-set refusal is where this criterion lands.

## Consequences

An operator reads the lease posture off the scope declaration and gets exactly it: either
the declared names mint, or the run stops with the name and the offending adapter printed.
A leftover credential is found at first hydration rather than at a vendor revocation weeks
later. Sizing the mint endpoint is predictable, since undeclared names issue no request.

The accepted cost: one adapter leads out of declared order. Anyone reading the assembled
chain sees an exception, and anyone reorganizing the resolver may "fix" it back into
declared order without noticing what it was for. Both directions need pinning — that the
lease provider leads for a declared name, and that it issues nothing for an undeclared one —
because each half is what makes the other safe to have.

A second cost lands on the operator: a scope typo stops a run that would previously have
worked. That is the intended trade and it is not free; it converts a silent downgrade into
an outage.

## Revisit triggers

- Provider attribution shows shadowed-name refusals firing routinely in steady state rather
  than at adoption, indicating the declaration and the backing store are habitually out of
  step rather than occasionally.
- A mint protocol appears whose unknown-scope answer is not distinguishable from a transport
  failure, making fail-closed on `404` indistinguishable from fail-closed on everything.
- A deployment needs one name served by a lease in one source and by a stored credential in
  another, which the current name-level declaration cannot express.
