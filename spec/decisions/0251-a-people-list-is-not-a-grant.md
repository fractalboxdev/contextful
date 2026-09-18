# 0251 — A people list inside a content payload confers no read

**Status:** accepted 2026-09-18
**Decides:** `visibility.declare-fidelity.refusal.roster-from-a-payload`

## Context

Content payloads are full of people. A calendar event carries attendees and invitees. A
message carries a sender and recipients. A document carries authors, commenters and
mentions. A customer record carries contacts and an account team. These lists are
structured, reliably present, and available on the same fetch that brings the content
itself — which makes them the most convenient thing in the payload to treat as an access
list.

They are not one. An attendee list records who was invited to a meeting, which is a claim
about interest and scheduling. Whether any of those people can open the meeting note is
decided somewhere else entirely — by the note's own sharing settings, the drive it lives
in, or the workspace that contains it. Attending a meeting confers no read on the note,
and appearing on a deal confers no read on the record. A recipient list is closer, because
delivery genuinely did happen, and still not the same: the recipient holds a copy in their
own mailbox, which is a different object from the one the mirror is governing.

The failure this produces is specific. A mapping deriving grants from a people list
produces a grant table that is neither the source's audience nor a coarser version of it —
it is a different audience, drawn from a different question, that happens to overlap. It
is wider than the truth wherever someone was invited without being given access, and
narrower wherever access was granted through a group or a container that never appears in
the payload. A reader receiving rows through it is receiving them on the strength of an
audience nobody enforced anywhere.

Calling the result `coarse` does not repair it. `coarse` means a container or workspace
signal joined at the grain the source itself enforces, so a reader told the grain knows
what approximation they hold. A participation list is not a grain the source enforces; the
envelope would carry a claim the source never made.

There is a plain test that separates the two. If the person in the list signed in to the
source right now and opened the object, would the source let them? Where the answer comes
from the source's own permission endpoint, that endpoint is the thing to mirror. Where the
source has no such endpoint, it has nothing to mirror.

## Decision

Attendees, invitees, recipients and contacts arriving inside a content payload describe
interest rather than entitlement. A mapping deriving grants from a people list in the
content rather than from the source's permission endpoint raises
`VisibilityRosterFromPayload`. The test a mapping passes is whether the source enforces the
list at sign-in.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Grants come from the source's permission endpoint; a people list in content is refused as a grant source** *(chosen)* | Every grant in the mirror corresponds to something the source itself would enforce, so the mirrored audience is a copy of a real one rather than a plausible reconstruction. | Sources that expose participation and no permission endpoint reach an organization-wide face through a federated leg or an exclusion, and the most-requested corpora are often exactly those. |
| Treat an attendee or recipient list as a roster | Immediate, broad coverage on sources whose permission endpoints are absent, expensive or rate-limited, using data already fetched. | Loses on the sign-in test: attending a meeting confers no read on the note, so the derived audience is a different audience that overlaps the real one and is wider wherever invitation outran access. |
| Treat it as a coarse signal, with the list named as the grain | Honest labeling of an approximation, and the reader is told what it rests on. | Loses on the same criterion: `coarse` promises a grain the source enforces, and a participation list is enforced nowhere, so the envelope would carry a grain claim the source never sent. |
| Use the list to narrow an existing mirrored grant, never to widen | Strictly narrowing, so no disclosure is possible, and the audience gets closer to who actually cares. | Loses on what the mirror is for: it makes the mirror unfaithful in the narrowing direction, and need-to-know narrowing is manifest policy's single job, read as a diff, rather than a mapping's. |
| Permit it behind an explicit operator acknowledgement | Operators who understand the trade can have their corpus; the default stays safe. | Loses on enforceability, at one remove: the resulting table looks like every other mirrored table to every downstream consumer, and the acknowledgement is invisible at the read where it matters. |

## Criteria

1. **Whether the source itself would enforce the list at sign-in** — whether the derived
   grant corresponds to an access decision that exists anywhere. *This criterion decides.*
   The competing criterion is coverage, which is real and pressing; but a grant with no
   enforcing counterpart makes the mirror's central property false — that it reproduces
   the audience the originating system drew — and once that property is conditional,
   nothing downstream can rely on it for any table.
2. **Direction of the error** — whether a derived audience is wider than the enforced one,
   and in which cases.
3. **Corpus coverage on sources with no permission endpoint** — the cost this decision
   spends.
4. **Whether the envelope's claim matches what the source sent** — the grain a `coarse`
   declaration promises.

## Consequences

Every row the join admits traces to an access decision the source would make itself, which
is what lets the overshare report be read as a statement about the organization's real
sharing rather than about the mapping's inferences.

The accepted cost is coverage, concentrated in the sources people ask for most. A calendar
corpus, a messaging corpus without a permission endpoint, a CRM whose record-level
security is not exposed — each reaches an organization-wide face only through a federated
leg or an explicit exclusion, and both of those are more work than reading a list already
present in the payload. That gap is a standing source of pressure on this decision, and it
is the reason the refusal is by name rather than by convention.

People lists remain available for everything that is not authorization — ranking signals,
entity extraction, answering questions about who attended — since the refusal governs
projection into `access_grants` and not the presence of the data.

## Revisit triggers

- A source is found whose participation list is the same object its sign-in check
  consults, making the list an access list rather than a proxy for one.
- The federated path for participation-shaped sources is built and measured, which would
  change how much the coverage cost actually bites.
- Sources begin exposing per-item permission endpoints that were previously absent, which
  would shrink the class this decision excludes without changing the rule.
