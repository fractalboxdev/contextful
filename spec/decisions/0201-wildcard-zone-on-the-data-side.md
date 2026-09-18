# 0201 — Issuance refuses a subject declaring a wildcard inference zone

**Status:** accepted 2026-09-18
**Decides:** `authority.issue.refusal.zone-wildcard`

## Context

An inference zone names where the model consuming a result runs — a local device, an
on-premises environment, a private cloud account, a public cloud vendor, or undeclared. It
is a property of the calling process, and the caller declares it. A zone value is one place.

The subject tuple tolerates wildcards generally. A human, a service account and a
timer-fired run share one tuple shape, and members that do not apply are set to a wildcard or
omitted. Zone is the member for which that general tolerance produces something incoherent,
because a wildcard there would have to mean either "anywhere" or "nowhere in particular",
and the enforcement side already has a value for the second: undeclared, which resolves most
restrictively and is admitted by one allow-set entry alone.

The first reading is worse. Zone widening happens on the data side, in the allow-sets an
operator declares per table, per column and per principal, where a permissive default over a
multi-principal store is itself refused and a protected-class floor cannot be widened without
an explicit override. That is where a decision to let data reach a cloud is recorded and
where it is auditable. A credential whose zone member reads "every zone" inverts the
direction: the presenting party asserts the breadth, and the assertion is self-asserted —
zone carries the self-asserted attestation label, so nothing verified it.

Whichever meaning is chosen, a credential carrying a wildcard zone is a credential whose
placement member decides nothing and circulates until it lapses.

## Decision

Issuance refuses a subject declaring a wildcard inference zone, raising
`IssuanceZoneWildcard`. A zone member names one place a consuming model runs, or the subject
omits it and the caller declares a zone per request. Breadth is expressed on the data side,
in the allow-set a table, column or principal declares.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the wildcard at the mint** *(chosen)* | The zone member holds one place and nothing else, and every widening decision stays on the side an operator declares and an override gates | A subject that genuinely spans zones — a batch process fanning work across hosts — mints one credential per zone rather than one covering them all |
| Accept the wildcard and read it as undeclared | Nothing to refuse; permissive input degrades safely | Lost on the meaning of the value: undeclared already exists and resolves most restrictively, so this adds a second spelling that reads permissive and behaves restrictive — the worst combination for anyone auditing a credential |
| Accept the wildcard and read it as admitting every zone | Expresses the multi-zone subject directly | Lost on which side widens: the allow-set is where a widening is declared, gated and overridden, and a credential granting itself every zone routes the decision around all of that, on a member nothing verified |
| Refuse at the checkpoint rather than at the mint | Catches credentials minted by any path, including outside the project tree | Lost on repairability: the credential exists, circulates and fails at first use, with the meaningless member already stamped and signed. The mint is where the subject is still editable |
| Leave the zone member out of the subject entirely, declaring it per request always | One place a zone is stated; no mint-side rule at all | Lost on effort and on what the member is for: pinning a subject to a zone at the mint is how a credential is constrained to a locality in advance, which a per-request declaration cannot do |

## Criteria

1. **Which side of the placement decision expresses breadth** — the data an operator
   declares over, or the credential a caller presents. *This criterion decides.*
2. **Whether the member holds one meaning** — whether a reader of a subject can say what its
   zone is.
3. **Repairability** — whether an incoherent value is fixable once minted and signed.
4. **Expressiveness for a subject spanning several zones.**

Criterion 1 outranks the rest because placement is the question authority deliberately does
not answer. Authority decides who reads what; placement decides where the answer is
processed, and the two compose — one session legitimately holds a grant over protected
records and a key to a public-cloud model, and those records stay inside the on-premises
boundary. A wildcard on the credential is placement authority migrating onto the side that
answers the other question, asserted by the party with the least standing to assert it.
Criterion 4 is the live cost.

## Consequences

Reading a credential's zone gives one place. Reading where a table's data may be processed
means reading the table's allow-set, its column narrowings and its per-principal pins, which
is where the protected-class floor and the override flag live. The two questions stay
separable, and neither surface can quietly answer the other.

The cost accepted is per-zone issuance for multi-zone subjects. A process that fans work onto
hosts in several zones holds a credential per zone, which is more credentials to mint,
distribute and lapse. A subject that omits the member and declares a zone per request avoids
that, at the price of not being pinned to a locality in advance.

Reversing toward an admitting wildcard is a mint-side change that immediately makes a
credential capable of overriding a data-side declaration, and credentials already issued
under it cannot be told apart from ones intended narrowly.

## Revisit triggers

- A multi-zone workload's credential count becomes an operational burden measured in mints
  per deployment rather than anticipated.
- The zone member gains a verified attestation, which would change who has standing to assert
  breadth on it.
- A zone constructor is added that legitimately denotes a set rather than a place, which
  would make the wildcard's meaning a question about the grammar rather than about the mint.
