# 0131 — Every encrypted entry carries an operator record, and a credential-shaped literal where a reference belongs is refused

**Status:** accepted 2026-09-18
**Decides:** `secret.record.refusal.undocumented-entry`, `secret.reference.refusal.material-in-a-declaration`

## Context

A credential does not describe itself. Given an opaque token, there is no way to determine
what it grants, which account or tenancy it addresses, when it was minted or when it expires.
Most providers cannot introspect a credential's own permissions at all without a second
credential holding a permissions-read grant — a credential most deployments never provision,
because provisioning it is itself a privilege escalation nobody wants to justify.

The practical consequence is that an undocumented entry is unknowable. An operator six
months later, facing a logical name bound to ciphertext, cannot answer whether the credential
can write, which tenancy it reaches, or whether it expires next week — without a console
session and the person who created it, one of which is slow and the other of which has left.
That question is asked at exactly the wrong moments: during an incident, during a rotation,
and while deciding whether a new source can reuse an existing binding.

The credential store holds opaque versioned ciphertext under a logical name and carries no
provider know-how, which is what keeps adapters interchangeable. It follows that the store
cannot hold the description either. The description has to live beside the ciphertext, in
plaintext, in the same committed file — which is safe, because grants, scope, dates and a
rotation location are not secret and are exactly what a reviewer needs.

The second half is the failure mode that makes the first half moot. A declaration binds
credentials by reference; the file that results is safe to commit. An author in a hurry who
pastes the token itself where the reference belongs produces a file that works, passes every
runtime check, and puts the credential in plaintext into the repository's permanent history.
Once committed, it is not removable by editing — the rotation is the only remedy.

## Decision

An entry holding a value with no record above it raises `SecretUndocumentedEntry`, naming
the logical name and the file. The record states the exact permission grants in the
provider's own vocabulary, the account, zone, project or tenancy the credential is scoped
to, its creation date, its expiry — an absent expiry written as such rather than left off —
and where it is minted and rotated. Grants an operator has not established are recorded as
unknown, an unknown marking being a state the record carries rather than an omission. A
credential-shaped literal standing where a reference belongs raises
`SecretMaterialInDeclaration` at validation, naming the key.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse an undocumented entry; refuse a literal in a reference position** *(chosen)* | Every bound credential's grants, scope and expiry are answerable from a committed file, and no credential reaches the repository in plaintext. | An operator adding a credential in a hurry writes four fields before the entry is accepted, and a rotation rewrites two of them. |
| Leaving the record to convention | Zero friction; a disciplined team writes the comment anyway. | Lost on recoverability: an undocumented entry's scope is unknowable without a console session and the person who created it, and convention decays precisely under the time pressure that makes the record matter. |
| Introspecting the provider for grants | The description is derived rather than written, so it cannot go stale. | Lost on availability: introspection needs a separate credential with a permissions-read grant that most deployments do not hold and should not provision. |
| Accepting a literal and warning | The declaration works; the warning prompts a fix. | Lost on irreversibility: the value is already in a committed file in plaintext, and editing does not remove it from history. Only rotation does. |
| Recording the description in the credential backend | Description travels with the ciphertext; no second file. | Lost on adapter neutrality: the backend holds opaque versioned ciphertext and carries no provider know-how, so a description field is a capability every adapter would have to implement. |
| Requiring the record but allowing fields to be omitted | Less friction on entry; partial records beat none. | Lost on the same criterion as convention, at finer grain: an omitted expiry is indistinguishable from a credential with no expiry, which is the field most often wanted and most often absent. |

## Criteria

1. **Whether the grants a credential carries are recoverable at all**, by someone who did not
   create it, without a console session.
2. **Availability of the mechanism** across the providers a deployment actually uses.
3. **Irreversibility of the failure it prevents.** A committed plaintext credential is
   remediated only by rotation.
4. **Operator friction at entry and at rotation.**

Criterion 1 decides. Criterion 2 is what disqualifies introspection, which would otherwise be
strictly better because a derived description cannot drift from the credential it describes.
Criterion 4 is the cost, and it is the smallest of the four: four fields at entry, against a
question that is otherwise unanswerable forever.

## Consequences

`contextful secrets list` reads records and reaches no value, printing per logical name the
adapter that answers it, the covered scope, the expiry and the days remaining — so an
expiry sweep is one command rather than a round of console sessions, and an entry inside
30 d of expiry is marked. An incident responder reads grants and tenancy out of a committed
file. A pasted credential fails validation before it is bound rather than after it is
committed.

The cost accepted: the record is written by a person and is therefore a claim, not a fact.
Nothing verifies that the stated grants match the credential's real ones — an over-scoped
credential described as narrow reads as narrow — and the only defense is the unknown marking
for grants the operator has not established. Rotation drift is bounded by requiring the
creation date and expiry to be rewritten in the same commit that lands new ciphertext, but
the grants line can go stale silently.

An operator adding a credential under time pressure writes four fields before the entry is
accepted, which is friction at precisely the moment it is least welcome.

## Revisit triggers

- A provider in use exposes credential introspection without a second credential, making a
  derived record possible for that adapter and the written one redundant.
- Records are observed drifting from real grants often enough that the unknown marking is
  doing no work, which would argue for a periodic verification pass rather than a written
  claim.
- The credential backend gains a neutral metadata channel that every adapter can carry,
  which moves the record next to the ciphertext without adding provider know-how.
