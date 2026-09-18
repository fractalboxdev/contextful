# 0100 — A table's failure halts the fire by default

**Status:** accepted 2026-09-18
**Decides:** `pipeline.land.refusal.table-failure`

## Context

A pipeline declares one source and the tables it lands from that source. A fire walks
those tables, pulling each in turn. When one table's pull fails, the engine either stops
the fire or moves to the next table.

The shape of a real pipeline decides which of those is noise. The overwhelming majority
of declared pipelines carry one source: a vendor API, one directory, one bucket prefix.
Their tables are endpoints of the same upstream under the same credential behind the
same rate limiter. A failure that stopped the first table — an expired token, a 429, a
vendor outage, a schema change — stops every table after it for the same reason.
Continuing under those conditions pulls the same failure N times and writes N copies of
one error into the run record.

The minority shape is a pipeline whose tables are genuinely independent, where one
table's upstream is down and the rest are healthy. There, halting costs every healthy
table its tick, and a table's freshness falls behind for a reason unrelated to its own
source.

A halt is also not silent. Both settings produce identical accounting — a run row bearing
the error kind, a step-failure event beside a run-failure event, and a position left
where the next attempt resumes — so the choice between them governs how much work a fire
does after the first failure, not how visible the failure is.

## Decision

A table whose pull fails raises `PipelineTableFailed` carrying the table, the error kind
and the run id. What the fire does next follows the pipeline's declared table-error
setting, and that setting defaults to halting the fire. A pipeline whose tables are
independent declares the continuing behavior explicitly.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Halt by default, continue by declaration** *(chosen)* | The common one-upstream pipeline reports one error rather than N; the independent case is one declared line away | Every halt costs the healthy tables behind the failing one a tick, until the operator declares otherwise |
| Continue as the default | No healthy table ever loses a tick to an unrelated failure | Lost on noise: most pipelines are one upstream, so the common case becomes N copies of one error and the run record stops being readable at a glance |
| No default — every pipeline declares it | Forces the author to think about it once | Lost on cost of authorship: the setting is correct for the common shape, and a mandatory declaration taxes every pipeline to serve the minority |
| A per-table override, and a cap on failures before a halt | Fine-grained control over mixed pipelines | Lost on declaration burden: each widens the declaration surface to serve a mixed pipeline shape that the one-upstream distribution weighs below the cost of writing it |

## Criteria

1. **Noise in the run record under the common pipeline shape** — how many distinct
   errors one upstream failure produces. *(decided it)*
2. **Tick cost to healthy tables** — what a halt denies work that would have succeeded.
3. **Declaration burden** — what an author writes to get correct behavior.
4. **Accounting parity** — whether the two settings differ in what they record.

Noise decided it because a default is a bet on the distribution, and the distribution is
one-source pipelines by a wide margin. Tick cost is real but bounded and self-correcting:
the operator sees a halt, reads one error, and either fixes the upstream or declares
continuation. N copies of one error are not self-correcting — they degrade every future
read of the run record, including reads about unrelated failures. Accounting parity
removes visibility from the comparison entirely, which is what let the choice be settled
on noise rather than on safety.

## Consequences

An operator reading a failed fire on the common pipeline reads one error naming one
table and one cause. Triage is the upstream, not the pipeline.

Declaring continuation is one line, and its effect is local to the pipeline that
declares it, so the minority shape is served without moving the default.

The cost accepted lands on the continuing setting rather than the default one, and it is
the more dangerous of the two. Continuing turns a total outage into a quiet partial: the
fire exits having landed most tables, rows-landed is non-zero, and a cadence driver
watching only that number sees a working pipeline while one table has been empty for
days. The exit status and the staleness budget both carry that signal, and a driver
reading neither will not see it. An operator who declares continuation is taking on the
obligation to watch one of those two.

Halting also means a failing table ahead of a healthy one in the walk order silently
determines what gets a tick. The walk order is not a declared priority, so a pipeline
that halts routinely starves whatever sits behind the flaky table.

## Revisit triggers

- Declared continuation becomes the majority setting across real pipelines, which means
  the default is betting on the wrong shape.
- A mixed pipeline appears whose tables split between one shared upstream and several
  independent ones, which is the case a per-table override exists for.
- A pipeline is observed halting on the same table repeatedly while tables behind it in
  the walk order go stale, which is the starvation the walk order does not guard.
