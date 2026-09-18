# 0066 — A prediction names exactly one resolution form

**Status:** accepted 2026-09-18
**Decides:** `memory.settle.refusal.resolution-form`

## Context

A prediction is a claim about the future the workspace emitted, held as one row carrying
the subject, the target, the resolution form, the resolution key, the declared resolution
source, `predicted_at` and the confidence attached. The resolution form says when the claim
becomes settleable: a relative horizon, an absolute deadline, or an open-ended watch.

The form is read by two mechanisms. Scheduling reads it — an observation is scheduled
asynchronously at the resolve instant, and an open-ended watch is scheduled not at all and
settles when something observes it. The label view reads it — the join keeps an observation
from the prediction instant through the deadline plus an inclusive grace, so a deadline is
what decides whether a settlement is scored or dropped. A prediction with no usable form is
therefore not a prediction with a missing field; it is a row that silently changes both what
gets observed and what gets scored.

Two shapes of malformed registration are available to a caller. Naming no form at all, and
naming two — a horizon and a deadline together, say, arriving from a caller that filled
both fields of a form.

Defaulting is the tempting move for the first. It costs nothing at the call site and makes
every registration succeed. What it produces is a population of predictions that look
settleable and carry a deadline nobody chose. Those deadlines drive scheduled observations
— real work against real sources — and they set the scoring window in the label view, so a
calibration figure computed over them is a figure about a default the engine invented.

The second shape has no defaulting answer at all, only a precedence rule. Any such rule
picks one of two stated intentions and discards the other without telling the caller which.

## Decision

A registration names exactly one of a relative horizon, an absolute deadline, or an
open-ended watch. Naming none and naming two both raise `OutcomeResolutionFormInvalid`. No
caller receives a fabricated default deadline, and a deadline given as a calendar day
canonicalizes to midnight UTC on that day rather than to a form the engine chose.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Exactly one form, both malformed shapes refused at registration** *(chosen)* | No scheduled observation and no scoring window rests on a value the caller did not state; the error names the field | Every caller states a horizon explicitly, including one that genuinely does not care when the claim settles |
| Default an unnamed form to an open-ended watch | Every registration succeeds; the default is the least presumptuous of the three forms | Lost on silence: a caller that meant a deadline never learns it omitted one, and the claim quietly leaves the scheduled population |
| Default an unnamed form to a fixed relative horizon | Every registration succeeds and every claim stays settleable | Lost on silence and on fabricated work: the default drives real observations against real sources on a schedule nobody chose |
| Accept a horizon and a deadline together and take the earlier | No registration is rejected for over-specification; the rule is stated and deterministic | Lost on ambiguity of what was predicted: the row records a settlement window the caller did not ask for, and the discarded field leaves no trace |

## Criteria

1. **Whether an unstated intention can become stored state** — the gap between what the
   caller said and what the row asserts.
2. **Fabricated downstream work** — observations scheduled and sources fetched on the
   strength of a value the engine supplied.
3. **Integrity of the scored population** — whether a calibration figure is computed over
   windows callers chose.
4. **Call-site burden** — fields every caller fills.

Criterion 1 decided it. A refusal at registration is the only point where the caller is
still present to correct the omission; after the row is stored, the difference between a
chosen deadline and a defaulted one is invisible to every later reader, including the one
computing calibration. Criterion 4 loses, and the loss is small and permanent: a caller
that does not care states an open-ended watch, which is one token.

## Consequences

Every stored prediction asserts a settlement window its author chose, so a scheduled
observation and a scored row both trace to a stated intention.

Every call site fills the field. A caller with no view on timing writes an open-ended watch
rather than leaving the field out, which is a small recurring cost and the one this record
accepts.

An over-specified registration is rejected rather than reconciled. A caller assembling a
registration from two sources — a form default and an explicit deadline, say — gets an
error instead of a row, and must decide at its own boundary which one it meant.

Adding a default later is cheap in the parser and expensive in the data: the moment one
exists, the stored population mixes chosen and defaulted windows with no column
distinguishing them, and every calibration figure computed afterwards spans both.

## Revisit triggers

- Registrations failing on the missing-form arm at a rate that suggests callers cannot
  determine a form at their own boundary, which would point at the wrong party holding the
  decision.
- A recorded distinction between a caller-stated and an engine-supplied form, which would
  let a default exist without contaminating the scored population.
- A fourth resolution form entering the set, which reopens whether "exactly one" is the
  right cardinality or whether forms compose.
