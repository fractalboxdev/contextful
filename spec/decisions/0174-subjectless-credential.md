# 0174 — A credential carrying no subject at all is refused at admission

**Status:** accepted 2026-09-18
**Decides:** `authority.identify.refusal.subjectless-credential`

## Context

Everything downstream of admission reads the subject tuple. An identity link joins the
verified principal against a source's own accounts to decide which rows a caller reaches;
the landed-row column recording who authorized a write reads it; the audit record that
makes an admitted read accountable is built from it. A credential with no subject member
at all is not a credential with a narrow subject — it is one for which every consumer has
nothing to read.

Each of those consumers has a defined behavior for a subject that resolves to nothing.
An unlinked principal reads nothing from a source rather than everything, so a missing
link fails as a denial. That rule is what makes a subjectless credential dangerous rather
than useless: it does not fail. It sails through admission, resolves to no link, and
reads nothing — which is indistinguishable from a correctly minted credential whose
directory link has not been provisioned yet, and from one whose link was deliberately
withdrawn. Three very different situations produce one empty result, and the operator
investigating has no way to tell them apart.

The audit record has no such fallback. A read admitted under no subject produces a record
naming nobody, which is a record that cannot answer the question it exists to answer.

The refusal itself handles attacker-supplied bytes. A credential that fails this check
failed before its claims were trusted, so its contents are not facts about anything.

## Decision

A credential carrying no subject member at all is refused at admission and raises
`AuthoritySubjectMissing`. The refusal names the surface that refused and echoes no value
from the credential. A deployment wanting an unauthenticated read face mints a credential
naming a public subject rather than one naming none.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse at admission; name the surface, echo nothing** *(chosen)* | The one shape that yields an unattributable audit record never gets past the door, and the empty-read population stays diagnosable | An anonymous read face is expressed as a credential naming a public subject rather than as the absence of one |
| Admit with an anonymous or wildcard subject | Anonymous access needs no credential at all; the shape is its own expression of "nobody" | Lost on accountability: the audit record names nobody, so an admitted read that returned rows cannot be attributed to anything afterwards |
| Admit and let the missing link deny the read | No new refusal; the existing unlinked-principal rule already fails closed | Lost on diagnosability: a malformed credential, an unprovisioned link and a withdrawn link all present as one empty result with no way to distinguish them |
| Refuse, and echo the offending credential's contents in the message | An operator debugging a broken template sees what was actually presented | Lost on disclosure: the bytes come from an unverified credential, so the refusal would render attacker-chosen content into operator output and audit records |
| Refuse only where the surface requires attribution | Anonymous surfaces keep the shorter shape | Lost on uniformity of the guarantee: every consumer of the tuple would need its own handling for the absent case, and the guarantee stops being a property of admission |

## Criteria

1. **Attribution** — whether every read that returned rows can be traced to a subject.
2. **Diagnosability** — whether an empty result distinguishes a malformed credential from
   an unprovisioned or withdrawn link.
3. **Disclosure** — what unverified content the refusal path renders.
4. **Expressiveness** — whether legitimate anonymous access remains representable.

Criterion 1 decided it. Diagnosability alone would be served by any distinct signal, and
expressiveness argues mildly for admitting a wildcard. Attribution outranks both because
it is the only one whose failure is unrecoverable: a read that has already returned rows
under no subject leaves a record that no later fix can complete. A denial can be undone
by provisioning a link; an unattributable disclosure cannot be undone at all.

## Consequences

Every admitted read carries a subject, so the audit record, the landed-row author column
and the link join each have something to read and none of them carries an absent case.
An operator seeing an empty result knows it is a link question rather than a credential
question, because a credential question would have refused.

The cost accepted is that a deployment serving a genuinely public read face cannot
express it as the absence of a subject. It mints a credential naming a public subject and
provisions that subject's reach like any other, which is more configuration than a
subjectless credential would have needed and makes the public face visible in the same
places every other reader is visible.

The refusal names the surface and nothing else, so an operator debugging a broken claim
template learns where the refusal happened and not what was presented. Correlating it
with the minting side is the price of not rendering unverified bytes.

## Revisit triggers

- Public read faces become common enough that provisioning a public subject per
  deployment is recurring configuration rather than a one-time act.
- Audit records gain a defined representation for an unattributed read, which would
  remove the unrecoverable failure criterion 1 rests on.
- Operators are found unable to diagnose minting-template defects from the refusal alone,
  which would re-open what the message may safely carry.
