# 0059 — Revision is scoped to one validity line: two claims revise each other only when both are open-ended or both anchored to one instant

**Status:** accepted 2026-09-18
**Decides:** `memory.revise.invariant.validity-line`, `memory.recall.invariant.recall-clocks`

## Context

Every write onto a claim table goes through revision. A landing claim retires the prior
live claim on the same subject, predicate and scope: the retired row takes `superseded_by`
and a validity end set to the landing claim's validity start, stays in the table for audit
and for a point-in-time read, and falls out of live serving through the liveness predicate
every consumer already applies.

Two clocks run over these rows and they answer different questions. The session's vantage
bounds when a row entered the store. The validity columns bound what the row claims to hold
true of. A reader asking what the workspace believed last month bounds the first; a reader
asking what was true last month bounds the second.

A direct write can carry an `observed_at` anchor, which sets the claim's validity start and
end to that instant — a point interval — while the ingestion clock stamps the present. That
is how a conclusion about a past moment is recorded: written now, asserted about then.

Under revision keyed on subject, predicate and scope alone, such a claim collides with the
present belief. A pass reconstructing what held in a past quarter would retire the current
belief about the same subject, and the retirement would look identical to a correction.

Recall reads live rows: a present-time read returns open-ended intervals, and a read at a
vantage of one day additionally returns the point intervals anchored to that day.

## Decision

Two claims revise each other when both carry an open validity end, or when both are
anchored to the same validity start. A past-anchored belief and a present belief about one
subject and predicate coexist on separate lines, each holding one live claim per subject,
predicate and scope on its own line. The retirement stamps, the liveness predicate and the
fold are unchanged; the revision rule reads one more thing before it fires.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Revision scoped to a validity line** *(chosen)* | A historical vantage records what it concluded without overwriting the present belief, and every existing liveness predicate keeps working unchanged. | A reader asking for one subject's history sees two interleaved lines and resolves them by validity. |
| Retire on subject, predicate and scope alone | One key, one rule, nothing extra for the revision rule to read. | Loses on correctness: a conclusion drawn at a historical vantage silently overwrites the present belief, and the overwrite is indistinguishable from a correction. |
| Give anchored claims their own table | Physical separation; the present-belief table stays exactly as it was. | Loses on substrate count: it forks recall, the fold and the erasure cascade, each of which then has two implementations that must agree. |
| Let the anchor participate in the deduplication key | Two anchored assertions stay distinct without a second rule. | Loses on identity: an otherwise identical claim re-asserted at two vantages would be two rows by accident of the anchor rather than by any assertion the caller made. |
| Refuse an anchored write where a present claim is live | No interleaving; the conflict is surfaced at the write. | Loses on the use case: recording what held in the past is a legitimate write, and refusing it makes a historical reconstruction impossible whenever the present belief exists. |

## Criteria

1. **Correctness of retirement** — whether a claim can retire another that it does not
   actually contradict. *This criterion decided it.* A past-dated conclusion and a
   present-time belief are not competing answers to one question; treating a retirement
   there as a correction corrupts the present belief with an assertion about another time,
   and the corruption leaves no trace distinguishing it from a legitimate supersession.
2. **Substrate count** — how many recall, fold and erasure implementations the scheme
   requires. This eliminated a separate table for anchored claims, on the same reasoning
   that keeps every memory shape on one substrate.
3. **Liveness-predicate stability** — whether existing consumers keep working unchanged.
   The chosen option adds a condition to the revision rule and touches no consumer's
   predicate.
4. **Identity stability** — whether the deduplication key stays a statement about content.
   This kept the anchor out of the key and made the line the place the distinction lives.

## Consequences

A reader asking for one subject's history sees two interleaved lines — open-ended beliefs
and point-anchored assertions — and resolves them by reading validity. Nothing presents
them as one sequence, so a naive history view mixes "what we believed then" with "what we
now say held then".

Two unscoped writers colliding on one subject, predicate and scope land the later one not
live, and nothing surfaces the disagreement to a human. That is an open question in the
contract rather than something this decision settles.

The anchor stays outside the identity hash, so a caller asserting the same claim at two
vantages varies the claim's evidence qualification by hand to keep the two apart, or the
assertions collapse onto one row.

An anchored claim is a point interval, so it is returned by a read at exactly that day's
vantage and by no other. There is no notion of an interval-anchored belief holding across a
span, and adding one later would change what an existing anchored row means.

## Revisit triggers

- Anchored claims are written over spans rather than instants, which a point interval
  cannot express.
- Readers are observed building their own reconciliation of the two lines, which says the
  interleaving should be resolved by recall rather than by each consumer.
- The unscoped-collision question is settled with a surfacing mechanism, which may change
  what "never live" should mean on either line.
