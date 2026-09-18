# 0127 — A report source distinguishes a truncated page set, a wrong-grain metric, a mis-ordered stream and a rejected token from ordinary emptiness

**Status:** accepted 2026-09-18
**Decides:** `connector.source.refusal.page-cursor-missing`, `connector.source.refusal.metric-grain`, `connector.source.refusal.ordering-column`, `connector.source.refusal.report-auth`

## Context

An async-report source reads the shape most advertising and analytics vendors expose: the
run enqueues a report job, the vendor bakes it, and the connector then pages results. Every
one of those stages has a legitimate empty answer. A job can bake to zero rows. A partition
can have no activity on a day. A page can be the last one. Emptiness is ordinary, and a
downstream model reads an empty result as a real zero — zero spend, zero impressions, zero
activity — because on this kind of data that is usually what it means.

That makes every failure mode that produces an empty-but-successful batch dangerous in a way
it would not be on a document source. Four of them exist.

A response can announce that another page exists while naming its cursor in neither the
explicit cursor field nor the next link. Page existence and page cursor are two separate
facts; collapsing them into one means the connector concludes the report is finished,
retires it, advances the watermark over unfetched rows and reports success. The rows are
gone from the pipeline's future — the watermark will never come back for them.

A metric can be requested at a grain where the vendor estimates rather than computes it.
Unique-reach metrics are the canonical case: the vendor computes them per campaign and
estimates them per placement, and the estimate is not additive. A downstream sum over
placements produces a number that is wrong by an unbounded amount and carries no marker
saying so.

Report rows carry an engine-injected ingest stamp alongside the vendor's event time.
Ordering a stream by the ingest stamp orders by write time, so a seed backfill loaded late
outranks a live restatement of the same day — the stale value wins.

An authentication failure produces a vendor response the connector can easily treat as an
empty result set, and the retry schedule will then re-attempt it several times before the
run ends successfully with zero rows.

## Decision

A response announcing another page while naming its cursor in neither the explicit cursor
field nor the next link raises `ConnectorPageCursorMissing` as a hard failure. A metric is
admitted at the grain the vendor computes it and raises `ConnectorMetricGrainRejected` at a
grain where it is an estimate. Ordering a report stream by the engine-injected ingest stamp
raises `ConnectorOrderingColumnRejected`. An authentication failure on a report read raises
`ConnectorAuthTerminal` and is terminal against the retry schedule.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Type each of the four as a named failure rather than as emptiness** *(chosen)* | No path produces an empty-but-successful batch that a reader can mistake for a real zero; each failure names its cause. | A vendor whose paging metadata is genuinely sparse fails where a lenient reader would have continued. |
| Collapsing page existence and page cursor into one fact | Simpler paging logic; a missing cursor just ends the walk. | Lost on watermark integrity: the connector retires the report, advances over unfetched rows and reports success, and no later run revisits them. |
| Landing an estimated metric at every grain and marking it | The metric is available everywhere, with a flag. | Lost on downstream correctness: a sum over a marked column is still a sum, and nothing forces a reader to consult the marker. The number silently ruins an aggregate. |
| Ordering by write time | Ordering is always available, including on rows whose event time the vendor left null. | Lost on restatement correctness: a late seed outranks a live restatement, so the stream's latest value is the oldest write. |
| Retrying an authentication failure on the ordinary schedule | Absorbs a transient token-service blip. | Lost on the zero-reading criterion: the retries exhaust and the run lands an empty batch a model reads as zero spend. |

## Criteria

1. **Whether an empty-but-successful batch can read downstream as a real zero.** On spend
   and volume data it always can.
2. **Whether a gap can be advanced over silently.** A watermark past unfetched rows is
   unrecoverable without an operator resetting a position.
3. **Whether a landed number survives ordinary downstream aggregation.**
4. **Tolerance of vendor sloppiness.** How much non-conforming paging metadata the source
   absorbs before failing.

Criterion 1 decides, and it is why this contract is stricter than the generic HTTP source's.
Elsewhere an empty result is visibly empty; here it is a number a reader acts on. Criterion
2 makes the paging case the sharpest of the four, because the loss is not merely a wrong
answer now but rows the pipeline has permanently written off. Criterion 4 is the cost, and
it is paid deliberately.

## Consequences

Every stream's emptiness means one thing: the vendor had nothing. Watermarks advance only
over pages actually drained, so a truncated report is a failed read that resumes from a
committed position rather than a success with a hole. Metric selection fails at the point of
request rather than in a downstream aggregate nobody audits. An expired token fails once,
loudly, instead of burning a retry schedule and landing a zero.

The cost accepted: a vendor whose paging metadata is genuinely sparse — one that reports a
next page by convention and omits the cursor on the final page, or that varies which field
carries the cursor by endpoint — fails where a lenient reader would have continued, and the
pipeline is blocked until someone reconciles the vendor's actual behavior against the
refusal. How often this happens across the vendors in use is unmeasured.

Grain rules have to be encoded per vendor and per metric, which is a body of vendor knowledge
in the source that goes stale when a vendor changes what it computes. Nothing detects that
staleness automatically.

## Revisit triggers

- A vendor's documented paging omits the cursor in a case the refusal rejects, and the
  refusal is the reason a pipeline cannot run.
- A vendor publishes machine-readable grain metadata, which replaces the encoded per-metric
  rules with something that cannot go stale.
- Downstream readers gain a way to refuse a non-additive column at aggregation time, which
  would make landing an estimated metric with a marker safe.
