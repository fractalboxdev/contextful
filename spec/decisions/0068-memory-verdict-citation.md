# 0068 — A verdict owes the source its prediction registered under, and a judgment verdict owes a settling citation

**Status:** accepted 2026-09-18
**Decides:** `memory.settle.refusal.unsourced-verdict`, `memory.settle.refusal.source-mismatch`, `memory.settle.refusal.settling-citation`

## Context

The three resolution sources owe different evidence. A `metric` verdict is reproducible: the
comparator and the ingested observable are both stored, so a reader can recompute it. An
`adjudicator` verdict comes from a resolver pass over raw source rows, and a `manual`
verdict comes from a human. Neither is reproducible from anything the store holds, so each
owes a citation — an `http` or `https` reference to what settled it.

The citation obligation is easiest to state at registration and hardest to hold at
observation, because the pressures point in opposite directions. At registration nobody
knows the answer, so declaring how the claim will settle is cheap. At observation the answer
is already known, and the citation is pure overhead to whoever is writing the row. That is
exactly when an obligation gets dropped.

Two ways of dropping it are available without ever writing a false citation. An observation
can carry a verdict and name no resolution source at all, in which case there is nothing to
check the citation requirement against. Or an observation can name a source that differs
from the registration's — settling under `metric` a claim registered under `manual`, which
moves the row into the one source class that owes no citation. Neither is necessarily
adversarial; both are what an observer with partial information does.

Where the citation is readable matters too. `outcomes` is the base table and
`outcome_labels` is the view that joins it to `predictions` and derives lead time. A scorer
reads the label view. A citation present on the base table alone is present where nobody
scoring looks.

## Decision

An observation carrying a verdict and no resolution source raises `OutcomeVerdictUnsourced`.
An observation whose resolution source contradicts the registration's raises
`OutcomeSourceMismatch`, so a claim registered under a source owing a citation is settled
under that source and no other. A verdict from `adjudicator` or `manual` carries an `http`
or `https` settling citation; its absence raises `OutcomeCitationMissing`, and the citation
rides the label view rather than the base table alone.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Bind the observation's source to the registration's, refuse an unsourced verdict, and project the citation onto the label view** *(chosen)* | The citation obligation is fixed at registration and cannot be shed at observation; a scorer reads the citation where it reads the verdict | An observer settling a claim whose registration it cannot read is refused rather than recorded, and a re-registration under a different source invalidates in-flight observations |
| Accept any source and record the discrepancy | Nothing is lost — every observation lands, and the mismatch is visible to an auditor | Lost on the requirement surviving: the citation obligation drops exactly when the answer is already known, which is the moment it exists to cover |
| Treat an unsourced verdict as inheriting the registration's source | Fewer refusals; the row lands with the right obligation attached anyway | Lost on the same criterion at one remove: an observer that did not read the registration also did not gather the citation, so the inherited obligation fails on the next check regardless |
| Carry the citation on the base table alone | Smaller view; no projection to keep in step with the join | Lost on reachability: a scorer reads the label view, so the citation is present where nobody scoring looks |

## Criteria

1. **Survival of the obligation after the answer is known** — whether a path exists to
   settle a citation-owing claim without a citation.
2. **Reachability of the evidence** — whether the reader who acts on a verdict can see what
   settled it.
3. **Availability to an observer** — how much of the registration an observing component
   must be able to read.
4. **Auditability of the discrepancy** — what a mismatch leaves behind.

Criterion 1 decided it, and it is what rejects the record-the-discrepancy option despite
that option scoring better on criteria 3 and 4. An obligation enforced everywhere except at
the moment it is inconvenient is not an obligation; a recorded discrepancy is a note in an
audit trail that nothing acts on, while a refusal is the only arm that keeps the
uncited row out of the scored population. Criterion 2 then decided the citation's placement
on the view.

## Consequences

The scored population is reproducible or cited, with no third category. A scorer reading
`outcome_labels` reaches the verdict, the source and the citation together.

An observer must be able to read the registration it settles. A component holding a grant on
`outcomes` and none on `predictions` is refused rather than recorded, which is a wiring
obligation this decision creates and the cost it accepts.

A re-registration under a different source invalidates in-flight observations: an observer
that read the old registration and writes under the old source is refused at the mismatch
arm. That is correct — the contract changed — and it is a failure mode operators will see
when they revise a prediction's settlement mechanism.

Relaxing the mismatch refusal later is straightforward; the population written under it
stays sound either way. Relaxing the citation requirement is not, because the rows written
after the relaxation are indistinguishable in the view from the ones written before it.

## Revisit triggers

- Refusals clustering on the mismatch arm around re-registrations, which would mean the
  rule is catching legitimate revisions rather than shed obligations.
- A settlement path where the observer genuinely cannot hold a read grant on registrations,
  which makes criterion 3 binding rather than a cost.
- A fourth resolution source whose reproducibility sits between metric and judgment, which
  reopens which sources owe a citation at all.
