# 0132 — An external manager read through workload identity is the default posture, and inline material is development-only

**Status:** accepted 2026-09-18
**Decides:** `secret.resolve.refusal.inline-outside-development`

## Context

Four postures order how far outbound credential material sits from the engine. Inline
environment credentials leave plaintext at rest at every call site. Reference plus
just-in-time hydration against a locally stored value keeps plaintext in the host process
and a stored credential on the host. An external manager read through the runtime's own
workload identity leaves the engine holding references and no standing credential to the
store. A secretless broker sidecar has the engine dispatch intent while the sidecar puts the
credential on the request inside the customer boundary, so material never enters the engine's
address space at all.

Two properties separate them, and they are not the same property. The first is whether the
engine holds a standing credential to the credential store — a long-lived key that, if the
engine process is read, unlocks every credential the deployment holds rather than the ones a
request happened to need. The second is whether plaintext ever enters the engine process at
all, which is a strictly stronger condition and the only one the broker satisfies.

Whichever posture is the default is the posture most deployments run, because a default is
what a deployment gets with no further choice and most deployments make no further choice.
So the default is a statement about the security of the median deployment, not about what is
available to a careful one.

Workload identity changes the arithmetic. A runtime that attests the workload to the
credential store removes the standing credential without adding an operational component:
the engine asks the platform who it is and the store answers. That is available on every
major runtime a deployment of this shape runs on, at no operational cost beyond the binding.

The broker buys the stronger property and costs a component. Every broker deployment runs,
monitors, upgrades and debugs a sidecar, and a sidecar in the request path is a failure
domain of its own. Two classes of deployment need it anyway: regulated ones where plaintext
in the application process is itself the finding, and every deployment where a hosted
control-plane component sits in the request path, since the hosted component would otherwise
be inside the trust zone that holds material.

Inline credentials keep a genuine use: a developer running the engine locally against a test
vendor, where the token is disposable and the call-site file is never committed.

## Decision

The external manager read through workload identity is the posture a deployment gets with no
further choice. The broker posture serves regulated and high-assurance deployments, and
covers every deployment where a hosted control-plane component sits in the request path. A
profile other than development binding plaintext material at a call site raises
`SecretInlineCredential` naming the binding.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **External manager through workload identity as the default; broker for regulated and hosted-path deployments; inline refused outside development** *(chosen)* | The median deployment holds no standing credential to the store and no plaintext at rest, with no extra component to operate. | Material sits in the engine's memory for the length of a request; removing that residual costs the deployment a sidecar to run. |
| Inline environment credentials as the default | Nothing to configure; every runtime supports it; a deployment starts in minutes. | Lost on plaintext at rest: material sits at every call site in a file or an environment a process listing exposes, and any read of the host yields all of it. |
| The broker sidecar as the default | Plaintext never enters the engine process; the strongest available property for every deployment. | Lost on operability: every deployment then runs and operates a sidecar and accepts a second failure domain in the request path, to buy a property most deployments do not need. |
| Reference plus just-in-time hydration against a locally stored value | No plaintext at rest at the call site; no external manager to configure. | Lost on the standing-credential criterion: the engine holds a long-lived key to the local store, so reading the process yields every credential rather than the ones in flight. |
| Refusing inline credentials in every profile | One rule, no profile to get wrong. | Lost on development ergonomics: a disposable token against a test vendor is the cheapest way to try a connector, and a misconfigured profile is a visible failure rather than a silent exposure. |

## Criteria

1. **Whether the engine holds a standing credential to the credential store.** This governs
   what one read of the process yields.
2. **Whether plaintext ever enters the engine process.** Strictly stronger, and the property
   the broker exists for.
3. **Operational cost imposed on every deployment**, since the default is what most run.
4. **Availability of the mechanism** across the runtimes deployments actually use.

Criterion 1 decides the default. Criterion 2 is where the chosen default is beaten by the
broker, and it does not decide because it cannot be bought without criterion 3: making every
deployment run a sidecar to remove a request-lifetime memory residual is a trade most
deployments would decline if asked, and a default is the answer for people who are not
asked. Criterion 4 is what makes workload identity a viable default rather than an
aspiration.

## Consequences

A deployment that configures nothing beyond a backend selection holds references and no
standing credential to the store, and the blast radius of reading the engine process is the
credentials in flight rather than every credential the deployment has. A regulated deployment
has a named posture with a named cost. A hosted control-plane component in the request path
does not silently downgrade the deployment, because that condition mandates the broker.

The cost accepted: the default leaves material in the engine's memory for the length of a
request. A memory disclosure, a core dump or a debugger attached at the wrong moment yields
what is in flight. Removing that residual costs the deployment a sidecar to run, monitor and
upgrade — which is the broker posture, available and unchosen by default.

Inline material stays reachable in the development profile, so a deployment that
misconfigures its profile as development gets the weakest posture. The refusal names the
binding, but nothing prevents the profile itself from being wrong.

## Revisit triggers

- Workload identity becomes unavailable or unattestable on a runtime a deployment needs,
  which removes the basis for the default on that platform.
- A broker becomes operable at close to zero marginal cost — bundled with the runtime, or
  supplied by the platform — at which point criterion 3 no longer blocks criterion 2.
- A memory disclosure in the engine's dependencies makes the request-lifetime residual a
  realized loss rather than a modeled one.
