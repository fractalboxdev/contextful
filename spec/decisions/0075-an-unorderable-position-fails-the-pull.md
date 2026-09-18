# 0075 — A clock value with no ordering fails its pull terminally

**Status:** accepted 2026-09-18
**Decides:** `run.advance.refusal.unorderable-position`

## Context

An incremental read rests on one property: the declared clock field carries values that order.
A `monotonic` position commits the highest value observed, and a polled load admits rows whose
clock value is newer than or equal to the stored one. Both operations are comparisons, and a
comparison needs two values from one ordered domain.

Real sources violate that in two ways. A row arrives with the clock field null, empty, or
holding something that does not parse into the domain the stored position lives in — a
partially migrated table, an optional column, a vendor that omits the field on one record
kind. Or the stream itself changes domain mid-pass: the first page returns numeric sequence
values and a later page returns them as strings, or a timestamp column starts arriving
pre-formatted. Neither is rare and neither is announced.

What makes this sharper than an ordinary bad row is the frontier. The frontier counts every
fetched row, landed or not, because a row below the bound still proves the endpoint reaches
that far, and a committed position moves forward or stays. Any answer that lets the pass
complete therefore advances the bound past data whose position was never established. The
comparison is also byte-wise or numeric depending on domain, so a text position compared
against a numeric one does not error — a variable-width numeric string sorts by character,
and `"9"` sorts above `"10"`.

## Decision

A row carrying no orderable value in the declared clock field fails its pull terminally,
raising `CursorPositionUnorderable`. A stream that switches between a text position and a
numeric one mid-pass raises the same failure on the switch. The failure is terminal rather
than retryable: the same input reproduces the same verdict, so it consumes none of the attempt
budget. The position stays where it was and the pipeline resumes from there once the
declaration or the source is corrected.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Terminal failure of the pull** *(chosen)* | The stored position continues to mean what it says. A pass either establishes an ordering over everything it saw or does not complete. | A source whose clock column is sparse fails rather than partially landing, and the fix is a declaration change rather than a data change. |
| Skip the unorderable row | The pass completes; sparse columns cost nothing. | Loses on silence. The frontier advances over the skipped row, no count shows a gap, and the row is never fetched again. The damage is exactly the shape nothing downstream can detect. |
| Coerce text to a number, or a number to text | Mixed domains stop being a problem at all. | Loses on ordering. A variable-width numeric string sorts lexicographically, so coercion produces a bound that is wrong rather than absent — worse than failing, because it looks like it worked. |
| Land the row and hold the position | Nothing is lost, and the pass still completes. | Loses on progress. A source with one permanently unorderable row never advances again, and every poll re-lands the whole window, which reads as a working pipeline producing duplicates forever. |

## Criteria

1. **Whether the position stays meaningful afterwards.** Whether the committed bound
   continues to describe what has actually been read.
2. **Detectability of the damage.** Whether a wrong answer leaves any trace.
3. **Whether progress is preserved.** Whether the stream can still advance.
4. **Cost of the failure to the operator.** How much work a refusal imposes when the source is
   merely untidy.

Criterion 1 decides it. A cursor whose value no longer bounds what was read is not a degraded
cursor, it is a false one, and every later pass compounds it. Criterion 2 rules out the skip
specifically; criterion 3 rules out holding the position. Criterion 4 is the price and is
paid — an operator who finds this has a real problem with the declaration, and finding it at
the first offending row is finding it as early as it can be found.

## Consequences

An incremental declaration is now a claim the engine checks rather than an assertion it trusts.
A source with a sparse clock column is pushed toward either a different declared field, a
source-side filter, or a snapshot-shaped read — all of which are correct, and all of which cost
the operator a decision.

The accepted cost is that partial progress is unavailable on this path. A pull that fetched
ten thousand good rows and hit one unorderable value lands none of them, because the batch and
its position move together. That is deliberate: the alternative is a landed batch under a bound
that does not cover it.

The refusal does not reach a source that is consistently wrong in an orderable way. A column
holding formatted timestamps that sort correctly as text passes, and continues to pass when the
format changes to one that also sorts — the ordering is checked, not the meaning.

## Revisit triggers

- Batch-level partial landing becomes possible, so good rows in a failing pull could land under
  a bound that provably covers them.
- A source appears whose clock field is legitimately sparse over a bounded, declarable subset,
  making a declared null policy cheaper than a different field.
- Domain switching mid-stream is observed often enough from well-behaved vendors that a
  declared domain in the manifest becomes the better check.
