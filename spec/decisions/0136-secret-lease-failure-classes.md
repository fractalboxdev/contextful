# 0136 — A mint failure maps to a class by status, and an expired lease produces no vendor traffic

**Status:** accepted 2026-09-18
**Decides:** `secret.lease.refusal.malformed-lease`, `secret.lease.refusal.mint-rejected`, `secret.lease.refusal.request-rejected`

## Context

The mint endpoint is a network hop that stands between a declared name and every request
that name authorizes. When it fails, the engine has one decision to make and very little to
make it with: retry, or stop. The retry schedule is the engine's ordinary one — bounded
attempts with exponential backoff — so the question is which failures enter it.

Three conditions hide behind the same "the mint call failed" surface, and they want opposite
handling. A deliberate revocation — the mint credential was withdrawn, or the scope was
taken away — clears only when an operator acts, and retrying it spends the remaining window
issuing calls that are designed to be rejected. An operator error — a wrong scope spelling,
a malformed body, a wrong endpoint — also clears only by an edit. A condition that clears on
its own — the provider restarting, a transport reset, a rate limit — is exactly what retry
exists for.

A malformed lease sits awkwardly between them. A response carrying no expiry, or an expiry
already in the past, looks like a contract violation, which reads as permanent. But the
party that produced it is a provider mid-rollout, and it recovers by itself.

There is a fourth condition that is not about the mint call at all. A run holding a cached
lease that has expired, whose provider is now unreachable, has a credential in memory. The
tempting move is to send it: the vendor may still honor it, and if it does not, the vendor
says so. What that produces is a burst of rejected requests at the vendor, attributed to the
deployment, diagnosed from a vendor-side 401 that names nothing about the provider hop that
actually failed.

## Decision

A `401` or `403` raises `SecretMintRejected`, classed `auth_expired`, with no retry
attempted. Any other `4xx` raises `SecretLeaseRequestRejected` as a permanent configuration
fault. A `5xx` answer and a transport error are transient, and the engine's retry schedule
governs what follows. A lease carrying no declared expiry, and one whose expiry has passed on
arrival, raise `SecretMalformedLease` and count as transient. A `429` carries its
`Retry-After` value into the engine's run-level retry, classed `rate_limited`. A run holding
an expired cached lease whose provider is unreachable issues no requests at the vendor; the
failure surfaces at the provider hop.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Class by status: auth, config, transient; fail at the hop** *(chosen)* | Each failure gets the handling its cause admits, and a failure is named at the component that produced it | A provider that answers `400` for an unknown scope reads as a permanent config fault with a less specific message than the `404` path gives |
| Retry a `401` | Survives a provider that briefly answers `401` during a credential reload | Lost on deliberate revocation: the mint credential was withdrawn on purpose, and retrying spends the window issuing calls whose rejection is the point |
| Treat a malformed lease as permanent | A contract violation is reported as a contract violation, sharply | Lost on provider rollout: a provider deploying a bad response shape recovers without operator action, and a permanent class turns a self-clearing condition into a stopped run |
| Retry every `4xx` uniformly | One rule, no status table | Lost on both of the above at once: it retries revocations and it retries typos, spending the schedule on conditions that cannot clear |
| Fall back to a cached expired lease | A run near a boundary survives a provider blip | Lost on vendor impact: the run issues rejected requests at the vendor instead of failing at the hop it can name, and the diagnosis lands on the wrong party |

## Criteria

1. **Whether the condition clears on its own** — deliberate revocation, operator error, or a
   self-clearing condition. *This is the criterion that decided the status mapping.* Retry is
   only ever right for the third, and the cost of getting it wrong is asymmetric: retrying a
   revocation burns the window and produces nothing, while not retrying a blip costs one run
   that a schedule would have saved.
2. **Where the failure is named** — that a reader diagnoses at the component that failed.
   *This is the criterion that decided the expired-cache behavior*, and it outranks
   survivability there because a vendor-side rejection carries no pointer back to the
   provider hop, so a deployment debugging it starts in the wrong place entirely.
3. **Message specificity** — how precisely a refusal names what an operator should change.
   The `404` path serves it well and the `400` path does not; it lost to the first criterion,
   since a provider's choice of status is not something the engine controls.
4. **Vendor-side blast radius** — how many rejected requests a failure sends to a third
   party. Served by the same decision as the second criterion.

## Consequences

A run that fails on credentials names the hop, the class and the scope, and an operator
reading `auth_expired` knows to look at the mint credential rather than at the vendor. The
retry schedule is spent on conditions that can clear, so a provider restart is survivable and
a revocation is fast to observe. The vendor sees no traffic the engine already knows will be
rejected.

The accepted cost: a provider that returns `400` for an unknown scope lands in the permanent
configuration-fault class with a message that names the status rather than the scope. The
`404` path exists precisely to give that case its specific wording, and a provider that does
not use `404` does not get it. The engine cannot infer the distinction from a `400` body
without guessing at provider-specific shapes, which is know-how the credential path does not
hold.

A second cost: classing a malformed lease as transient means a provider with a permanently
broken response shape consumes the full retry schedule on every attempt before failing.

## Revisit triggers

- Mint providers converge on a machine-readable error body, so that scope-unknown is
  distinguishable from other `400` conditions without provider-specific parsing.
- Observed `401` answers from providers are dominated by transient reload conditions rather
  than by revocations, inverting the first criterion's asymmetry.
- A deployment demonstrates that failing closed on an expired cached lease costs more run
  time than the vendor-side rejections it avoids, measured as failed runs against rejected
  vendor requests.
