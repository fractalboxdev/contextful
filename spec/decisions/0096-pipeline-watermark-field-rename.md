# 0096 — Renaming the clock field under a live cursor refuses at open

**Status:** accepted 2026-09-18
**Decides:** `pipeline.land.refusal.clock-field`

## Context

`incremental = "<field>"` names the stream's clock on the pipeline rather than inside a source's
config block, so the field means one thing whichever connector the pipeline points at. Declared,
the source reports a lease-free cursor kind and commits a position shaped as the field name
beside the value it reached. Undeclared, the source keeps full-refetch behavior.

The committed position therefore carries two things: a value, and the field that value was
measured on. That pairing is what makes the rename question answerable at all. Without the field
name in the cursor, a manifest edit from `updated_at` to `modified_time` is invisible — the next
pull compares a stored value against a new field and the comparison is arithmetic on unrelated
scales.

Both wrong answers are bad in different ways. Silently restarting the cursor re-lands a full
window: correct data, at the cost of a re-fetch the operator did not ask for and does not see,
which on a large source is hours of vendor quota reported as a successful run. Silently
continuing is worse: the two fields measure different things, so a bound taken from one scale
applied to the other excludes rows that should have landed, and the table is short by a set
nobody can enumerate afterwards.

The rename is detectable at open, before any request leaves the host — the declaration is in the
manifest and the field name is in the committed cursor.

## Decision

A committed cursor carries the field name it was measured on. Opening a pipeline whose
`incremental` declaration names a different field raises `PipelineClockFieldChanged` before any
request leaves the host. Declaring `incremental` on a pipeline that holds no cursor for that
field starts the cursor fresh, so the source's current window re-lands once rather than being
stepped over; a deliberate rename is expressed by resetting the cursor explicitly.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse at open, naming both fields** *(chosen)* | The mismatch is caught before a request, and the operator chooses the recovery rather than inheriting one. | A deliberate rename needs an explicit reset verb, and the refusal lands at open rather than at the manifest review that introduced it. |
| Restart the cursor silently on a rename | Every rename just works; no operator action. | Loses on visibility of cost: the operator reads success while the pipeline re-pays a full window against the vendor's quota, and on a large source that is the most expensive thing the pipeline can do. |
| Compare the stored value across the two fields | No interruption and no re-fetch. | Loses on correctness: the scales are unrelated, so the comparison drops rows invisibly and the table is short by a set nobody can reconstruct. |
| Keep a cursor per field, so a rename starts a second position | Both clocks stay available; a rename back resumes where it left off. | Loses on what it hides: the table then holds rows landed under two incomparable bounds with nothing recording which is authoritative, and the operator never learns the clock moved. |

## Criteria

1. **Whether the mismatch is detectable before a request goes out.** *This is the criterion that
   decided it.* It is detectable — both halves are in hand at open — and a failure that is
   cheaply detectable ahead of I/O should not be resolved by guessing. The two silent options
   both amount to choosing a recovery on the operator's behalf when asking costs nothing.
2. **Cost of the wrong direction, per option** — a silent restart re-lands a window; a silent
   continue compares two incomparable scales and loses rows. The second is unrecoverable, which
   is why neither is left to a default.
3. **Recoverability** — whether the operator can reach the intended outcome after the refusal.
   The reset verb reaches both the restart and a fresh start.
4. **Authoring friction** — a rename becomes two steps rather than one. This is the criterion the
   chosen option loses on.

## Consequences

A cursor's value is always interpretable, since the field it was measured on travels with it. An
accidental rename — a typo in the field, a copy from another pipeline — fails loudly with both
names printed and no vendor call made.

The cost accepted is friction on the deliberate case. An operator who genuinely means to move the
clock edits the manifest, watches the open refuse, and then runs the reset, which re-lands the
source's current window once. The refusal also arrives at open rather than during the review of
the edit that caused it, so the feedback is one step removed from the change — a manifest-time
check comparing the declaration against committed catalog state would close that gap and does not
exist.

## Revisit triggers

- A manifest-time validation path gains access to committed cursor state, which would move the
  refusal to the edit rather than the open.
- Clock renames become routine — a vendor deprecating field names on a schedule — making the
  two-step recovery a recurring tax rather than a rare one.
- A source appears whose two clock fields are provably on one scale, for which the refusal is
  correct by rule and wrong in fact.
