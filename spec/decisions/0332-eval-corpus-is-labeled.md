# 0332 — An evaluation corpus carries zone and row-policy labels, and an unlabeled one is refused rather than scored

**Status:** accepted 2026-09-18
**Decides:** `build.evaluate.refusal.unlabeled-corpus`

## Context

The harness measures the read path by using it: the corpus loads through the real store,
the retriever under test calls the production ranked-retrieval surface, and enforcement
sits on that path like it does for any caller. That is the point of the arrangement — a
harness with its own storage and its own retriever cannot go red when the real one goes
wrong.

Enforcement is fail-closed. A row with no zone and no row policy is not visible to a
caller, because the default for unlabeled data is to withhold it rather than to expose it.
That default is correct and it is what makes the rest of the authority model safe.

The two combine badly at the harness. A corpus ingested without labels loads successfully,
indexes successfully, and then reads as empty to every retrieval call. Recall goes to zero.
Precision is undefined over an empty ranking. The judged tier reads from nothing and
reports whatever a reader says when handed no rows. The run completes, produces a report
with every field populated, and every number in it is a statement about labeling rather
than about retrieval quality.

A zero produced this way is indistinguishable from a genuine collapse in the ranked path.
Both look like a recall figure at the floor. The baseline gate compares it against a
committed number, reds, and names a metric — and the metric it names is the wrong place to
look.

There is a second property at stake. A labeled corpus means every evaluation run exercises
the access-control path, so a policy that is too restrictive shows up as a recall
regression in the harness rather than as a support question from a deployment.

## Decision

An evaluation corpus carries zone and row-policy labels, so a run exercises the
access-control path rather than stepping around it, and an over-restrictive policy surfaces
as a recall regression. An unlabeled corpus meets the fail-closed default and reads empty,
driving recall to zero; the harness raises `EvalCorpusUnlabeled` rather than reporting that
zero as a quality figure. A label, or an explicit local zone, is the precondition for a run
that answers anything.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Requiring labels, refusing an unlabeled corpus** *(chosen)* | A zero in the report is a retrieval fact. Every run exercises enforcement, so an over-restrictive policy reds the harness. | Every corpus needs labeling work before it produces a number, and a quick local run takes an explicit local zone rather than no configuration at all. |
| Running unlabeled and reporting the zero | No precondition; any corpus runs immediately; the number is technically what the system returned. | Lost on readability: the zero is caused by labeling and reads as a retrieval regression, so the gate reds on a metric that points nowhere. It separately means the run never exercises the access-control path at all. |
| Bypassing enforcement for evaluation runs | Removes the interaction entirely; the harness measures retrieval in isolation, which is what it claims to measure. | Lost on coverage: an over-restrictive policy would then never surface as a recall regression, so the one place the policy's effect on retrieval is measurable stops measuring it. It also makes the harness's read path differ from a caller's, which is the arrangement the harness exists to avoid. |
| Defaulting an unlabeled corpus to a permissive label | Convenient, and the run produces real retrieval numbers with no labeling work. | Lost on fail-closed posture: it introduces a path where unlabeled data becomes visible, in the one part of the tree whose whole job is to exercise the real path. |
| Detecting the empty read after the fact and annotating the report | No precondition, and the report explains its own zero. | Lost on readability again, one step later: the annotation is a field in a report that the gate compares numerically, so the red still names the metric and the explanation is somewhere a reader may not go. |

## Criteria

1. **Readability of a zero** — whether a floor figure in the report attributes to
   retrieval or to configuration.
2. **Coverage of the access-control path** — whether a run exercises enforcement, so that
   an over-restrictive policy is measurable.
3. **Fail-closed posture** — whether any arrangement creates a path making unlabeled data
   visible.
4. **Setup cost per corpus** — labeling work before a corpus produces a number.

**Readability decides it.** A harness exists to produce numbers someone acts on, and a
figure whose most likely cause is invisible in the figure is worse than no figure, because
it consumes an investigation and points it at retrieval. Coverage then eliminates the
bypass option, which satisfies readability and gives up the only measurement of the
policy's effect on recall.

## Consequences

A recall figure at the floor means retrieval returned nothing for rows the caller could
see. An over-restrictive policy shows up in the harness, where it is cheap to find, rather
than in a deployment. The judged tier never runs over an empty ranking, so judge cost is
not spent producing a number about nothing.

The cost accepted: every corpus needs labeling work before it produces any number, which is
real friction on adopting a new benchmark or a new application's ground truth — the
converter that brings it into the canonical case format now also has to decide the labels.
And the smallest case pays too: a quick local run takes an explicit local zone rather than
no configuration at all.

Reversing this is cheap in code and expensive in the archive: relaxing the refusal is one
change, but every historical run produced under the relaxed rule has a report whose zeros
cannot be classified after the fact.

## Revisit triggers

- Labeling cost becomes the reason a benchmark is not converted, measured by converters
  written and left unused.
- Enforcement gains a mode where an unlabeled row is distinguishable from a withheld one in
  the retrieval result, which would let the harness report a zero that explains itself.
- The access-control path acquires its own direct coverage sufficient to catch an
  over-restrictive policy, removing the coverage argument against measuring retrieval in
  isolation.
