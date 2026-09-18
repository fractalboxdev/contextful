# 0234 — A credential granting a wildcard inference zone is refused at issuance

**Status:** accepted 2026-09-18
**Decides:** `enforcement.place.refusal.wildcard-on-the-subject-side`

## Context

Zone resolution compares two things: the allow-set a table carries and the zone the calling
session asserts. The wildcard `*` is a legal allow-set entry and the only entry admitting an
undeclared zone — no category entry admits it, a public-cloud entry included. On the data
side, `*` is therefore a meaningful statement by an operator: this table is fit for any
model, wherever it runs, including a caller who declined to say.

The same spelling can appear on the other side. A credential can carry a zone tag fixing
where its holder runs, and nothing in the string grammar prevents that tag from being `*`.
But the two sides are not symmetric. An allow-set is a permission an operator grants over
data they control; a zone tag is a factual claim about where a process runs. A process runs
somewhere. `*` on the subject side asserts no location at all, which is the undeclared case
written in a spelling that reads as broad rather than as absent.

The consequence follows from how undeclared is treated everywhere else. Undeclared is the
most restrictive value: it is admitted by `*` alone and reaches nothing else. If a
wildcard tag were read as admitting everything, the one field the caller fully controls
would invert the fail-closed direction — a caller could reach every table by declining to
say where it runs, dressed as declaring everywhere.

Issuance is also where the mistake is cheapest. A credential is minted once and used many
times; a tag fixed at issuance travels with every request the holder makes, and a holder
carrying a grant that reads as broad will act on that reading.

## Decision

A credential granting a wildcard zone raises `EnforceWildcardZoneOnSubject` at issuance.
A wildcard belongs to the data's declaration. A caller whose zone varies per call carries no
zone tag on the credential and asserts the concrete zone on each request.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse a wildcard zone tag at issuance** *(chosen)* | The mistake is caught once, before the credential exists, and the wildcard keeps one meaning: an operator's statement about data. | A caller whose zone genuinely varies per call sets the tag per call rather than once at issuance, which is per-request work the credential could otherwise have carried. |
| Accept it and treat it as undeclared | Fail-closed, consistent with the string grammar's treatment of unparseable text, no new refusal. | Loses on legibility of the grant: the holder carries a credential whose zone field reads as permissive and behaves as the most restrictive value, which is a discrepancy that surfaces as unexplained empty results. |
| Accept it and treat it as admitting everything | The spelling means what it looks like, and the two sides use one vocabulary uniformly. | Loses on fail-closed direction: it inverts the one field the caller controls, so declining to declare becomes the widest possible assertion. |
| Accept it and resolve per request against the table, warning at issuance | Nothing is refused; the operator is told. | Loses on where the mistake is cheapest — a warning at issuance is a warning nobody reads, and every request the holder makes thereafter carries the ambiguity. |
| Use a distinct spelling for a subject-side "varies per call" | Expresses the legitimate intent behind a wildcard tag without overloading `*`. | Loses on necessity rather than on safety: an absent tag already means the zone is asserted per request, so a second spelling adds vocabulary for a case already covered. |

## Criteria

1. **Which side a wildcard is checkable on** — whether the value states a permission the
   system can honor or a fact the system cannot verify. *This criterion decides.* An
   allow-set entry is a grant and a zone tag is a claim, and admitting the same token on
   both sides makes the caller the author of a permission over data the caller does not own.
2. **Fail-closed direction on caller-controlled fields** — whether an omission or an
   ambiguity narrows or widens.
3. **Where the mistake is cheapest to catch** — issuance happens once; requests happen
   continuously.
4. **Legibility of the credential a holder carries** — whether the grant's fields read as
   what they do.

## Consequences

The wildcard keeps one meaning across the system, so a reader of a manifest or a credential
knows which party's statement they are looking at without consulting the field's position.

The cost accepted is per-request work for a legitimate case. A caller that genuinely runs in
different places at different times — a worker pool spanning environments, an agent whose
steps land on different hosts — sets the zone tag on each request rather than once at
issuance, and a request that forgets resolves as undeclared, which is the most restrictive
value and produces empty results rather than an error.

The refusal also lands on an operator who wrote `*` meaning "this holder may run anywhere",
which is a sensible thing to want and is not expressible on this side. That intent is
expressed on the data side instead, by the tables the holder reads.

Reversing toward acceptance is expensive in the "admits everything" direction because the
undeclared-is-most-restrictive rule is relied on throughout placement; it is cheap in the
"treated as undeclared" direction, which is what a later record would most plausibly choose.

## Revisit triggers

- Callers with genuinely varying zones become common enough that per-request tagging is a
  measured source of undeclared-zone reads and the empty results they produce.
- A verified zone attestation appears, making the subject-side value a checkable fact rather
  than a claim, at which point a wildcard on that side could carry a defensible meaning.
- Issuance-time refusal is observed pushing operators to omit the zone tag entirely where
  they meant to constrain it, which would make an accept-as-undeclared reading the safer
  outcome.
