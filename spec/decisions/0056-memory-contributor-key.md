# 0056 — A fetched result stamps its contributor key as the result's registrable domain

**Status:** accepted 2026-09-18
**Decides:** `memory.synthesize.shape.contributor-key`

## Context

Every claim carries `support = {contributors, records}`, stamped from pre-aggregation
counts at write time. `records` counts evidence rows. `contributors` counts distinct
contributor keys the ingesting pipeline declares per source row, falling back to distinct
source tables and then to 1.

A reader uses those two numbers to weigh a claim. The useful distinction is between nine
independent publishers agreeing and one publisher restated nine times, and `records` alone
cannot draw it: a syndicated story carried by nine outlets and a story reported
independently by nine outlets both land nine rows.

Fetched results are the hard case. They arrive from a search over an index, not from a
declared feed, so the ingesting pipeline has no operator-supplied notion of who the
contributor is. What it has is a link.

A link supports several candidate units. They differ in how they behave at two boundaries:
one publisher operating several subdomains, and several publishers operating under one
host. Most real publishing is the first shape; the second is rarer and mostly appears in
platforms that host many authors.

## Decision

A fetched result stamps its contributor key as the result's registrable domain, so
`contributors` counts distinct publishers rather than distinct retrievals of one publisher.
Two results from different subdomains of one publisher contribute one, and two results from
two publishers contribute two.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Registrable domain** *(chosen)* | The coarsest unit that separates publishers without splitting one publisher across its own subdomains; available on every fetched result. | A publisher network operating under many registrable domains inflates the count, and a syndicated story counts once per carrying domain. |
| Full hostname | Sharper — distinguishes two genuinely separate operations that share a registrable domain. | Loses on splitting: one publisher's subdomains inflate the count, which is the exact failure the metric exists to prevent, and it is the common case rather than the rare one. |
| Source table alone | Already declared, needs no parsing, and is the fallback for non-fetched rows. | Loses on resolution: every fetched result lands in one table, so `contributors` collapses to 1 for the entire class of rows this decision is about. |
| Author or byline | Measures the unit a reader actually cares about — an independent person asserting a thing. | Loses on availability: most fetched results carry no byline, so the key is absent for the majority and the count becomes a measure of metadata richness. |
| A curated publisher map | Handles networks and syndication correctly. | Loses on availability in a second sense: the map has to exist, be maintained per deployment, and cover the open web, and a missing entry silently falls back to something else. |

## Criteria

1. **Independence fidelity** — whether the count measures independent publishers or merely
   distinct retrievals. *This criterion decided it.* `contributors` exists solely to
   separate agreement from repetition, and an option that fails it is not a worse metric
   but a different one.
2. **Availability** — whether the key can be derived from every fetched result without an
   external input. This eliminated the byline and the curated map, both of which are
   sharper where present and absent too often to be the rule.
3. **Failure direction** — which way the count is wrong when it is wrong. The registrable
   domain over-counts a publisher network and under-counts nothing structural; the
   hostname under-counts agreement by inflating a single source, which reads as
   corroboration that does not exist.
4. **Cost to compute** — parsing a link versus consulting a store. This broke no tie; every
   surviving option is cheap.

## Consequences

Support counts read honestly for the common shape and over-count for a publisher network
that operates many registrable domains: each of its properties contributes, so one
editorial voice can present as several. The number is a floor on repetition, not a proof of
independence, and a reader weighing a high `contributors` count on a contested subject
still has the evidence row identifiers to check.

A syndicated story counts once per carrying domain, which is the same over-count from the
other direction: nine outlets republishing one wire report look like nine contributors.
Detecting syndication would require comparing content across results, which the write path
does not do.

`support` is computed at write time and takes no part in the deduplication key, so a replay
whose attribution shifted is a no-op on the row it already wrote. A change to how the key
is derived therefore does not retroactively correct existing rows; it applies to rows
written after it.

Under a banded reporting setting the count renders as a band rather than a number, which
absorbs small derivation differences and leaves the boundary cases visible only at the
band edges.

## Revisit triggers

- Claims on contested subjects are observed carrying high `contributors` counts that a
  reader checking the evidence rows finds to be one publisher network, meaning the
  over-count is reaching decisions.
- Fetched results begin carrying a reliable publisher identifier of their own, removing the
  availability objection to a sharper key.
- A deployment's sources are dominated by multi-author platforms sharing one registrable
  domain, where the chosen key under-counts genuine independence.
