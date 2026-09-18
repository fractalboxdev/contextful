# 0097 — Input a parser cannot read fails one table's pull by name

**Status:** accepted 2026-09-18
**Decides:** `pipeline.land.refusal.unreadable-input`

## Context

A pipeline lands from sources whose items the engine did not author: a watched
directory of documents, a bucket prefix, a fetched response body. A fraction of those
bytes are malformed — a truncated archive, a document whose declared encoding is a
lie, a worksheet the vendor exported from a beta build. The rate is low and never
zero, and it does not fall with engineering effort, because the producer is outside
the system.

The landing sequence has three units it could fail at: the item, the table's pull, or
the fire. Each has a different consequence for what the run record can answer
afterwards. The run record is the only surface an operator has: a fire is driven by a
cadence, nobody watches it, and the question asked a week later is "does this store
hold the documents I put in that directory".

Ingested tables carry no negative statement. A collection that landed nothing and a
collection whose source is empty produce the same rows — none. Nothing downstream
distinguishes them, so whatever the engine chooses to say about a failed item is the
whole record of it.

An unreadable input is also permanently unreadable. The same bytes decode the same way
on the next attempt, so a retry schedule built for a flaky vendor spends attempts on
an outcome that cannot change.

## Decision

Input a parser cannot read is data rather than a fault. It raises
`PipelineUnreadableInput` naming the path and, where the input carries internal
structure, the position inside it — the page, the worksheet, the entry. The refusal is
permanent against the retry schedule. The failing unit is one table's pull: the
pipeline's other tables keep their tick, and one bad document among five hundred is
answerable by name from the run record rather than inferred from a row count.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Permanent named refusal, failing one table's pull** *(chosen)* | Every unreadable item is recoverable by path from the run record; healthy tables keep landing; the retry schedule is not spent on bytes that cannot change | A systematically broken directory emits one refusal per input, so the run record carries the volume |
| Skip the item and continue | Cheapest run, no failed count, no operator attention | Lost on answerability: a collection whose documents never landed reads exactly like one that has none, and no later read can tell them apart |
| Fail the whole fire | One diagnosis per bad batch, loud and unmissable | Lost on cost: one bad document among five hundred stops every healthy table and freezes the whole pipeline's freshness behind an item nobody needs |

## Criteria

1. **Answerability from the run record** — whether an operator holding only the run
   record can name which inputs did not land. *(decided it)*
2. **Blast radius** — how much healthy work one bad item stops.
3. **Retry economy** — whether attempts are spent on an outcome that can change.
4. **Diagnosis volume** — how many lines a systematic failure produces.

Answerability outranks the others because the alternative is silent and permanent:
blast radius and diagnosis volume are both visible the moment they hurt, and an
operator adjusts. A skipped item is invisible at land time and stays invisible at read
time, so the failure is discovered by the consumer of an answer rather than by the
person running the pipeline. A criterion that governs what nobody can later observe
outranks one that governs how loud an observable thing is.

## Consequences

Naming the path makes repair mechanical: re-export the named document, re-fire, read
the failed count drop. The per-table unit keeps a pipeline of independent tables
useful while one of them is stuck on a producer's bad export.

The cost accepted is volume. A directory whose inputs are systematically unreadable —
a wrong export format, a bulk re-encode — produces one refusal per input rather than
one diagnosis naming the pattern. Five hundred lines saying the same thing is a worse
read than one line saying it once, and the engine offers no grouping over them.

Permanence is also a commitment: an input the engine calls unreadable stays unattempted
until the bytes change. A parser improvement that would now read it does not
retroactively re-land the item; a re-fire over the source does.

Reversing the unit is expensive in one direction only. Widening to fail the fire is a
one-line change; narrowing to skip the item cannot recover the record of what was
skipped in the runs that already happened.

## Revisit triggers

- A run record routinely carries more than a few dozen unreadable-input refusals,
  which is the shape a per-source grouping would collapse.
- A parser class emerges whose failures are genuinely transient — a decode depending
  on a fetched resource — which breaks the permanence assumption.
- A read surface gains a negative statement about a collection, so a skipped item
  becomes observable downstream and answerability stops resting on the run record
  alone.
