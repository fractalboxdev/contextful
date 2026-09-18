# 0081 — A stop matching no in-flight run is an error rather than a quiet success

**Status:** accepted 2026-09-18
**Decides:** `run.cancel.refusal.terminal-run`

## Context

A stop is a durable catalog write onto a run row, and it carries a grain: a run-scoped stop
halts one table's work or one backfill chunk's, and a pipeline-scoped stop halts every run of
that pipeline in flight and every table or chunk the fire had left. Both grains are addressed
against rows that exist now.

A stop therefore races the thing it is aimed at. An operator reads a listing, decides, and
issues the stop; between the read and the write the run can succeed, fail, or already be
stopped. Pipelines also fire on a schedule, so the same pipeline id names a different set of
runs minutes later.

That is what makes the answer to an unmatched request consequential rather than cosmetic. Any
mechanism that holds the request for a future run is a mechanism where a stop issued at 09:00
against a fire that had already finished halts the 11:00 fire, which nobody asked about. And
any answer that reports success reports it to an operator who reads success on a stop command
as "it is stopping" — and then waits, watches a run that was never marked, and concludes the
stop mechanism is broken.

## Decision

Only a running or pending row accepts a mark, so no queue of stale requests exists and no
request survives to halt an unrelated fire later. A request matching nothing raises
`CancelTargetNotInFlight`, answering `409` over HTTP and exiting non-zero at the terminal
rather than an empty success a reader would take for "it is stopping". Marking an
already-marked row overwrites it with the newer request, so a repeated stop is harmless and a
reader sees the most recently given explanation.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse by name, non-zero exit** *(chosen)* | The operator learns immediately that nothing was stopped, and no request outlives the fire it was aimed at. | A real outcome the surfaces must report — a status code and a non-zero exit — rather than a quiet acknowledgement a script ignores. |
| Queue the request for a future run | The operator's intent is honored even when they were slightly late. | Loses on staleness. An unmatched request survives to halt an unrelated fire hours later, and nothing in the request records which fire was meant. |
| Answer empty success | Idempotent-looking, and scripts never have to handle an error. | Loses on legibility. An operator reads it as "it is stopping", waits on a run that was never marked, and mistrusts the mechanism afterwards. |
| Refuse, but with a zero exit and a warning | The information is present without breaking a script. | Loses on legibility too, one step less severely: a warning in a stream of output is not an outcome, and automation branching on exit status treats it as done. |

## Criteria

1. **Whether a request can outlive the fire it was aimed at.** Staleness of a durable stop.
2. **What a reader takes the answer to mean.** Whether the reported outcome matches what
   happened.
3. **Behavior under automation.** Whether a script can distinguish stopped from not-stopped.
4. **Tolerance for a late operator.** Whether losing the race costs the operator anything.

Criterion 1 decides it. A queued stop is the one answer that can cause harm to a run nobody was
looking at — the operator's intent was scoped to a fire that no longer exists, and honoring it
later applies an old decision to new work. Criterion 2 then rules out both quiet answers, since
the whole value of reporting is that the operator re-reads and re-issues. Criterion 4 is what is
given up, and it is cheap: re-issuing a stop is one command.

## Consequences

A stop is now a request against present state with a reported outcome, which makes it usable
from automation: `409` and a non-zero exit are branchable, and an operator wrapper can re-read
the listing and retry. The re-mark rule keeps that safe — a repeated stop against a row already
marked overwrites the reason rather than erroring, so retrying is harmless.

The accepted cost is that every caller-facing surface now carries a real failure arm on a
command that would otherwise always succeed. A script that ignores exit status sees no
difference; a script that checks it has to handle a case that is not a malfunction.

An operator who loses the race pays a re-read and a re-issue, and in the schedule case may find
the pipeline has already fired again — at which point the stop they now issue is against a
different run, deliberately.

## Revisit triggers

- Operators are observed losing the race often enough that the re-issue cost dominates, which
  would argue for a bounded, fire-scoped queue rather than an unscoped one.
- A stop gains an identity for the fire it was aimed at, which would make queuing safe by
  removing the staleness.
- The terminal surface gains a structured outcome channel where a distinct non-error outcome is
  as branchable as an exit status.
