# 0313 — A conclusion distilled from a reading session lands under its own scope

**Status:** accepted 2026-09-18
**Decides:** `console.learn.refusal.unscoped-learning`

## Context

Two quite different processes in the system produce durable conclusions about the same
subjects. A reading session distils an exchange into at most three plain-language
conclusions, each carrying the question that prompted it as its evidence. A synthesis pass
over landed rows produces conclusions of the same shape from data nobody asked about, on a
schedule, with a run as its evidence. Both hand their output to the engine, which
consolidates it, decides its standing, and stamps its provenance like any other row.

The two differ in everything a reader would want to act on. A session conclusion is one
reader's interpretation, produced under whatever framing that reader brought; a synthesis
conclusion is derived from the corpus itself. Their error modes differ, their half-lives
differ, and the appetite for retiring one in bulk differs. An operator who concludes that a
week of reading sessions ran against a mistaken assumption wants to drop exactly those
conclusions and keep the pipeline's.

The greeting derivation makes the same distinction for a different reason. It reads live
conclusions and folds them to distinct subjects to decide what a returning reader is
interested in, then crosses that set against rows that arrived while the reader was away.
The interests it needs are the ones a person asked about, not everything the store has ever
inferred. Without a way to name that subset, the derivation either widens its interest set
past what a reader recognizes or grows a second list to maintain beside the conclusions.

The conclusion substrate itself is not in question. Conclusions live in one place with one
recall path, one policy surface, one erasure path and one merge rule, and that is settled
elsewhere in the corpus. What is open is whether the origin of a conclusion is recorded on
the row or thrown away.

## Decision

A distilled conclusion carries the reading-session scope, and one landing without it raises
`ConsoleLearningUnscoped`. What a session concluded stays addressable apart from what a
synthesis pass concluded, in one table, through one recall path, under one filter. A
store's interests are its reading-session conclusions, so the greeting derivation names its
interest set by that scope alone.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A scope on the conclusion, refused when absent** *(chosen)* | One retirement, one policy edit and one query bound the whole of what sessions concluded, and the greeting's interest set is a scope predicate rather than a maintained list. | Every consumer of conclusions carries a scope filter, and a conclusion useful across both origins is either duplicated or read through two queries. |
| One unscoped conclusion space | Nothing to declare at the write, nothing to filter at the read, and a conclusion is a conclusion whatever produced it. | Lost on bulk retirement. Dropping what sessions concluded becomes a subject-by-subject sweep with a human deciding each row's origin from its evidence text, which is exactly the judgement the scope would have recorded. |
| A separate table for session conclusions | The strongest separation available, and no consumer carries a filter it did not ask for. | Lost on substrate. Recall, policy, erasure and merge are settled as one surface over one table; a second table forks all four, and a reader asking one question reads two places. |
| A per-reader scope | An operator retires one reader's conclusions, and a reader sees their own reasoning reflected back. | Lost on the greeting's interest set and on ownership: a distilled subject belongs to the store, so a colleague's question shapes what the next reader recalls, and per-reader scoping makes the interest set narrower than the store's own. |

## Criteria

1. **Bulk retirement** — whether an operator drops what sessions concluded without touching
   what pipelines concluded. *This criterion decided it.* Retirement is the operation where
   the two origins pull hardest apart, it is the one a mistaken framing forces, and it is
   the only one where the absence of the distinction cannot be recovered after the fact —
   the origin is not reconstructible from a conclusion's text.
2. **Precision of the interest set** — whether the greeting derivation names exactly the
   subjects a person asked about, without a second list beside the conclusions.
3. **Write-path cost** — what the surface pays per landing. One declared field, checked at
   the boundary.
4. **Substrate integrity** — whether recall, policy, erasure and merge stay one mechanism.
   This one outranked the separation an extra table would have bought.

## Consequences

Retirement, policy and the greeting's interest set all become one predicate over one
column. The distinction is recorded at the moment it is known, by the party that knows it,
rather than inferred later from evidence text.

The accepted cost is a filter every consumer carries. Code that reads conclusions and
forgets the scope reads both origins and looks correct; the refusal guards the write, not
the read. A conclusion genuinely true of both origins has no shared home — it is written
twice or read through two queries — and there is no promotion path from a session
conclusion to a store-wide one.

## Revisit triggers

- A third producer of conclusions appears, making a two-valued scope the wrong shape and a
  producer identifier the right one.
- Read-side omission of the scope filter is observed in practice, which would argue for
  scope being a required argument of recall rather than an optional predicate.
- Conclusions are observed to be routinely true of both origins, so duplication rather than
  separation becomes the common case.
