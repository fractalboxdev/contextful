# 0312 — An unplaceable vantage refuses the request rather than falling through to the latest data

**Status:** accepted 2026-09-18
**Decides:** `console.set-vantage.refusal.unparseable-vantage`, `console.set-vantage.refusal.unbounded-web-leg`

## Context

A session reads one day. The vantage lives on the session, a conversation holds exactly one,
and every leg of a turn carries it: the chat, the browse templates, the store's published
output routes, and the public-web supplement. The reader is told which day they are reading
by a pill that opens both the empty state and the transcript, and the analyst is told its
frame in a single direction per turn — a rewound turn answers in the past tense as of the
snapshot.

So the turn's own text asserts a bound. The pill names the day, the prose is written as of
it, and each fact carries the day it happened. Those assertions are made before any leg has
returned, and they are made in the reader's own language, which is the form that gets
quoted.

A vantage arrives as a string. It comes from a hop the reader made, from a link a colleague
shared, from a bookmark, from a hand-typed value. Some of those will not parse.

The public web has its own version of this. It exposes publication date and no ingestion
clock, so a rewound turn sends an end-published bound at the vantage and additionally drops
every returned result dated after that instant locally — an index ignoring its own filter
therefore reopens nothing. That local drop depends on the bound parsing. If it does not, the
leg has no bound at all while the turn's text still claims one.

## Decision

A vantage that is neither a calendar day nor an instant answers `400` and raises
`ConsoleVantageUnparseable`, rather than dropping to the latest data under a turn whose text
asserts a bound. A web leg whose bound fails to parse fails that leg and raises
`ConsoleWebBoundUnparseable`; the turn proceeds on its remaining legs, which are the store's
own and are bounded on the store's clock.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse an unplaceable vantage; fail an unbounded web leg** *(chosen)* | What the turn's text asserts about time is what the legs actually read, in every case, with no silent divergence. Both refusals are exercisable in a test with no model. | A hand-typed or link-shared vantage in an unexpected spelling gives a reader an error rather than a result, and a flaky upstream date format takes the whole web leg out of a turn rather than degrading it. |
| Drop an unplaceable vantage and read the latest data | The reader always gets an answer, and the latest data is the most useful default. | Lost on the assertion: the pill and the analyst's frame still claim a day, so the reader is told they are reading history while the legs read the present. The answer is wrong in the one dimension the surface most explicitly promises. |
| Clamp to the nearest stop on the snapshot timeline | Always produces a valid vantage; stays inside the store's real history. | Lost on the same ground plus a shift the reader never sees: the pill names a day the reader did not ask for, and a shared link silently resolves to a different day for a store whose timeline has moved. |
| Answer the web leg unbounded and label the results in prose | Keeps the leg's coverage, which matters most on exactly the questions a historical vantage is asked. | Lost on the assertion: a label inside prose does not bound what synthesis treats as established, so a post-vantage result still shapes the conclusion while the turn reads as of the vantage. |

## Criteria

1. **Whether the answer's stated time basis can disagree with what the legs actually read** —
   **this criterion decided.** The turn's own text asserts a bound before any leg returns; a
   dropped or clamped bound makes that assertion false, and a false statement about the time
   basis is undetectable to the reader and travels with every quote of the answer.
2. **Reader recovery cost** — what a person does after the refusal. A `400` on a vantage is
   recoverable in one action; a wrong answer is not recoverable at all because it is not
   noticed.
3. **Coverage under a failed leg** — how much material the turn loses. This is the criterion
   the web refusal concedes.
4. **Testability without a live model** — both refusals fire on parse, so they are
   deterministic; a prose label's adequacy is not.

## Consequences

The pill becomes trustworthy without qualification: if the transcript says a day, every leg
read that day. A shared link either reproduces the colleague's exact time basis or fails
visibly, so two readers comparing answers are never comparing different days without knowing
it. Undated results are handled by a separate, deliberate rule — they are kept, because the
coverage matters most on historical questions, and both channels disclose the gap — which
stays coherent only because a parseable bound is guaranteed everywhere else.

The cost accepted: a reader who types or receives a vantage in an unexpected spelling gets an
error rather than a result, at the moment they were trying to look at history. And a single
upstream returning a malformed date removes the public-web leg from the turn entirely rather
than degrading it, so a turn that would have been supplemented by the web falls back to the
store alone, with the coverage loss landing on exactly the historical questions where the web
leg was most useful.

## Revisit triggers

- Vantage refusals are observed on spellings a reader could reasonably produce, meaning the
  accepted set is too narrow rather than the input too loose.
- Upstream date-format instability takes the web leg out of turns regularly, so the leg-level
  failure is a routine loss rather than a rare one.
- Per-result bounding becomes reliable enough that an unbounded leg can be filtered wholly
  locally, which is the ground the label option lost on.
