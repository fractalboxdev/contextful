# 0276 — The at-rest guarantee travels with its qualifier or is not asserted

**Status:** accepted 2026-09-18
**Decides:** `accountability.attest.refusal.unqualified-guarantee`

## Context

Write-time redaction delivers a real and unusually clean property. A column redacted before it
lands is not in the columnar files, so a stolen object-store credential reads bytes that never
held it, and where the table's sidecar indexes are encrypted under the key covering the table,
the same credential reads nothing from those either. That is worth stating: it is the
difference between a credential compromise being an incident and being a disclosure.

The property is also narrow in ways that are invisible from the sentence describing it. A
column protected at the query layer alone sits in cleartext in the bytes on disk, and a
credential over the bucket reads it — the mask runs in a path the thief is not in. A column
protected at the sync layer is outside the guarantee for the same reason. A vector, full-text
or bloom sidecar left outside the table's encryption carries derived content from the column it
indexes, readable by the same credential. A tokenized column where the same actor holds the
tokenizing key is protected against nobody who has both.

Those exclusions are not caveats about edge deployments. Query-layer masking is the ordinary
way a column gets protected, so the most common protection in a deployment is precisely the one
the at-rest guarantee does not cover. An operator who reads "redacted columns are unreadable
from a stolen credential" and holds a deployment where masking is done at query time has been
told something true about a mechanism they are not using.

The failure mode is specific to how text moves. A guarantee stated bare in one place and
qualified in another is quoted bare — into a security questionnaire, a compliance summary, a
sales answer, a customer's own documentation — and the qualifier stays behind. Nobody involved
is being careless; a sentence is quoted because it is quotable, and a one-line guarantee is
more quotable than a four-line one. The qualified statement two sections away is not present at
the moment the quoting happens.

The same care applies to the larger claim the mechanisms sit under. The engine supplies a
compliance substrate — erasure, residency-aware placement, sensitive-column tagging, a
tamper-evident chain — and is not a certification of any regime; mechanism sits with the
engine, and classification, process and audit sit with the deploying organization.

## Decision

A stolen object-store credential reveals redacted columnar files for columns redacted at write
time whose sidecar indexes are encrypted under the key that covers the table. That qualifier
travels with the guarantee wherever it appears, and authored text asserting the at-rest
guarantee without the qualifier beside it raises `AtRestClaimUnqualified`. The guarantee omits
three cases: a column protected at the sync layer, a vector, full-text or bloom sidecar left
outside the table's encryption, and a tokenized column where the same actor holds the
tokenizing key. A column protected at the query layer alone sits in cleartext in the bytes on
disk.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **The qualifier travels with every appearance, enforced on authored text** *(chosen)* | Every quotable form of the claim is accurate on its own, so a sentence lifted into a questionnaire or a customer's documentation carries its own scope. | Every appearance of the guarantee is longer, and an author cannot compress it into a headline claim. |
| Stating the guarantee bare, qualifying it once in a canonical place | The strongest version of the claim is available in short form where it does the most good. | Lost on accuracy of what a holder can assert: a quoted sentence travels without its qualifier, and the reader who acts on it is the one whose deployment the qualifier excluded. |
| Qualifying only in the compliance section | Keeps operational prose readable while the audience that needs the scope has it. | Lost on the same reading: it is the bare-statement option with a different location for the qualifier, and the quoting behavior that defeats one defeats the other. |
| Dropping the guarantee entirely | No unqualified claim can ever escape, and no enforcement rule is needed. | Lost on withheld truth: write-time redaction delivers a real property worth stating, and removing it would push operators to assume less protection than they have. |
| Enforcing the qualifier at review rather than in a check | No rule to maintain; a reviewer applies judgment to context. | Lost on reliability of the mechanism: the failure is a sentence appearing somewhere without its scope, which is exactly what a reviewer reading one file at a time does not see. |

## Criteria

1. **Accuracy of what a holder can assert** — whether a reader could act on the guarantee in a
   deployment it does not cover. **This criterion decided.** It outranks brevity because the
   reader who is misled is not the reader of the full document; they are the reader of a
   quoted line, and the whole class of harm here comes from the sentence's portability rather
   than from its length.
2. **Portability of the sentence** — whether the claim survives being lifted out of context
   intact.
3. **Reliability of the mechanism** — whether the qualifier's presence is checkable rather
   than remembered.
4. **Readability** — how long each appearance becomes. The chosen option is the worst here.
5. **Completeness of what is claimed** — whether a real property goes unstated to avoid
   overstating it.

## Consequences

The guarantee is safe to quote, which is the point: a security questionnaire answered by
copying a sentence from the corpus produces an accurate answer. Because the check runs over
authored text rather than over a review, a new appearance of the claim in a file nobody
associated with security still carries its scope. The three exclusions are enumerated in the
same place as the guarantee, so an operator evaluating their own deployment can check theirs
against the list rather than inferring it.

The cost accepted: no short form exists. Every appearance is longer, marketing-shaped prose
cannot compress the claim into a headline, and an author who wants a crisp line has to either
write a weaker claim or link to the qualified one. That friction is permanent and it is the
mechanism working. A second, smaller cost: the check reads text, so it can be satisfied by
proximity — a qualifier placed beside the claim without being the right qualifier passes. What
the rule guarantees is that the scope is present, not that it is correct.

Dropping the enforcement would not immediately produce a wrong claim; it would produce a slow
one, as bare restatements accumulate in places nobody re-reads.

## Revisit triggers

- Sidecar encryption becomes unconditional for every index kind, removing one of the three
  exclusions and narrowing the qualifier.
- Query-layer masking is superseded by write-time redaction as the ordinary protection in
  deployments, which would change which case the common reader is in.
- The qualifier's presence is observed being satisfied by text that does not actually state the
  scope, meaning proximity is not a sufficient check.
