# 0213 — Removal rules over a source that records its pulls verbatim are refused before the first read

**Status:** accepted 2026-09-18
**Decides:** `enforcement.redact.refusal.rule-over-a-recorded-source`

## Context

Write-time removal is the first of the three enforcement layers, and the only one that
makes a value absent rather than hidden. It runs inside the writer ahead of columnar
encoding, after relational normalization has shredded structured values into child tables,
and it offers two operations per rule: a drop, where no cell ever holds the value, and a
transform, where a policy-defined substitute occupies it. The property it buys is stated
against a specific threat — a stolen object-store credential yields columnar bytes with
the value already gone.

The run path keeps its own copy. A source's pull is recorded so that a replay returns the
recorded output and repeats no side effect, which is what makes a pipeline re-runnable
without hitting a rate-limited API again. That record holds the batch as the source
returned it, and it is written before the removal stage runs. One recording is a permanent
recording: replay reads it rather than re-deriving it.

So a project that declares removal rules on a table fed by such a source has two
artifacts. The Parquet has the value removed. The durable record has it in full, under the
same storage prefix, reachable by the same stolen credential. The rule is doing exactly
what it says and providing none of what it was declared for.

What makes this worth a refusal rather than a warning is the belief it produces. An
operator who declares a removal rule and sees it applied has been told the value is gone.
Nothing in the pipeline's behavior contradicts that, and the contradiction surfaces only
when someone reads the run path's records — which is to say, during an incident.

## Decision

Declaring removal rules over a source whose pulls are recorded verbatim raises
`EnforceRedactionOnRecordedSource`, and the refusal lands before that source is first
read. A table under removal rules gives up replay-without-refetch: re-running its pipeline
reaches the source again. The value this layer removes is absent from the run path's
durable record, and the record rather than the destination is the boundary this layer
holds.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the declaration before the source is first read** *(chosen)* | No configuration exists in which an operator believes a value is removed while a permanent copy of it sits beside the data | A table under removal rules gives up replay-without-refetch, so re-running its pipeline reaches the source again and pays its rate limits and latency |
| Reorder the removal stage ahead of the record | Both properties at once: removal applies and the pull is still recorded | Loses on replay: the record exists to return a pull without refetching, and a redacted record cannot reconstruct the source batch the pipeline would have processed |
| Accept the declaration and apply removal to the columnar files alone | Nothing is blocked; the destination bytes are clean | Loses on the same belief criterion: the removed value stays permanently in the record, so the operator holds a rule they believe binds and the record already defeated |
| Strip the record retroactively once removal runs | The permanent copy is eventually gone | Loses on permanence: replay returns the recorded output and re-runs no side effect, so a stripped record either breaks replay or is not actually stripped |

## Criteria

1. **Where the permanent copy lands, and what a holder believes about it.** *(decided
   it)* The others weigh capability against cost. This one is about whether a declared
   control means anything: a rule that is applied, reported as applied, and defeated by an
   artifact written one stage earlier is worse than no rule, because it stops the operator
   looking for the real exposure. Refusal is the only outcome that never produces that
   belief.
2. **Whether replay stays available.** Replay-without-refetch is a real property of the
   run path, and it is what the chosen option gives up.
3. **Whether the contradiction is visible at declaration time.** It is: the source's
   recording behavior is known before the first pull, which is why the refusal can land
   there rather than at the first read.
4. **Cost to the source of a refetch.** Rate limits and latency, borne per re-run, and
   bounded by how often a pipeline is re-run rather than by data volume.

## Consequences

The contradiction is impossible to construct: any pipeline that runs under removal rules
runs against a source whose pulls are not recorded verbatim, and the stolen-credential
guarantee holds across both artifacts. The refusal lands at declaration, so the failure
costs a configuration edit rather than a data-deletion exercise.

The cost accepted is replay. A table under removal rules re-reaches its source on every
re-run, paying the source's rate limits and latency, and a pipeline that was a cheap
idempotent re-run becomes an expensive one. For a source with a narrow quota, that can
mean a removal rule and a frequent re-run schedule are not both available.

The refusal is cheap to reverse in code and expensive to reverse in effect: relaxing it
would leave existing records in place, so every table that ran under the relaxation
carries a permanent copy nobody declared.

## Revisit triggers

- A recording shape appears that can reconstruct a pull for replay without holding the
  removed values — for instance a record of the request and a content digest rather than
  the batch.
- Refetch cost is observed to make removal rules unusable on the sources that most need
  them, so the refusal is pushing operators toward query-time masking on data that should
  not be at rest in cleartext.
- The run path's record moves under a separate credential boundary from the columnar
  files, changing which threat the second copy is exposed to.
