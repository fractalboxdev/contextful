# 0151 — Task and driver are checked against each other at build rather than per unit

**Status:** accepted 2026-09-18
**Decides:** `derive.bind.refusal.unknown-task`, `derive.bind.refusal.driver-mismatch`

## Context

Two things are named in a derive configuration and they are not the same thing. `task` states
what kind of answer is wanted: `transcribe` turns recorded speech into timed passages,
`link_preview` turns an address a landed row points at into that document's head facts and the
picture candidates it advertises. `driver` states the mechanism that produces it: `exec` runs
an operator-declared argv chain, `fetch` opens sockets against an allowlist, `none` lands zero
rows and one loud run-audit line naming the key to fill in.

The two vocabularies do not compose freely. `exec` and `none` stand behind `transcribe`;
`fetch` stands behind `link_preview`. A `fetch` driver has no way to produce timed passages
from a recording, and an `exec` chain against a local binary is not how a third party's
document is read.

The tempting simplification is to drop one side and infer it from the other. That fails on the
inert driver: `none` serves `transcribe` and produces no rows at all, deliberately, so a
deployment can exercise the scan against stored data with nothing at stake. Task does not
determine driver, so the pair is not a function of one side.

The remaining question is when a bad pairing surfaces. The unit loop is a long serial walk; if
the pairing is only discovered when an engine is asked to do work, the discovery arrives once
per unit, in a table of failures, long after the operator who wrote the configuration stopped
watching.

## Decision

`task` is `transcribe` or `link_preview`, and an absent `task` reads as `transcribe`. A value
outside the pair raises `DeriveUnknownTask` and prints both supported spellings. `driver` is
`exec`, `fetch` or `none`; `exec` and `none` stand behind `transcribe` and `fetch` stands
behind `link_preview`, and any other pairing raises `DeriveDriverMismatch` naming the task and
the driver together.

Both checks run when the engine is built, before the first unit is scanned.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Two vocabularies, checked as a pair at build** *(chosen)* | The capability gap surfaces once, while the operator is watching, naming both halves; the inert driver remains expressible | Adding a modality touches both vocabularies and the pairing table rather than one list |
| Discovering the mismatch per unit | No build-time table to maintain; the engine reports what it cannot do | Lost on timing: it builds an engine that answers every unit with the same refusal, turning one configuration error into a table of identical failures discovered after the run |
| Inferring the driver from the task | One key instead of two, no pairing table at all | Lost on the inert posture: `none` serves `transcribe` and produces no rows, so the driver is not a function of the task and the inference has no correct answer for it |
| A single flat engine-kind vocabulary | One key, one list, no pairing check | Lost on the run record: the answer kind is what a consumer branches on and the mechanism is what an operator configures, so collapsing them puts a mechanism name in a column that describes findings |
| Making the driver a property of the `[derive.<name>]` block alone, unchecked against the task | Fewer refusals to write | Lost on the same criterion as per-unit discovery: nothing compares the two until work starts |

## Criteria

1. **When a capability gap surfaces** — at build with the operator watching, or per unit in a
   table of failures. **This criterion decided it.** A per-unit discovery is not merely later;
   it is a different failure shape, producing N identical refusals, consuming attempt budget
   and filling the derived table with markers for a mistake that is one key.
2. **Whether the inert posture stays expressible** — whether a driver that deliberately
   produces nothing can be bound.
3. **What a consumer reads** — whether the column describing the kind of answer stays free of
   mechanism names.
4. **Cost of adding a modality** — how many lists an edit touches.

## Consequences

A new modality is three edits, not one: the task vocabulary, the driver vocabulary if it
brings a new mechanism, and the pairing table that says which stands behind which. That is the
cost accepted, and it grows linearly with modalities rather than with deployments.

What gets easier: a misconfiguration is a single message naming both halves and both supported
spellings, delivered before any engine work and before any attempt budget is spent. An
operator can also bind `none` against a real pipeline to rehearse the scan with nothing at
stake.

What is now expensive to reverse: both vocabularies are closed sets that manifests are written
against, so a value refused today cannot start being accepted without a version of the
configuration that distinguishes the two eras.

## Revisit triggers

- A driver arrives that legitimately stands behind both tasks, so the pairing stops being a
  small fixed table.
- The task vocabulary grows past the point where a printed list of supported spellings is a
  useful error message.
