# D12 — Claim standing is one derived tier; revision is scoped to a validity line

**Status:** accepted

## Context

Claims arrive from human curation, declared sources and self-directed research. A reader needs to know which kind outranks which, and a writer cannot be trusted to say. A belief about the past and a belief about the present concern one subject and predicate, yet neither retires the other.

## Decision

- `read.synthesize` stamps every claim with `tier`, one of `curated`, `derived` or `researched`, computed from the writing grant and the run's source and never from the payload. Grounding that mixes legs takes the lowest tier.
- `read.recall` orders by tier, then by score.
- `read.revise` lets a claim retire a prior of equal or higher standing. A lower-standing claim contradicting a live higher one lands with a bounded validity end, never live. Promotion patches the record, appends a marker pass, and stamps the promoting human beside the synthesizing agent. Two claims revise each other only when both are open-ended or both anchor to the same instant.
- A direct write accepts claims alone and names the entity upsert as the door for an identity row; an `observed_at` anchor sets a point interval and stays outside the dedup key.
- A fetched result's contributor key is its registrable domain. A coverage gap closes only on a declared source.
- Only a `researched` claim expires: a pass stamps a validity end and keeps the row, and a citation or a promotion retains it.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| One derived tier with lexicographic precedence *(chosen)* | — | A high-scoring researched claim ranks below a weak curated one, and promotion is a human action. |
| A caller-supplied tier | Forgery | Any writer promotes its own output. |
| A separate store for self-directed conclusions | Recall coherence | Every reader merges two stores under its own precedence rule. |
| A confidence ceiling per tier | Ordering | Scores from different sources are not comparable, so ceilings leak across tiers. |
| Retire on subject, predicate and scope alone | History | A past-anchored belief retires the present one. |

## Consequences

- A bounded read at an earlier vantage returns an expired claim, since expiry stamps rather than deletes.
- An open gap tells an operator which feed to declare.
