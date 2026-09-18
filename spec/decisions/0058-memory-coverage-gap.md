# 0058 — A coverage gap on a subject closes on a declared source, and a self-directed fetch leaves it open

**Status:** accepted 2026-09-18
**Decides:** `memory.synthesize.invariant.coverage-gap`

## Context

A synthesis pass that cannot answer a question about a subject from landed rows records a
coverage gap: the subject, the gap kind, the pass that observed it, and the instant it was
last observed open. The gap exists to tell an operator that the deployment's declared
sources do not cover something the workspace is being asked about.

The same pass can reach outside the store. A self-directed fetch chases the question,
returns results, and those results are enough to produce a claim — stamped `researched`,
carrying an expiry, and ranked below every declared-source claim.

So a gap can be observed and answered in one pass, which poses the question of whether the
answer closes it.

The two ways of covering a subject differ in what they buy per future run. A declared feed
is one edit that covers every later pass at zero marginal cost, from a publisher that stays
the same, through the ingestion path whose provenance the citation gate downstream reads. A
search hit buys one run's worth of snippets from an index with no publisher stability, and
it arrives by a route that bypasses the quality gate and the freshness window the declared
path applies by construction.

A source connector receives no store handle. It cannot resolve its own query against the
store, so anything a source needs to know about what is missing arrives as run parameters
from the caller that already holds the store.

## Decision

A coverage gap on a subject closes when a declared source covers that subject. An answer
assembled from a self-directed fetch is a stopgap: the claim lands, the reader is served,
and the gap stays open until a declared source covers the subject. The open-gap record is
what tells an operator to declare a feed.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Only a declared source closes a gap** *(chosen)* | The one indicator telling an operator to declare a feed survives being answered around; gaps track structural coverage rather than incidents. | Gaps stay open across runs that did answer the question, so the open-gap count overstates what a reader could not learn. |
| Close the gap on the research hit | The count means "questions the workspace could not answer", which is what it looks like it means. | Loses on signal preservation: it silences the indicator at the exact moment it fired, so a subject answered by ad-hoc fetching every run looks permanently covered. |
| Close the gap for a bounded window after a research hit | Suppresses the repeat noise while keeping the signal eventually. | Loses on marginal cost: it buys quiet at the price of the operator seeing the gap once per window rather than once, and the window is a number with nothing to set it from. |
| Let the source resolve its own query against the store | The source decides what it is missing; no parameter plumbing. | Loses on the connector interface: a source receives no store handle, so resolution belongs above it, arriving as run parameters from the caller that already holds the store. |
| Drop gaps entirely and rely on the researched tier | One fewer record; the tier already marks a stopgap answer. | Loses on signal preservation: the tier is per claim and per run, so no reader can see that one subject has been answered this way twenty times. |

## Criteria

1. **Marginal cost per future run** — what each way of covering a subject buys the runs
   after this one. *This criterion decided it.* One feed declaration covers every later run
   at zero marginal cost, with a stable publisher and the provenance the downstream
   citation gate reads. A search hit covers one run, from an index with no publisher
   stability, bypassing the quality gate and the freshness window by construction. The two
   are not substitutes, so treating them as interchangeable for closing a gap is a category
   error.
2. **Signal preservation** — whether the indicator survives being worked around. This is
   what eliminated closing on the research hit and the bounded window.
3. **Interface fit** — where resolution can physically happen. This settled that the
   subject a run targets arrives as a parameter rather than being resolved inside a source.
4. **Count legibility** — whether the number reads as what it is called. The chosen option
   scores worst here, and this is the cost accepted.

## Consequences

The open-gap count overstates what a reader could not learn, because it stays open across
runs that did answer the question. An operator reading it learns which subjects lack
structural coverage, not which questions went unanswered, and the record's name has to be
read carefully for that distinction.

Retention for the claims produced along the way leans on citation rather than on recall.
The request ledger records outbound fetches and records no reads, so an expired
`researched` claim survives when an artifact's provenance cites it — not when someone read
it. A claim that was recalled often and cited never expires out.

Closing behavior is defined by the source's declaration, not by the content of what
arrived. A declared source that covers a subject badly still closes the gap, so the gap
measures coverage and not quality.

## Revisit triggers

- Open gaps accumulate on subjects a declared source will never cover, making the count a
  standing backlog nobody can clear.
- The request ledger gains a read-side counterpart, at which point retention can ask
  whether a claim was recalled rather than whether something cited it, and the stopgap
  claim's lifecycle stops depending on citation alone.
- A self-directed fetch path appears that carries publisher stability and passes the same
  quality gate and freshness window a declared source does, which removes the marginal-cost
  asymmetry this rests on.
