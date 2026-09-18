# 0078 — A permanent failure closes its step immediately and burns none of the attempt budget

**Status:** accepted 2026-09-18
**Decides:** `run.retry.refusal.permanent-failure`

## Context

One tagged failure type crosses every port and both halves of the engine: `Transient`,
`Permanent`, `SchemaIncompatible`, `AuthExpired`, `RateLimited { retry_after_ms }`, `Config`,
`Storage`, `UnknownConnector` and `SecretNotFound`. The retry decision is a pure function of
the attempt number and the classified failure — it reads no timer, no clock and no random
state — and the runner turns the returned duration into the actual sleep.

Those tags are not equally informative about the future. `RateLimited` says the same call
later may succeed. `Transient` says a network or upstream condition may clear. `Permanent`
says the answer is settled: the request is malformed, the resource does not exist, the
operation is not allowed. Retrying a `Permanent` failure runs the identical call against the
identical state and receives the identical answer, five times, separated by a schedule that
by default exponentiates from 100 ms with each delay clamped at 30 s.

A second class of failure carries the same property for a different reason. Some refusals are
checks the engine performs itself over static input — a spelling refused, a pin compared, a
field name matched. Those are pure functions over data already in hand, so repeating one
produces the same verdict with no call going out at all.

The step's answer also has to travel. A run compensates on a failed step, dead-letters it, or
fails; each of those needs the step label and the underlying typed failure, not a flattened
message.

## Decision

A `Permanent` failure closes its step immediately and surfaces as `StepFailed` carrying the
step label and the underlying typed failure, for the run to compensate on, dead-letter or fail
on. A step exhausting its schedule surfaces the same way, carrying its terminal tag. A refusal
whose check is pure over static input is terminal and consumes none of the attempt budget.
Retry is reserved for `Transient` and `RateLimited`; everything else stands as the step's
answer whatever budget remains.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Retry only the tags that can change** *(chosen)* | A settled answer is delivered at once, and the attempt budget means what it says: attempts that could have succeeded. | Every failure site owes an honest tag, and a source mislabelling a transient condition as permanent gives up a recovery no later check restores. |
| Retry every tag uniformly | One rule, no classification to get wrong, and a mislabelled transient failure still recovers. | Loses on latency. A fixed answer is delayed by the whole schedule — minutes of sleeping to reproduce a verdict already in hand — and every retry re-enters an effect that cannot help. |
| Branch on the error message rather than the tag | No taxonomy to maintain; new upstream conditions classify themselves. | Loses on drift. An upstream's wording changes without notice or version, so a substring match silently reclassifies an entire failure class the first time a vendor edits a string. |
| Retry `Permanent` once as a hedge | Covers the mislabelled-transient case at a bounded cost. | Loses on repeatability. It pays a delay on every genuine permanent failure to recover a case that is a bug in the classifier, and it makes the attempt counter mean two different things. |

## Criteria

1. **Whether a further attempt can change the answer.** Repeatability of the verdict over the
   same input.
2. **Latency to a settled answer.** How long a caller waits for something already determined.
3. **Stability of the classification itself.** Whether the rule keeps meaning the same thing
   as upstreams change.
4. **Recovery from a misclassification.** What happens when a failure is tagged wrongly.

Criterion 1 decides it. A verdict pure over static input reproduces exactly, so an attempt
spent on it is an attempt spent on nothing; the budget exists to absorb conditions that clear,
and spending it where nothing can clear makes the number meaningless. Criterion 3 rules out the
message-substring alternative independently, because a rule that changes meaning without a
change in the code is worse than a rule that is sometimes wrong in a fixed way. Criterion 4 is
the accepted exposure.

## Consequences

An attempt count is now legible: it counts attempts that could have succeeded, which makes it
comparable across steps and usable in a health reading. A run reaches a settled answer in one
call rather than one schedule, so a malformed request costs a request rather than a minute.

The cost accepted lands on the classifier. Every failure site owes an honest tag, and the tag
is a claim about the future that the site is asserting on its own authority. A connector that
tags a transient upstream condition `Permanent` gives up the recovery, and no later check
restores it — the run fails on the first call and the schedule it would have had is never
consulted. Auditing that is per-site work with no global test.

The same holds in the other direction with a smaller blast radius: a genuinely permanent
condition tagged `Transient` costs a schedule and then closes anyway, carrying its terminal
tag.

## Revisit triggers

- A connector class appears whose upstream cannot distinguish a permanent refusal from a
  transient one at the point of classification.
- Mislabelled tags are observed as a recurring cause of failed runs, making a bounded hedge
  cheaper than the per-site audit.
- The taxonomy gains a tag whose repeatability is genuinely unknown at the failure site, which
  would need a third answer between retry and terminal.
