# 0193 — Issuance refuses a grant of write or execute whose subject names no on-behalf-of principal

**Status:** accepted 2026-09-18
**Decides:** `authority.issue.refusal.principal-required`

## Context

Every row this engine lands carries an authorship column naming the verified principal the
write was authored for. That column is not decoration. Per-principal placement policy reads
it, so a row authored by a principal pinned to local processing stays pinned wherever the
row travels. Identity links join on it. The audit record names it. It is the one member of
the subject tuple an identity provider checked, and everything downstream treats it as
identity rather than as a claim.

An absent authorship value already carries a meaning: no capability credential authorized
this write. The project's own owner operating the tree directly, and a run a timer fired,
both land rows that way, and the placement rule resolves such a row as restrictively as an
undeclared zone. That reading holds only while absence has one cause.

A credential carrying a write or execute grant and naming no `on_behalf_of` would supply a
second cause. Rows landed by it read back identical to rows the owner landed, and the two
are not the same thing: one has no credential behind it, the other has a credential whose
holder is unaccounted for. Once written, the two are indistinguishable in the file, because
the distinction was never recorded.

Write and execute are the two actions whose exercise lands rows. Read exercises nothing that
needs an author, which is why the default action set of a mint is read alone and an ordinary
mint meets this condition without a caller thinking about it.

## Decision

Issuance refuses a grant of write or execute whose subject names no `on_behalf_of`, raising
`IssuancePrincipalRequired`. A row-landing credential names the principal its rows will
carry, decided at the mint by the party that verified it. An absent authorship value on a
landed row therefore has exactly one cause — nothing capability-bearing authorized the
write.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the unbound row-landing grant at the mint** *(chosen)* | The authorship column holds one kind of value and its absence holds one meaning, decided where the principal is verifiable | A caller that wants a write credential and has no principal to name has no path; the rule binds issuance alone, so credentials minted before it keep verifying until they lapse |
| Stamp the unbound case with a constant sentinel | Distinguishes the two absences, and every row carries a value | Lost on the meaning of the column: it holds the verified principal, and a constant is not one. Per-principal policy would have to special-case it, and a link joining on it would join rows that share nothing |
| Leave both cases null and disambiguate from context later | No mint-side rule; nothing to enforce | Lost on repairability. The ambiguity is durable in files already written, and the context that would resolve it — which credential was presented — is not in the row |
| Refuse at the write rather than at the mint | Catches credentials minted anywhere, including outside the project tree | Lost on where the failure lands: the credential verifies, admits, and fails at its first useful action, in a caller that cannot fix it. The mint is the one moment the principal can still be supplied |

## Criteria

1. **Whether the authorship column keeps one meaning** — whether a reader of a landed row
   can say what a null means. *This criterion decides.*
2. **Repairability** — whether an ambiguity introduced here is fixable after the rows exist.
3. **Where the refusal lands relative to its cause** — whether the party that can supply the
   missing value is the party that sees the error.
4. **Reach** — which credentials the rule actually binds.

Criterion 1 outranks the rest because the column is read by machinery that cannot ask.
Placement policy resolves a row with no verified author to the most restrictive setting; it
has no way to distinguish a row that is restrictive by design from one that is restrictive
by accident of an under-specified mint. Criterion 2 reinforces it: a wrong answer here is not
a bug to fix but a set of files whose provenance is unrecoverable.

## Consequences

Rows carry provenance that resolves, and the restrictive default for authorless rows stays
honest rather than absorbing a second population. Read credentials are untouched — the
default action set is read, so the common mint never meets this condition and names no
principal.

The cost accepted is reach. The rule constrains issuance and not verification, so a
credential minted before this holds, or minted by a path outside the project tree, keeps
admitting and keeps landing authorless rows until it lapses. Closing that would mean
refusing at the checkpoint, which moves the failure away from the only party that can
supply the value.

Reversing this is cheap at the mint and expensive in the data: dropping the rule immediately
admits a second population into the null authorship value, and the rows written under it
carry no marker separating them from the owner's own.

## Revisit triggers

- A legitimate row-landing workload appears whose subject genuinely has no principal and is
  not the project owner or a timer — an automation acting for an organization rather than a
  person would be the shape.
- The authorship column stops being the key per-principal policy and identity links read,
  which removes the reason its meaning must resolve.
- A verification-side check becomes affordable without moving the failure away from the
  minting caller, which would extend the rule to credentials the mint never saw.
