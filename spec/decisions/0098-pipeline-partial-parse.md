# 0098 — A parse that covers part of an input refuses the whole input

**Status:** accepted 2026-09-18
**Decides:** `pipeline.land.refusal.partial-parse`

## Context

Many of the inputs a pipeline lands are multi-part: a document of pages, a workbook of
worksheets, an archive of entries, a feed of records. A reader over such an input can
fail in the middle — page forty of sixty decodes into garbage, a worksheet carries a
construct the extractor does not implement, an archive's central directory disagrees
with its entries. The reader has already produced thirty-nine good pages when it
stops.

Landed rows carry no completeness marker. A table holds text and provenance; nothing
in the row shape says "this is part of a document". A retrieval over that table ranks
the landed text, and an answer cites it as the document. The consumer of the answer has
no route to the fact that the remaining parts were never read, and neither does the
query that produced it.

That makes a truncated ingest and a complete one identical everywhere downstream. The
store reports holding a document whose second half nothing has ever seen, and every
question asked of it — does this contract contain a termination clause, what did the
board minute say — is answered confidently over the fraction that decoded.

Commit is also one-way. Rows written and committed for a run are visible; there is no
un-land. Whatever the decision is, it is made before the write rather than after it.

## Decision

A reader that reads part of a multi-part input and stops raises `PipelinePartialParse`
over the whole input. The parts it managed to read land nothing. The input is named the
same way a clean parse failure names it, so the run record carries one entry per input
rather than a count of parts.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the whole input** *(chosen)* | The store never holds a fraction of a document under that document's name; every landed document is whole | A document with one damaged part yields nothing, where most of its text was readable |
| Land the parts read and record a truncation flag | Keeps the readable text; the fact is recorded | Lost on downstream distinguishability: nothing consults the flag — not the retrieval, not the ranker, not the citation — so a partial document is quoted as the document |
| Land the parts and then refuse the run | Loud failure with the text preserved | Lost on ordering: the rows commit before the refusal, so the refusal is an annotation over data already readable, and the next read sees the fraction regardless |
| Land the parts under a distinct incomplete-document table | Text preserved, separation enforced by schema | Lost on cost and on reach: every read surface, fold rule and retrieval path would carry a second table shape, for a case measured in single-digit percentages |

## Criteria

1. **Downstream distinguishability** — whether any consumer can tell a truncated
   landing from a complete one. *(decided it)*
2. **Text preserved** — how much readable content the choice discards.
3. **Ordering against commit** — whether the choice is enforceable before rows are
   durable.
4. **Reach** — how many surfaces a shape change touches.

Distinguishability decided it because the failure it prevents is a wrong answer rather
than a missing one. Losing a document is visible: the operator sees a refusal, the
retrieval returns nothing, the question comes back unanswered. Holding half a document
under its full name produces a fluent, cited, wrong answer, and no surface in the
system flags it. Between a loud absence and a silent falsehood, absence is the
recoverable state.

## Consequences

Every landed document is whole, so retrieval, citation and aggregate counts over an
ingested table need no completeness predicate. That absence of a predicate is worth
more than the text discarded to buy it: a marker only some paths consult is worse than
no marker.

The cost accepted is discarded text. A sixty-page document with one damaged page lands
nothing, and the fifty-nine readable pages are unreachable until the input is repaired
or the extractor improves. For a source whose exports are routinely slightly damaged,
that is most of the corpus.

Repair is the operator's, and it is real work: re-export, re-encode, or split the input
by hand into parts that each parse. The engine offers no per-part landing verb to
shortcut it.

Reversing toward partial landing is cheap in code and expensive in data. The clause
flips in one place, but every row landed under the new rule is indistinguishable from
the whole documents around it, so the corpus after the flip cannot be separated from
the corpus before it.

## Revisit triggers

- A read surface starts carrying a completeness predicate that retrieval, ranking and
  citation all honor, which removes the distinguishability loss.
- Measured partial-parse rates on a real source exceed the rate at which whole inputs
  fail, so the discarded text dominates the corpus.
- An extractor gains part-addressable output where a part is independently citable and
  a missing part is visible in the citation itself.
