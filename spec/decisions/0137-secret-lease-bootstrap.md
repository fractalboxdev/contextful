# 0137 — One standing mint-scoped credential bootstraps leasing, and non-recursion is structural

**Status:** accepted 2026-09-18
**Decides:** `secret.lease.refusal.bootstrap-unserved`, `secret.lease.refusal.bootstrap-declared-leased`

## Context

A lease posture replaces standing vendor credentials with short-lived grants. It cannot
replace all of them: something has to authenticate to the mint endpoint, and whatever that
something is, it is a credential the engine holds continuously. The posture's whole value is
that this one credential is the deployment's entire standing surface — scoped to minting,
granting no vendor access, and revocable in one act that severs every leased source at the
next expiry.

That makes where the bootstrap credential is stored a real question rather than a detail. A
deployment adopts leasing precisely to stop keeping vendor credentials in stores the engine
reads. If the bootstrap credential is then required to live in an environment variable or a
keychain entry, the deployment has been told to put its single most consequential credential
in exactly the class of place it was trying to leave.

The bootstrap reference also creates a termination problem. It is spelled `secret://<name>`
like everything else, so it hydrates through the chain — and the lease provider sits at the
head of that chain. Resolving the bootstrap reference through the full chain means the lease
provider is asked to mint the credential it needs in order to mint.

And because the lease provider does not fall through for declared names, a bootstrap name
that no adapter behind it answers has no second chance. Whether that is an ordinary miss or
a fault decides whether a misconfigured bootstrap fails loudly or degrades into something
else.

## Decision

The engine authenticates to the mint endpoint with one standing credential, itself a
`secret://` reference, scoped to minting and granting no vendor access. The lease provider
holds a handle to the chain as assembled before it joined, so the mint reference hydrates
through adapters behind it by construction rather than by convention.
`CONTEXTFUL_LEASE_BOOTSTRAP_BACKEND` composes a customer-operated manager behind the lease
provider, so the mint reference needs neither an environment variable nor a keychain entry. A
bootstrap name no adapter behind the lease provider answers raises
`SecretBootstrapUnresolved`, naming the reference, rather than falling through as an ordinary
miss. Listing the bootstrap name among the leased scopes raises `SecretBootstrapLeased` at
startup.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **One mint-scoped reference, resolved through the chain behind the provider, with a configurable backend** *(chosen)* | The standing surface is one credential in a store the deployment chose; recursion is impossible by construction rather than by review | A configured bootstrap backend is a second backend to operate |
| Restrict the bootstrap credential to environment or keychain | No extra backend, and bootstrap resolution is trivially non-recursive | Lost on adoption: a deployment adopting this posture to keep credentials out of engine-readable stores is forced to put its one standing credential in exactly such a store |
| Resolve the bootstrap reference through the full chain including the lease provider | One resolution path, no special handle | Lost on termination: infinite recursion, or a fallback that defeats the declaration by quietly serving the bootstrap name from behind |
| Guard recursion with a check at the lease provider | Keeps one chain, refuses re-entry when it happens | Lost on where the guarantee lives: a check holds while nobody removes it, whereas a handle to the chain as it was cannot be re-entered at all |
| Treat an unserved bootstrap name as an ordinary miss | An operator gets the same unresolved-reference error as anywhere else | Lost on fail-closed: with no fall-through available to the lease provider, a miss on the bootstrap name is a configuration fault wearing a generic message |

## Criteria

1. **Size of the standing surface** — how much a deployment holds continuously, and what
   revoking it reaches. *This is the criterion that decided it.* The posture is chosen for
   this and nothing else; an option that pushes the standing credential into a weaker store
   trades away the reason the posture exists, even though it is cheaper to operate.
2. **Termination by construction** — whether non-recursion is a property of the structure or
   a property of a check somebody maintains. The guard option satisfies the behavior and not
   the criterion.
3. **Operational surface** — how many backends a deployment runs. The chosen option loses
   here and accepts it.
4. **Diagnosability** — whether a bootstrap misconfiguration names itself. Decides the
   unserved-name refusal and the startup refusal on a leased bootstrap name.

## Consequences

Revocation is one act with a known reach: withdraw the mint credential and every leased
source stops at the next expiry, with no per-vendor cleanup. The bootstrap credential can
live in the same customer-operated manager that holds everything else, so a deployment that
keeps no credentials on the engine's host keeps none. A configuration mistake in the
bootstrap path — an unserved name, or a bootstrap name declared as a leased scope — is named
at startup or at first hydration rather than appearing as a mint failure.

The accepted cost: `CONTEXTFUL_LEASE_BOOTSTRAP_BACKEND` is a second backend to operate,
configured, reachable and monitored alongside the mint provider, for the sake of exactly one
credential. A small deployment pays a fixed operational price to avoid a single environment
variable, and for some deployments that ratio does not justify itself.

A further cost is concentration. One credential now stands behind every leased source, so
its compromise is broader than any single vendor credential's, and it is the one credential
no lease shortens. What bounds it is scope — it mints and reaches no vendor — not lifetime.

## Revisit triggers

- A runtime workload identity becomes available that authenticates to the mint endpoint
  directly, removing the bootstrap credential rather than relocating it.
- Deployments are observed running the bootstrap backend and no other, indicating the second
  backend is pure overhead in practice.
- A mint provider offers per-source mint credentials, so that the standing surface is
  divisible and the concentration cost can be traded against operational count.
