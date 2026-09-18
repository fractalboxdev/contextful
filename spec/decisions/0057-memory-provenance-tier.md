# 0057 — A memory claim's standing is one derived column, and recall compares standing before score

**Status:** accepted 2026-09-18
**Decides:** `memory.synthesize.invariant.tier-derivation`, `memory.revise.invariant.tier-precedence`, `memory.revise.workflow.promotion`, `memory.recall.invariant.tier-order`

## Context

Claims reach the store by three routes with very different standing behind them. A human
writes one, or a version-controlled file a human wrote supplies it. A synthesis pass
distils one from rows that arrived through a source the deployment declared. A synthesis
pass distils one from rows the system fetched on its own initiative, chasing its own
question.

Recall serves all three through one door and ranks them. Within a tier the score multiplies
`confidence` by a rank-position factor of `(index + 1) / total` over the live claim set.
That factor is unbounded below at `1/N`: across 200 live claims the lowest-ranked position
contributes a factor of `0.005`, and across a larger set, less. No fixed confidence ceiling
per tier can hold an ordering against that, because the multiplier's range grows with the
corpus while a ceiling is a constant.

Standing also has to decide revision. One live belief per subject, predicate and scope is
the invariant recall depends on, and a self-directed conclusion contradicting a human's
curated claim would otherwise retire it.

Write authority is gated once, at the write surface, on the caller's grant. Every field of
the payload after that gate is trusted. A model composing a write payload composes every
field in it.

The word `authority` is already spoken for by the credential subsystem in another sense.

## Decision

Every claim carries `tier`, one of `curated`, `derived` or `researched`, computed when the
row is written from the writing grant and the run's source, and read from no field of the
write payload. A pass whose grounding mixed declared-source rows with a self-directed fetch
takes the lowest tier across its legs. Recall orders by `tier` first and by score second. A
claim retires a prior of equal or higher standing; a lower-standing claim contradicting a
live higher-standing one lands with a bounded validity end and never live. Promotion to a
higher tier patches the record and appends a marker pass in the shape supersession uses,
keeping the synthesizing agent recorded and stamping the promoting human beside it.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **One derived tier column, three levels, lexicographic precedence** *(chosen)* | Recall, row and column policy, the erasure cascade, sync and deduplication all stay on one substrate; the ordering holds at any corpus size. | A lower-standing claim contradicting a higher one lands not live, so the disagreement leaves recall with nobody shown it. |
| A parallel store for self-directed conclusions | Physical separation; no risk of a researched claim reaching a curated reader at all. | Loses on substrate count: it forks recall, policy, erasure, sync and deduplication, so every one of those grows a second implementation that must agree with the first. |
| A caller-supplied tier field in the write payload | Trivial to implement; a caller that knows its standing states it. | Loses on assertability: the write surface gates once on the grant and trusts every field after it, so a model could assert its own standing and a researched claim could present as curated. |
| A confidence ceiling per tier instead of tier-first ordering | One ordering key; no lexicographic comparison in the ranking path. | Loses on the `(index + 1) / total` arithmetic: the rank-position factor is unbounded below at `1/N`, so across 200 live claims no fixed ceiling ratio separates the tiers. |
| Two levels — human and machine | Simplest vocabulary; the distinction a reader asks for first. | Loses on the meaning of a declared source: an operator who declared a feed chose the source even where they did not write the claim, which is real standing a two-level scheme discards. |
| Naming the column `authority` | Reads naturally in prose about standing. | Loses on collision: the credential subsystem uses that word in another sense, and one word for two concepts in one corpus is a defect the registry refuses. |

## Criteria

1. **Substrate count** — how many storage, recall, policy, erasure and sync
   implementations the scheme requires. *This criterion decided the shape.* Memory's value
   rests on every claim living on the same tables under the same enforcement as ingested
   data; a second store for one class of claim doubles five subsystems and guarantees they
   drift.
2. **Assertability** — whether the entity whose standing is being recorded can set it. This
   eliminated the payload field. Derivation from the writing grant and the run's source is
   what makes the column a fact about the write rather than a claim within it.
3. **Ordering stability across corpus sizes** — whether the tier distinction survives the
   score's arithmetic. This eliminated the confidence ceiling on a measured property of the
   ranking function rather than on taste.
4. **Fidelity of the vocabulary** — whether the levels name distinctions that carry
   consequences. This set three levels rather than two.

## Consequences

A lower-standing claim that contradicts a higher-standing live claim lands with a bounded
validity end and never live. The disagreement is recorded and auditable, and recall shows
it to nobody — so a researched finding that genuinely overturns a curated belief sits in
the table unread until a human looks. That is the sharpest cost here, and the
corresponding unsettled question about surfacing unscoped collisions is open in the
contract.

Only the researched tier expires. The two tiers a human or an operator chose grow without
bound, so a curated claim written once stays live indefinitely with nothing forcing a
re-examination.

A mixed-grounding pass takes the minimum tier across its legs. A conclusion that was mostly
store-grounded and touched one self-directed fetch is stamped `researched`, understating
it. Taking the minimum is the direction that fails safe, and it is a systematic
understatement rather than a rare one.

Promotion is append-only in the shape supersession already uses, so the synthesizing agent
stays recorded beside the promoting human and a promoted claim's history reads as one line
rather than as a rewrite.

Reversing the derivation is expensive: every row already written carries a tier computed
under the current rule, and a changed rule applies only to rows written after it.

## Revisit triggers

- The ranking function's rank-position factor changes shape, which is the fact that
  eliminated a confidence ceiling.
- Claims landing not live under tier precedence accumulate on subjects where a reader later
  agrees the lower-standing claim was correct, which says the silence costs more than the
  invariant buys.
- Curated claims are observed going stale at a rate that argues for an expiry on tiers that
  currently carry none.
- A fourth standing distinction appears that the three levels cannot express without
  overloading one of them.
