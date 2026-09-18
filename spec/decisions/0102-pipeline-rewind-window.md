# 0102 — An unevaluable rewind window refuses while a window matching nothing is a no-op

**Status:** accepted 2026-09-18
**Decides:** `pipeline.backfill.refusal.rewind-window`

## Context

A rewind returns every chunk whose window overlaps a half-open range to pending and
leaves the rest done. The operator supplies that range by hand or from a script, on
whatever scale the plan's cursor uses — instants for a clock, integers for an id range,
partition keys beside a window for a partitioned clock.

Two very different things can be wrong with what arrives. The range may be
uninterpretable against the plan: the bounds are transposed, the range is empty, or a
bound sits on a scale the cursor cannot be compared with — a timestamp handed to an
id-range plan, a string where the plan counts. Or the range may be perfectly
interpretable and simply overlap no chunk: a valid window over a region the plan does not
cover, or covers with chunks that are already pending.

Rewinds are also driven from cron. A scripted rewind over a rolling range — the last
three days, the current partition — legitimately matches nothing on most runs, because
the range has already been rewound or the chunks in it were never planned. If that
exited non-zero, every such driver would treat its normal state as a failure and the
operator would learn to ignore the signal.

A rewind is cheap to repeat and destructive of nothing: the first pass's files stay on
disk, stay attributable through the run column, and the fold picks the winner by the
table's recency rule. The risk in a rewind is therefore not in doing too much of it —
it is in an operator believing a rewind happened when it did not.

## Decision

An inverted window, an empty window, and a bound that cannot be compared with the plan's
cursor scale each raise `PipelineRewindWindowInvalid`. A window that is well-formed
against the plan and overlaps no chunk is a silent no-op: it returns nothing to pending
and reports success.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the unevaluable, no-op the empty overlap** *(chosen)* | An operator's malformed input is refused while a correct request with no work in it stays quiet, so a scripted rolling rewind runs clean | An operator who names a correct-looking but wrong region gets silence, and cannot tell it from a region already rewound |
| Refuse an empty overlap too | Every rewind that does nothing says so, so a mistyped region is caught | Lost on false alarms: a scripted rewind over a rolling range legitimately matches nothing on most runs, and a driver that fails on its normal state stops carrying signal |
| Accept an inverted window as an empty one | One code path; no refusal to write | Lost on error detection: it hides a transposed pair of bounds, which is the most common way the input is wrong, behind the same silence as a legitimate no-op |
| Refuse nothing; clamp or reinterpret every input | No rewind ever fails | Lost on auditability: the rewind log then records a window the operator did not ask for, and the audit trail describes a decision the engine made |

## Criteria

1. **Whether the operator can distinguish an error in what they wrote from a correct
   request with no work in it.** *(decided it)*
2. **Behavior of a scripted rewind on a rolling range** — whether the normal case is
   clean.
3. **Cost of being wrong** — what a missed or spurious rewind actually destroys.
4. **Auditability of the rewind log.**

The distinction between a malformed request and an empty one decided it because the two
have opposite correct responses and are the only two outcomes an operator sees. Once
that separation is the goal, the line falls where the engine can actually draw it: the
plan can tell whether a window is interpretable, and it cannot tell whether an
interpretable window was the one the operator meant. Drawing the line anywhere else
either fails the scripted case or hides a transposition.

The low cost of a spurious rewind is what made silence acceptable on the empty side: the
originals survive, so a repeated rewind costs a re-pull rather than data.

## Consequences

A cron-driven rewind over a rolling range exits clean on the runs where it finds nothing,
so its non-zero exit remains meaningful.

A transposed pair of bounds — the most common hand-entry mistake — is caught before
anything is returned to pending, and the message names the window.

The cost accepted is at the edge of the refusal. An operator who transposes bounds gets a
refusal, and one who names the wrong scale entirely gets the same refusal with a
different message; neither message distinguishes a typo from a plan the operator
misremembered. Worse, an operator who names a well-formed window over the wrong region
gets nothing at all — the same silence a correctly-empty rewind produces — and must read
the rewind log to learn that no chunk moved.

Reversal is cheap in both directions: the arms are independent and the rewind log records
what each pass did, so moving the empty-overlap case onto the refusal side breaks no data
and only breaks drivers.

## Revisit triggers

- A rewind verb gains a machine-readable count of chunks returned to pending, which lets
  a driver distinguish empty from effective without an exit status and removes the
  argument for silence.
- Operators are observed reading the rewind log to confirm a rewind took effect, which is
  the workaround the silent no-op forces.
- A plan kind appears whose cursor scale cannot be checked for comparability ahead of the
  pass, which removes the third refusal arm's precondition.
