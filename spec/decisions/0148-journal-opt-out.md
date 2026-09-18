# 0148 — The derive tier records no verbatim copy of its pulls

**Status:** accepted 2026-09-18
**Decides:** `derive.select.refusal.journaled-pull`

## Context

Non-local sources journal their pulls. The journal records what the upstream handed back,
verbatim and before redaction runs, so that a crash between the pull and the commit does not
cost a second call to a rate-limited or paid API. For a feed reader or a mailbox connector
this is a good trade: the payload is already stored a moment later anyway, and re-pulling is
the expensive part.

The derive tier's payload class is different in kind. A transcript is the most PII-dense
object the system produces. Speech is unguarded — names, street addresses, phone numbers,
diagnoses, account numbers and salaries are spoken aloud in recordings whose published
metadata contains none of those things. The redaction rules on the write path exist precisely
for this material, and the journal is the one surface that holds the payload before those
rules have run.

So the journal is a second copy of the payload, written earlier, under a different retention
regime, that no redaction rule covers. Every control the write path applies to a derived row
— redaction, visibility, the disclosure surface — steps over it.

The work being protected is also unusually expensive to redo. An hour of audio is an hour of
inference; a crash between the engine call and the commit throws that away.

## Decision

`derive` sits on the pull-journaling opt-out list. A derive pipeline configured to journal its
pulls raises `DeriveJournaledPull`. A crash between an engine call and the commit re-pays for
the units of that run: the anti-join finds them outstanding on the next tick and the engine is
called again.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **No journal; re-pay for a crashed run's units** *(chosen)* | The payload exists in exactly one place, after redaction; no surface holds pre-redaction transcripts | A crash re-pays for up to a full run's worth of inference — the most expensive opt-out on the list |
| Journal the pulls like every other non-local source | Crash-resume is free and uniform across connectors | Lost on redaction coverage: the journal records each pull verbatim and pre-redaction, so the most PII-dense payload the system holds sits outside every rule written to govern it |
| Journal a redacted copy | Coverage restored, resume mostly preserved | Lost on purpose: redaction runs against a reconciled schema on the write path, so a redacted journal is a second implementation of the redaction rules that can drift from the first |
| Journal only for the link task, not for transcription | Coverage where it matters, resume where it is cheap | Lost on simplicity of the guarantee: opt-out becomes a per-task property an operator has to reason about, and a link document prefix can carry personal data too |
| Shrink the run's row cap so a crash costs less | Bounds the loss with no new surface | Lost on effect: it reduces the size of the loss rather than removing the copy, and the copy is what the criterion measures |

## Criteria

1. **Redaction coverage of the payload class** — whether any copy of the payload exists that
   the write path's rules do not reach. **This criterion decided it.** A pre-redaction copy is
   not a performance property that can be traded against another performance property; it is a
   hole in a control the rest of the system is built to guarantee, and no amount of saved
   inference buys it back.
2. **Cost of re-doing work after a crash** — how much inference a crash between the engine call
   and the commit throws away.
3. **Whether the guarantee is one sentence** — whether an operator can state what the tier
   stores without qualifying it per task.
4. **Number of implementations of the redaction rules** — whether a second copy path would
   need its own.

## Consequences

Derive is the one opt-out member whose pull is expensive. A crash mid-run costs up to a run's
budget in inference — with the default row cap, up to twenty-five units of engine work, which
for transcription can be hours of compute. That is the cost this decision accepts, and the
row cap is the only control over its size.

What gets easier: the retention story for derived text is one sentence — derived passages exist
in the derived table and nowhere else. Answering where a transcript lives needs no exception.

What is now expensive to reverse: nothing in the tree records pre-redaction payloads, so adding
a journal later means adding a new class of stored data and the retention, visibility and
disclosure treatment it needs, not flipping a flag.

## Revisit triggers

- Redaction moves ahead of the journal on the write path, so a journaled payload is covered by
  the same rules as a committed one.
- Metered engine cost per crashed run becomes a reported figure large enough to outweigh the
  handling cost of a second payload copy.
