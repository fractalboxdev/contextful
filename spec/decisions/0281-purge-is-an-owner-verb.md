# 0281 — A purge presented with a capability token is refused before the store answers anything

**Status:** accepted 2026-09-18
**Decides:** `accountability.erase.refusal.capability-token-caller`

## Context

A capability token is the credential class this system hands out freely. It is attenuable —
narrowed to a store, a table, a tenant, a window — and it is designed to be delegated onward
to an agent that runs unattended. The whole point of the class is that holding one is not
rare and that a holder can pass a weaker one along.

A tenant purge is the most destructive operation the engine performs. It rewrites every
reachable file retaining the rows outside the named tenant, it reaches held builds that
garbage collection would have skipped, and it holds the per-model write lock across a table's
whole sweep. Nothing undoes it.

The two facts sit badly together, and the sequence in which erasure actually happens settles
how badly. A tenant purge follows offboarding. By the time the request arrives, the tenant's
own credentials are normally revoked — that is what offboarding means — so an authorization
model that depends on the tenant's token is asking for a credential that the preceding step
was supposed to destroy. Deployments would respond by keeping a live token around for the
purge, which reintroduces exactly the credential offboarding removed.

The refusal's position in the request matters as much as its existence. A purge names a
tenant. If authorization is decided after the store has been consulted about whether that
tenant exists, the refusal's timing and wording tell an unauthorized caller whether the
identifier they guessed is real.

## Decision

A purge presented with a capability token is refused with `PurgeRequiresOwner`, and that
refusal is evaluated ahead of anything that would tell the caller what the store contains.
The purge runs under an operator credential, reached through the single audited verb, and its
authorization does not depend on any credential the tenant holds.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse a capability token; require an operator credential, decided first** *(chosen)* | Authorization does not depend on a credential that offboarding has normally already revoked, and an unauthorized caller learns nothing about which tenants exist. | The purge cannot sit in a self-service offboarding flow driven by the tenant's session; it runs under an operator credential, making it a human or control-plane action. |
| Authorize with the tenant's own token | Self-service: the tenant erases themselves, and the authority is obviously correct. | The token is normally revoked by the time the purge runs, so deployments keep one alive for it. It also makes self-purge a denial-of-service surface — a leaked token destroys the tenant's data irrecoverably. |
| Add a dedicated purge grant to the capability token class | Keeps one credential class, attenuable, with the destructive power gated behind an explicit scope. | Puts a highly destructive scope in a token class designed to be attenuated and handed to agents. Every delegation chain then has to be trusted not to carry it, and an attenuation bug becomes data loss. |
| Require an operator credential but decide it after resolving the tenant | Better errors: the caller learns whether the tenant identifier was even valid. | The error itself is an oracle over tenant existence, offered to a caller who has just been found unauthorized. |
| Require two operator credentials | Destruction needs two people, matching how irreversible operations are usually guarded. | The engine has no notion of a second principal, and building one for this verb alone puts an approval workflow inside a storage engine. The deploying organization can require it around the verb instead. |

## Criteria

1. **Credential availability at the moment of use** — whether the authorizing credential
   still exists when the operation is normally performed. The tenant's own token fails this
   by construction.
2. **Blast radius of a leaked credential** — what a stolen token can destroy. The tenant
   token and the purge-scoped capability both put irreversible destruction behind a credential
   built for wide delegation.
3. **Information disclosed by the refusal** — whether an unauthorized caller learns what the
   store holds. Late evaluation fails this.
4. **Fit with the engine's principal model** — whether the rule is expressible in what the
   engine knows about callers. Two-credential approval fails this.
5. **Self-service reach** — whether a tenant can complete their own erasure without an
   operator. This is the criterion the chosen option loses on.

Authorization not depending on a credential that no longer exists decides it. Blast radius
points the same way and reinforces it, but availability is the one that makes the alternative
incoherent rather than merely risky: a design whose authorizing credential is destroyed by
the step immediately preceding its use will be worked around, and the workaround is worse
than the design it replaces.

## Consequences

Erasure authority sits with the operator credential throughout, so the purge composes with an
offboarding sequence that revokes the tenant's access first, in the order that sequence
naturally runs. No capability token, however it was attenuated or delegated, can destroy a
tenant's data.

The accepted cost is that self-service erasure is not available at the engine level. A
product offering "delete my account" builds it in its own control plane and calls the verb
with an operator credential, so the engine's audit chain attributes the purge to that plane
rather than to the person who asked for it — the request's provenance has to be carried
alongside, in the stated reason on the chain entry.

Reversing this is cheap in one direction only: adding a purge scope to the capability class
later is a token-format change every issuer and verifier sees, and every token minted before
it would need to be understood as not carrying the scope.

## Revisit triggers

- The token format gains a scope class that attenuation provably cannot carry forward, making
  a destructive grant safe to delegate.
- A deployment's offboarding sequence is observed keeping a tenant token alive specifically to
  drive the purge, which would mean the rule is being routed around rather than followed.
- The engine acquires a multi-principal approval notion for an independent reason, at which
  point two-credential purge stops being a bespoke workflow.
