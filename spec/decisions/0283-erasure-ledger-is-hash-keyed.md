# 0283 — The erasure ledger and the receipt hold digests, never cleartext identifiers

**Status:** accepted 2026-09-18
**Decides:** `accountability.receipt.refusal.cleartext-identifier`

## Context

Erasure needs durable state. A ledger row opens ahead of the first tombstone and takes its
completion timestamp after the last commit, so an interrupted operation leaves an open row
naming what was in flight. A re-run reads the last completed request for the same key before
opening its own, which is where idempotence comes from. And a rebuilt catalog still carries
every completed request, since the ledger counts as engine journal state rather than derivable
cache.

That state outlives the erasure it records — necessarily, since its job is to answer
questions after the fact. Which makes its contents a problem of their own kind: a ledger of
erasure requests keyed on cleartext identifiers is a durable, queryable, backed-up list of
exactly the people and tenants who asked to be forgotten, retained precisely because the
erasure happened. The record of the removal re-discloses what was removed, and it does so to
every future reader of the catalog, including one arriving with a subpoena.

The receipt has the same shape and travels further. It is signed, retainable after the
erasure it attests, and handed to a counterparty who keeps it in their own audit file.

What the ledger and the receipt are actually used for does not need cleartext. Idempotence
needs equality against the key of a prior request. Verification needs equality against an
identifier the verifier already holds — they are checking a receipt for their own tenant, so
they have the identifier in hand. Both are equality tests, and a digest supports an equality
test exactly as well as a cleartext value does.

## Decision

`tenant_hash` and `subject_hash` are written `sha256:<hex>`, one of the pair null per path,
and the ledger holds no cleartext identifier at any position. The verb accepts a cleartext
subject or tenant key, hashes it at the command boundary, and writes nothing but the digest
onward. The receipt carries no cleartext identifier at any position and a receipt carrying a
tenant identifier in cleartext raises `ReceiptIdentifierLeak` before the signature is applied.
Verification recomputes `tenant_hash` from an identifier the verifier already holds. The same
digests are what the chain entries carry, so the chain confirms an erasure reached every
derived artifact while naming nobody.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Digests everywhere, hashed at the command boundary** *(chosen)* | The durable record of an erasure cannot re-disclose who was erased, and verification still works with no key custody and no store access. | No operator-facing lookup by name: answering whether a person was erased requires recomputing the digest, and a lost identifier makes the row unreadable permanently. |
| Cleartext subject and tenant identifiers | Operators read the ledger directly; support answers "was this person erased" by eye. | Lost on re-disclosure: the ledger becomes a durable list of exactly the people who asked to be forgotten, retained indefinitely and reachable by anyone who reaches the catalog. |
| Encrypt the identifiers under the project key | Reversible for an authorized operator; the lookup-by-name workflow survives. | Lost on credential-free verification: verification then needs the key, so a counterparty checking a receipt cannot, and the plaintext exists behind one key whose compromise restores the whole list. Reversibility is the property being avoided. |
| Keep no ledger at all | Nothing durable to leak. | Lost on support for idempotence: idempotence and the completion timestamp both derive from it, so a caller retrying after a timeout would trigger a second full rewrite and hold no evidence the first one finished. |
| Hash, but keep a separate operator-side identifier map | Digests in the durable record, names available to the operator who needs them. | Lost on re-disclosure: the map is the cleartext list, with one more place to forget to delete it. It re-creates the rejected option and adds a synchronization problem. |
| Salt the digest per deployment | Resists a dictionary attack over a small identifier space. | Lost on custody cost: the salt has to reach every verifier for verification to work, so it is published, and a published salt defeats its purpose while adding custody. |

## Criteria

1. **Re-disclosure** — whether the record of an erasure can itself reveal who was erased, to
   a reader with access to the catalog or to the receipt. Cleartext and the side map both
   fail this; encryption fails it under one key compromise.
2. **Credential-free verification** — whether a counterparty checks a receipt with nothing
   but the artifact, a pinned public key and an identifier they already hold. Encryption
   fails this.
3. **Support for idempotence** — whether a re-run can find the prior request. No-ledger
   fails this.
4. **Custody cost** — how many secrets the scheme adds. Encryption and salting both add one.
5. **Operator legibility** — whether the ledger is readable by a human without tooling. This
   is the criterion the chosen option loses on.

Re-disclosure decides it. The ledger outlives the erasure it records, so what matters is
whether it survives a later subpoena without naming the subject, and only a one-way digest
does. Credential-free verification is what eliminates the reversible alternative specifically:
the property that makes encryption attractive to the operator is the property that makes the
counterparty unable to check the artifact on their own.

## Consequences

The durable artifacts of erasure — the ledger, the chain entries, the receipt — form a
complete, checkable account of what happened while naming nobody. A receipt can be retained
lawfully after the erasure it attests, and a counterparty verifies it offline by hashing the
identifier they already have.

The accepted cost is that these records are not browsable. There is no "list everyone erased
last quarter" with names in it, and answering whether a particular person was erased means
recomputing their digest — which requires still having their identifier. Where the identifier
is itself gone, the row is unreadable permanently, and that is a design outcome rather than a
defect: the ledger deliberately cannot be turned back into a list of people.

Reversing this is expensive in the direction that matters. Adding cleartext later means every
row written under the digest scheme stays opaque, so the two eras of the ledger answer
different questions.

## Revisit triggers

- Identifier spaces in real deployments turn out small enough that an unsalted digest is
  reversible by enumeration, which would force a keyed construction and re-open the custody
  question.
- A regime requires the erasing party to produce a named list of erasure requests, pricing
  re-disclosure against the obligation.
- Verification moves to a setting where the verifier does not already hold the identifier, at
  which point equality against a digest stops being sufficient.
