# 0219 — A keyed hash standing alone is refused over an exhaustible value space

**Status:** accepted 2026-09-18
**Decides:** `enforcement.mask.refusal.digest-alone-over-an-exhaustible-class`

## Context

A keyed hash is the mask strategy that keeps a column joinable. Drop nulls the cell and takes
equality filtering with it; a digest preserves equality, so two rows carrying the same original
value carry the same masked value and a join still lands. Both enforcement layers compute the
same digest — natively where bytes are written, as an expression inside the registered
relation where they are read — so a value masked at one layer joins against the same value
masked at the other.

The key for that digest is a project pepper, supplied by one environment variable and resolved
once for both layers. Its secrecy is the whole of the digest's strength, and its custody is
weak in a specific, predictable way: the query-time expression carries key-derived pad blocks
in the relation body, leaving the pepper legible to anyone reading the session catalog. The
threat this strategy is chosen against — a stolen object-store credential, an exfiltrated
column — is a threat that reaches the same deployment the pepper lives in.

That matters differently depending on the column's value space. Over random identifiers and
opaque tokens, an attacker who holds the pepper still has to know which value to test, and the
space offers no enumeration. Over a national identifier, a phone number, an email address or a
medical record number, the space is enumerable end to end: the attacker computes the digest of
every candidate and reads the column back as cleartext. The digest is then not a mask but an
encoding, and the operator who declared it believes otherwise.

## Decision

A keyed hash standing alone over a column declared a national identifier, a phone number, an
email address or a medical record number raises `EnforceDigestAloneOnExhaustibleClass`. A
column whose values are random identifiers or opaque tokens carries a keyed hash with no
secondary. Truncation is the secondary that satisfies the refusal, collapsing many inputs onto
one output so an exhaustive search over an enumerable domain returns a crowd rather than a
person.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse a bare digest over the declared exhaustible classes; admit it behind truncation** *(chosen)* | Recovery over an enumerable domain returns a set, not an identity, even for an attacker holding both the column and the pepper. Equality filtering survives. | A join on such a column lands on a crowd, so a query needing a per-person join does not get one. |
| Accept a hash alone and rely on the pepper's secrecy | Nothing to declare beyond the strategy; exact joins keep working. | Loses on cost of recovery: the pepper sits in the relation body and the same credential reads both, so the attacker holding the column holds the key. |
| Refuse hashing outright for these classes | No reversible encoding presented as a mask, under any configuration. | Loses on utility: equality filtering and joins are the reason a hash is chosen at all, and truncation keeps them while collapsing the domain. |
| Require tokenization for these classes | Format preserved, downstream consumers unchanged. | Loses on the same criterion as the bare hash: tokenization is reversible under a key the deployment also holds. |

## Criteria

1. **Cost of recovery to whoever already holds the masked column** — how much work turns the
   masked value back into the original. This is the criterion that decided it.
2. **Utility retained** — whether equality filtering and joins survive the mask. Outright
   refusal fails this.
3. **Whether the declaration means what the operator thinks it means** — a bare hash over an
   enumerable domain reads as protection and is not.
4. **Precision of the join afterwards** — whether a masked column still identifies one row.
   This is the criterion the chosen option loses on.

Recovery cost decides it. The other criteria are about convenience and legibility; this one is
about whether the layer does anything at all against its stated adversary. Against an attacker
who holds only the digest and not the pepper, every option scores the same — and that attacker
is not the one the write-time layer was built for.

## Consequences

The exhaustible classes are a short, declared list rather than a judgment, so an operator
reading the manifest can tell which columns the guardrail covers. High-entropy columns keep the
exact-join behavior with no secondary, which keeps the common case simple.

The accepted cost: a joinable column over an enumerable domain is joinable only into a crowd.
A query that needs a per-person join on a phone number or an email address does not get one,
and the operator either restructures around a stable internal identifier or accepts the
aggregate. How large that crowd is depends on the truncation width and the column's real value
space, which the manifest does not state — so the size of the protection is unmeasured here
and rests on the width the operator chooses.

Reversing this is cheap in mechanism and expensive in consequence: admitting a bare digest over
these classes turns every column already masked that way into a reversible encoding, with no
signal that it changed.

## Revisit triggers

- The pepper moves out of the relation body, so the query-time expression no longer leaks the
  key to a reader of the session catalog.
- A digest construction is adopted whose per-candidate cost makes enumeration of these domains
  impractical rather than merely tedious.
- A declared class turns out not to be enumerable in a deployment's actual data, or a class
  outside the four is found to be enumerable in practice.
