# 0072 — A pipeline pairing write-path redaction with a journaling source is refused before its first pull

**Status:** accepted 2026-09-18
**Decides:** `run.journal.refusal.redacting-source`

## Context

Pull journaling defaults on. A journaled pull records the batch as the source handed it
over, serialized inside the pull and ahead of the land path — a second envelope over the
same rows rather than a copy of what the destination table ends up holding. That recording
is what lets a crashed run replay without refetching: a journal hit hands back the recorded
batch and the land path runs over it as it ran the first time, so a resumed run lands bytes
identical to an uninterrupted one.

Write-path redaction strips values that a policy says are never to be stored. It runs
post-normalize, after the land path has shredded a nested payload into parent and child
tables, because that is where the columns it must reach exist — a value nested three levels
down inside a vendor payload only becomes a column at that point.

The two orderings are incompatible as written. The recorded artifact is serialized inside
the pull, which is ahead of normalization; redaction runs after it. A pipeline declaring
both writes the raw vendor payload to the journal and then redacts the shredded columns, so
the value the policy forbids is on disk in the recorded envelope, durable, and reachable by
anything that reads the journal. The rule that no value a policy would strip reaches the
journal holds where ordering reaches, and this is exactly the pairing where ordering does
not reach.

Once such a batch is written there is no undoing it that satisfies the policy: the recorded
envelope is journal state, it is what a replay resolves, and stripping it retroactively
turns a replay into a different run.

## Decision

A pipeline declaring write-path redaction over a source that journals its pulls raises
`JournalRedactionConflict` ahead of its first pull — from the connector name during manifest
validation, on the apply and reconcile path, and from the built source at run open, which
also catches a source wired under a name no table declares. Nothing is written down and
regretted.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the pairing at three points, all ahead of the first pull** *(chosen)* | No un-redacted artifact ever exists on disk; the rest of the corpus keeps journaling; the refusal names the pipeline before it runs | Exactly those pipelines give up replay-without-refetch: a crash re-pulls from the source rather than resolving the recorded batch |
| Move the recorded mirror behind the land path, so one redaction rule covers the artifact and the stored file alike | The tidier shape — one rule, one ordering, both surfaces covered, and journaling stays available to every pipeline | Lost on coverage: redaction runs post-normalize, where it reaches columns relational shredding moves into child tables, so a mirror recorded ahead of it is un-redacted and a mirror recorded behind it is no longer the batch the source handed over |
| Mask the mirror separately, with its own rule over the raw envelope | Journaling and redaction coexist; replay-without-refetch is preserved | Lost on drift: two rules encoding one idea disagree, and the disagreement is a policy-forbidden value surviving in the envelope one of them did not cover |
| Refuse at run open alone | One check rather than three; the failure still precedes the first pull | Lost on reach: a manifest carrying the pairing validates clean and reconciles clean, so the conflict surfaces only when somebody runs it |

## Criteria

1. **Whether an un-redacted artifact can exist on disk at any instant** — the property the
   redaction policy asserts.
2. **How much of the corpus keeps working** — pipelines that lose journaling under the rule.
3. **Single encoding of the policy** — whether one idea is stated once.
4. **How early the conflict surfaces** — the distance between declaring the pairing and
   learning it is refused.

Criterion 1 decided it, and it is absolute rather than weighted: a policy that forbids
storing a value is not satisfied by storing it briefly. That eliminates the relocated mirror
despite it being the better shape on criteria 2 and 3 — the shape is right and the ordering
it needs does not exist, because the columns redaction must reach do not exist until after
the point where the batch is still the batch the source handed over. Criterion 3 then
rejects the separate masking rule among the remaining options. Criterion 4 chose three check
points over one.

## Consequences

The redaction policy holds without qualification: no pipeline configuration produces a
recorded envelope carrying a value the policy strips.

Exactly those pipelines give up replay-without-refetch. A crash mid-run re-pulls from the
source rather than resolving the recorded batch, which costs vendor calls, quota and time
proportional to how far the run had got. That is the cost accepted, and it lands on the
pipelines handling the most sensitive data.

An operator declaring the pairing learns at manifest validation rather than at the first
run, and a source wired under a name no table declares is caught by the same check at run
open. Three check points are three places to keep in step, and a redaction declaration that
becomes reachable through a fourth path is not covered until that path checks too.

Reversing the refusal means one of the rejected options becoming available, which is a
change to where redaction runs rather than a change to this rule.

## Revisit triggers

- A redaction pass that can run over a nested payload ahead of shredding, which is what the
  relocated-mirror option needs and does not have.
- A recorded envelope format that is itself normalized, so the artifact and the stored file
  share one column set and one rule can cover both.
- Refusals landing on pipelines whose redaction declaration reaches no column the source
  actually produces, which would mean the check is coarser than the conflict.
