# 0069 — An outcome derived from the store's own rating carries a null verdict, in the view rather than in each reader

**Status:** accepted 2026-09-18
**Decides:** `memory.settle.invariant.self-rated`

## Context

Calibration asks whether the workspace's stated confidence tracks what actually happened. It
is only an answer if the observations are independent of the predictions. An outcome
derived from the store's own rating is not: the same machinery that emitted the claim is
grading it, so a calibration figure computed over such rows measures self-consistency and
reports it as accuracy. The figure looks better the more of these rows it contains, which is
the worst property a metric can have.

Excluding them is not in question. Where the exclusion lives is, and that turns on who can
write `outcomes`. It is an ordinary table on the store substrate. A pipeline can write it
directly, a resolver pass writes it, the observation tool writes it. A precondition on the
observation tool covers exactly the rows that came through the observation tool and nothing
else, so a pipeline appending rows the ordinary way bypasses it entirely — and the bypass is
silent, because a row written directly looks like any other row.

The label view is the other candidate. `outcome_labels` joins `predictions` to `outcomes` and
is what a scorer reads; every row that reaches a calibration figure passes through it,
regardless of which door wrote it. An exclusion there covers the whole table by construction.

That leaves what happens to the row itself. Dropping it loses the evidence that a self-rated
settlement was attempted, which is exactly the thing an audit wants to see — a component
grading its own predictions is worth noticing whether or not the grade counted.

## Decision

An outcome derived from the store's own rating carries a null verdict under a self-rated
signal, stays visible for audit, and is excluded from every calibration figure. The
exclusion lives in the view, since the outcomes table is writable by any pipeline.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Null verdict under a self-rated signal, row retained, exclusion in the label view** *(chosen)* | Covers every writer of `outcomes` by construction; the attempt stays visible to audit; one encoding of the rule | A self-rated row occupies storage and shows in audit queries while contributing to no figure, and a reader querying the base table directly sees it unfiltered |
| Refuse the write at the observation tool | The bad row never exists; smallest table; the error reaches the caller immediately | Lost on coverage: a pipeline writing the table directly bypasses it, and the bypass leaves no trace |
| Drop the row from the table | Nothing to filter downstream; no null-verdict rows to explain | Lost on auditability: the attempt itself is evidence that a component graded its own predictions |
| Exclude per reader — each consumer filters self-rated rows | No view change; each consumer chooses its own policy | Lost on repetition: each new consumer re-implements the filter, and the first one that forgets reports a contaminated figure as a clean one |

## Criteria

1. **Coverage over every writer** — whether the rule holds against a pipeline appending rows
   directly.
2. **Auditability** — whether an attempted self-settlement remains observable.
3. **Single encoding** — whether the rule is written once or once per consumer.
4. **Storage and query noise** — rows carried that contribute to nothing.

Criterion 1 decided it, and it eliminates the write-path precondition outright rather than
on balance: a rule that covers one of several doors is not a weaker version of the rule, it
is a rule that does not hold. Criterion 3 then rejects the per-reader filter among the
options that do cover every writer, because a filter re-implemented per consumer fails
quietly the first time a new consumer omits it. Criterion 4 loses, and the cost is stated
below.

## Consequences

Every calibration figure derived from `outcome_labels` is computed over independently
observed rows, whatever wrote the base table, and adding a new scorer adds no new
obligation.

Self-rated rows accumulate. They occupy storage, appear in audit queries and in any
count over the base table, and contribute to no figure — which is the cost accepted here.

A reader querying `outcomes` directly rather than through the label view sees them
unfiltered, with only the self-rated signal to distinguish them. The rule holds for the
scored path and not for an arbitrary ad-hoc query, and a deployment that scores off the base
table gets no protection from it.

Moving the exclusion elsewhere later is cheap in the view and expensive in trust: any rule
written outside the view inherits the coverage question the view exists to close, and
whatever population was scored in between cannot be re-verified from the rows alone.

## Revisit triggers

- Self-rated rows growing to a share of `outcomes` where their storage or their presence in
  audit queries becomes the complaint rather than the point.
- A write path onto `outcomes` narrow enough that a precondition covers every writer, which
  is the condition criterion 1 rests on being false today.
- A scorer reading the base table directly appearing in a deployment, which means the view
  is no longer the single point every figure passes through.
