# 0315 — A greeting that cannot be derived inside its budget silently does not exist

**Status:** accepted 2026-09-18
**Decides:** `console.brief.refusal.absence-is-earned`

## Context

A returning reader is greeted with a card crossing what arrived while they were away
against what the store has concluded. It is a derivation over two governed reads plus one
component: no storage of its own, no engine change, and no model on its path. It renders
inside the first second of a session and costs nothing while nobody is reading.

The card sits above the composer, on the first screen, before any turn exists. That
position sets the whole problem. The composer is the surface's purpose; the card is a
courtesy. A reader who arrives knowing their question types immediately, and anything that
shifts, delays or occupies the space under their cursor takes something from them to give
them something they did not ask for.

The derivation can come back empty for reasons that are ordinary rather than faulty. The
session may have turns already, or sit at a historical vantage. The store may hold no live
conclusion, so there is no interest to cross against. Nothing may have arrived that matches
one. Each of those is a correct outcome of a correct derivation, and a card is simply not
warranted.

It can also fail: a read errors, or the derivation runs past its budget. The reader cannot
tell those cases apart from the ordinary ones, and could do nothing with the distinction if
they could. This surface's copy rule is that an unreachable store, a refused read and an
empty result render as sentences a reader can act on, carrying no status code and no
vendor name — and here there is no action to offer. Meanwhile the operator can tell the
difference exactly, from the spans the derivation emits.

## Decision

Five conditions carry the card: a session with no turns, a session at present time, a store
holding a live conclusion, an arrived row matching an interest, and a derivation that
answered inside its budget. Any error or timeout raises `ConsoleBriefUnavailable` and the
card silently does not exist. Absence is the card's ordinary state, the composer is never
blocked or delayed, and a reader who types immediately wins that race.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Render only on all five conditions; absent otherwise, with no notice** *(chosen)* | The first screen is stable, the composer is never delayed, and a failure costs the reader nothing they can perceive. | A derivation that fails consistently is invisible to a reader and visible to an operator through spans alone, so a broken greeting can persist without anyone reporting it. |
| Render a skeleton until the derivation answers | The reader sees that something is coming, and a slow but successful derivation still lands its card. | Lost on composer latency. A reader who types immediately sees a first screen that shifts under the cursor, which is the exact cost the card exists to avoid imposing. |
| Render an error card | The failure is visible, and a reader can report it. | Lost on readability as a fault. The reader has no action to take, and copy describing the failure would have to name machinery this surface keeps off the page. |
| Retry on a backoff before the first turn | Recovers a transient failure without the reader noticing the first attempt. | Lost on composer latency for the same reason as the skeleton, and a retry window that outlives the first turn would land a card into a session already in progress. |

## Criteria

1. **Whether the composer is ever blocked or delayed** — whether the reader's own question
   is the fastest path through the surface. *This criterion decided it.* The card is
   optional and the question is not; a mechanism that trades the question's latency for the
   card's completeness inverts what the surface is for, and the failure it protects against
   is one the reader cannot act on anyway.
2. **Whether an absent card is readable as a fault by a reader who cannot act on it** —
   whether the notice buys the reader anything.
3. **Operator visibility into failures** — whether the failure is observable somewhere. It
   is, in spans, which is why the reader-facing notice was affordable to drop.
4. **First-screen stability** — whether the layout under the cursor moves after paint.

## Consequences

Absence carries no information, which is what makes it cheap: every condition that does not
warrant a card, and every failure that prevents one, produce the same empty first screen,
and no copy has to be written for any of them. The card needs no exclusion written into
title derivation, the distiller or a vantage fork, because it lives outside the session's
turns and stops rendering once the first real turn lands.

The accepted cost is a monitoring dependency. A greeting broken for every reader looks
exactly like a store where nothing matched, and no reader will report it. Whether the card
works at all is a question only spans answer, so the derivation's failure rate is worth
watching rather than assuming.

## Revisit triggers

- The card's failure rate is observed to be materially above zero over a sustained window,
  which would argue for surfacing the distinction to an operator more loudly than a span.
- A deployment wants the card to arrive after the composer rather than never, which is a
  different trade and takes an entry animation that cannot move existing layout.
- The derivation acquires state of its own, which would break the property that it costs
  nothing while nobody is reading and re-open the budget question.
