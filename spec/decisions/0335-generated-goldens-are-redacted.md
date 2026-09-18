# 0335 — A golden mined from a live query or promoted from a labeled outcome passes write-time redaction and zone policy before commit

**Status:** accepted 2026-09-18
**Decides:** `build.baseline.refusal.raw-production-text`

## Context

A golden set has to come from somewhere. A set written by hand against a synthetic corpus
measures a system nobody runs; the cases that discriminate are the ones drawn from a real
deployment — the questions people actually asked, the rows that actually answered them, the
outcomes a reviewer actually adjudicated. Three generators bootstrap a candidate set from a
deployment's own store, and mining live queries and promoting labeled outcomes are the two
ways real traffic becomes ground truth.

Both routes move text. A golden set is version-controlled in the tree, so committing a
mined query commits the question a person typed, and committing an expected answer commits
the content of the rows that answered it. The product's whole posture is that primary data
stays inside the store under the operator's zone policy — and this path would carry exactly
that text out of the store, into a repository, where it is replicated to every clone and
retained under version control forever, outliving the rows it came from and any deletion
applied to them.

Promotion carries a different hazard. An outcome label can be adjudicated by a reviewer or
self-rated by the store. A self-rated row promoted into ground truth encodes the store's own
judgment as truth, and then the harness measures the store against its own bias while every
case carries full provenance and reads as real. The error is invisible in the numbers and
grows with every promotion cycle.

## Decision

A golden mined from a live query or promoted from a labeled outcome passes the store's
write-time redaction and zone policy before commit, defaulting to redacted or
referenced-by-id. The conversion step is fail-closed and raises `GoldenRedactionFailed`
where it cannot redact. Only an adjudicated outcome label promotes into a golden set; a
self-rated row is excluded.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Redaction and zone policy at the conversion step, fail-closed, with adjudicated-only promotion** *(chosen)* | No primary data enters the tree, and ground truth is a human's judgment rather than the store's. The store's own redaction pass is what runs, so there is one implementation of the rules. | Redacted goldens are weaker test inputs than the original text, and a by-id reference resolves only against a store that still holds the row. |
| Committing mined queries verbatim | The strongest possible test inputs: the real distribution of questions, with all the detail that makes a case discriminate. | Lost on data movement: the tree would hold exactly the customer text the product keeps inside the store, replicated to every clone and retained past any deletion applied to the rows. |
| Promoting every labeled outcome, self-rated included | Far more ground truth, far faster, with no adjudication bottleneck. | Lost on truth quality: a self-rated row carries the store's own bias into ground truth wearing full provenance, so the harness measures the store against itself and the error is invisible in the report. |
| A redaction pass written for goldens, separate from the store's | Tunable for this use — it could preserve more of the text where the case needs it. | Lost on having one implementation of the rules: a second redactor diverges from the store's, and the divergence is in the direction of preserving more text, which is the direction that leaks. |
| Referencing every case by row identifier, with no text at all | Zero text in the tree, and the strongest possible data-movement posture. | Lost on portability: a by-id case resolves only against the store that holds the row, so no case travels to another deployment or survives that row's deletion. Kept as one of the two defaults rather than as the only one. |

## Criteria

1. **Data movement** — whether committing a golden set can move primary data into a
   repository.
2. **Truth quality** — whether promoted ground truth is a judgment independent of the
   system being measured.
3. **Test input strength** — how much discriminating detail a case retains.
4. **Portability of a case** — whether a case resolves outside the store it came from.

**The first two are hard constraints and decide it together.** Neither is traded: a
repository holding customer text fails the product's own posture regardless of how good the
resulting tests are, and ground truth derived from the system under measurement fails to be
truth regardless of how much of it there is. Test input strength and portability are what
the decision spends, and the fail-closed conversion step is what keeps the first constraint
from being satisfied by intention rather than by mechanism.

## Consequences

A golden set is committable without moving primary data, and a conversion that cannot
redact stops rather than emitting something weaker than it should. Ground truth in the tree
is a human's adjudication, so an improvement measured against it is an improvement against
an independent judgment. The redaction rules have one implementation — the store's — so a
change to the policy reaches goldens without a second edit.

The cost accepted: redacted goldens are weaker test inputs than the original text. A
redacted question may lose the specific entity that made it a hard case, and a redacted
expected answer measures a coarser match. The by-id default has a sharper limit — such a
case resolves only against a store that still holds the row, so it does not travel between
deployments and it stops resolving the day that row is deleted, which is the day the golden
silently stops measuring anything unless the resolution failure is itself surfaced.
Adjudication is also a human bottleneck on the rate at which ground truth grows.

Reversing this is not a code change: any relaxation applies only forward, because text
already kept out of the tree cannot be recovered, and text once committed cannot be
uncommitted from every clone.

## Revisit triggers

- Redacted cases are found not to discriminate — a change that should move a metric does
  not, traced to detail the redaction removed.
- By-id cases fail to resolve often enough that the golden set's effective size drifts
  without notice, which would argue for surfacing unresolved references as a first-class
  tally.
- Adjudication throughput becomes the limit on golden-set growth, which would argue for a
  tier of machine-labeled cases held separately and gating nothing.
