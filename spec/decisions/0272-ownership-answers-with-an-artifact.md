# 0272 — An ownership question resolves to an artifact and its attached person, and a ranking over people is refused

**Status:** accepted 2026-09-18
**Decides:** `disclosure.bound-cohort.refusal.ranked-people`

## Context

"Who owns this" is among the most common questions asked of a store that has ingested an
organization's own material, and it has two entirely different answers available in the same
data.

One answer reads a record. A code-owners entry names a team. A directory's commit history
names who has touched it. An issue's routing carries a team. A page carries an owner field.
Each of those is an attachment someone deliberately recorded, and an answer built from one is
checkable at its source: a reader given the artifact identifier, the field the attachment was
read from, and the attached principal can go and look.

The other answer measures a person. It ranks contributors by activity, counts one person's
commits, or aggregates a metric over a single individual. Nothing in the data marks that
question as different — the same rows support it — but what comes back is a performance
signal about a named human, derived from material collected for a different purpose, with
whatever collection bias the ingestion carried. A deployment that ingested a code host to
answer questions about its codebase did not consent to producing an activity leaderboard, and
the person measured consented to still less.

The disclosure controls elsewhere in this contract do not reach these shapes. A group-size
floor and a contributor-share ceiling bound what an aggregate reveals about a contributor
inside a group; an aggregate computed over one person is a group of one, which is not a
small cell to be suppressed but a measurement whose entire subject is the individual. A
per-person grant does not fix it either: a grant that permits "aggregates over one person"
is, in practice, indistinguishable from ordinary read access over that person's rows, because
any row set can be aggregated to one number.

Refusing the question entirely is the other extreme and it gives up something real. The
artifact attachment is legitimate, checkable, and exactly what the asker usually wants.

## Decision

An ownership question resolves to an artifact and the person attached to it — a code-owners
entry, a directory's commit history, an issue's routing team, a page's owner field. The record
speaks and the answer names what it says. An ownership answer carries the artifact identifier,
the field the attachment was read from, and the attached principal, so a reader checks the
attachment at its source. A ranking of people, an aggregate computed over one person, and a
count of one person's activity raise `DisclosureRankedPeopleRequest`. An ownership question
with no recorded attachment returns nothing.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Artifact-and-attachment answers, person-measurement shapes refused** *(chosen)* | The answer is a reading of a record the organization already keeps, checkable by the person who receives it. The shapes that measure a human are unreachable rather than merely discouraged. | A question with no recorded attachment returns nothing rather than a best guess, and a deployment wanting contribution analytics builds it outside this surface. |
| Ranking contributors by activity | Answers the question in the common case where nobody filled in an owner field; needs no curation. | Lost on record-versus-measurement: a ranking is a per-person aggregate with no floor behind it, producing a performance signal from material collected to describe a codebase. |
| Returning an aggregate over one person behind an explicit grant | Keeps the capability available to deployments that genuinely hold it, with an auditable gate. | Lost on the same criterion, and on the grant's meaning: a grant permitting aggregates over one person is indistinguishable in practice from ordinary read access to that person's rows. |
| Inferring an owner from activity when no attachment exists | Closes the coverage gap that is the chosen option's whole cost. | Lost on checkability: the answer names a person on evidence nobody recorded, and the asker has no source to verify it against, so a wrong attribution is unfalsifiable at the point of use. |
| Returning no ownership answer at all | No person is named by the engine under any circumstance. | Lost on necessity: the artifact attachment is a legitimate, checkable record the organization maintains precisely to be read, and refusing it removes a correct answer to guard against a different question. |

## Criteria

1. **Whether the answer reads a record or measures a person.** **This criterion decided.** An
   artifact attachment is a fact someone recorded and stood behind; a ranking, a per-person
   aggregate and an activity count are measurements the deployment did not consent to
   producing and the subject did not consent to being. That distinction cuts cleanly through
   every candidate shape, which is why it outranks coverage.
2. **Checkability at the source** — whether the recipient can verify the answer against
   something outside the engine.
3. **Coverage** — what fraction of ownership questions get an answer. The chosen option is the
   weakest here, and the weakness is deliberate.
4. **Whether a gate means anything** — whether a proposed grant distinguishes the permitted
   case from ordinary access.
5. **Consent alignment** — whether the output stays within what the material was collected to
   support.

## Consequences

Every ownership answer carries its own provenance: identifier, field, principal. A disputed
answer is settled by looking at the artifact rather than by arguing about the engine, and a
wrong answer is a wrong record, which is fixable where records are kept. The refusal is stated
in terms of answer shape, so it holds regardless of which surface the question arrives on.

The cost accepted: coverage. An ownership question with no recorded attachment returns
nothing, and that is the common case in an organization that has not curated owner fields.
Users will experience the engine as unable to answer something it obviously has the data for,
and the honest response to that complaint is that the data supports a different question. A
deployment that wants contribution analytics builds it outside this surface, with its own
consent posture, which means the engine does not become the place that analysis lives — and
also does not stop it happening elsewhere.

Loosening this is cheap to implement and hard to unwind: once activity rankings have been
produced from ingested material, the material has been used for a purpose nobody declared, and
withdrawing the capability does not withdraw what was learned.

## Revisit triggers

- A deployment records an explicit, subject-visible consent posture for activity measurement,
  making the consent gap a settled question rather than an assumed one.
- Coverage of recorded attachments is measured and proves low enough that the surface answers
  almost nothing, indicating the record-reading premise does not match how organizations
  actually keep ownership.
- An inference method appears whose output is checkable at a source, which would move it out of
  the measurement category.
