# 0336 — Traces from a run touching a deployed store export to a self-hosted collector inside the operator's perimeter

**Status:** accepted 2026-09-18
**Decides:** `build.baseline.refusal.hosted-trace-export`

## Context

Evaluation and observation runs emit traces, and the traces are useful precisely because
they are detailed: the question a caller asked, the ranking each leg returned, the rows the
reader was handed, the citations the judge scored. That is the content that makes a span
worth keeping when a regression has to be attributed to a leg rather than to a reader.

It is also primary data. A span carrying a visitor's query and the retrieved rows holds the
same text the store holds, under the same zone policy and the same retention rules — but
in a different system, reached by a different configuration, subject to whatever defaults
the collection tool ships with. Nothing about a span's format marks it as governed by the
store's policy, and a collection tool's default retention has no relationship to the
store's.

Hosted collection makes that a boundary crossing. The spans leave the operator's perimeter
through a path the store's policy never sees, to a third party the operator did not choose
as a data processor, and the store's own deletion has no reach into them. That is the same
movement the product exists to avoid, arriving through the observability configuration
rather than through the data path.

The configuration shape matters too. A single collector endpoint variable shared across
environments means one edit redirects every environment's traffic at once, including a
deployed store's, and the edit looks like an ordinary configuration change.

Some runs carry none of this. A run whose store is built entirely from public or synthetic
fixtures holds no primary data in any span, and the whole argument is inapplicable to it.

## Decision

An evaluation or observation run touching a deployed store exports its traces to a
self-hosted collector inside the operator's own perimeter, and a hosted endpoint configured
against such a store raises `TraceExportOutOfPerimeter`. Hosted collection reaches a run
whose store is built from public or synthetic fixtures alone. The collector endpoint is
named per environment, spans pass the redaction pass ahead of export, and their retention
follows the store's zone policy rather than the collection tool's default.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Self-hosted collection inside the perimeter for any run touching a deployed store, per-environment endpoints, redaction and store retention on the spans** *(chosen)* | Spans carrying primary data stay under the rules that govern the rows they came from, and one environment's endpoint cannot be flipped into another's. | An operator wanting hosted trace tooling runs a self-hosted collector instead, and every environment carries its own endpoint variable to maintain. |
| Hosted collection for every run | The best tooling with no operational burden — query, retention and alerting all supplied, and nothing to run. | Lost on content: it moves primary data outside the perimeter through a path the store's policy never sees, and the store's deletion cannot reach it. |
| A single collector endpoint variable shared across environments | One variable, one place to change it, no per-environment drift. | Lost on blast radius: one flip redirects a deployed store's traffic, and the change reads like ordinary configuration with no signal about what it moved. |
| Exporting unredacted spans to a self-hosted collector | Keeps the full detail that makes a span worth having, with no data leaving the perimeter. | Lost on retention: the spans then hold row text under the collection tool's retention rather than the store's, so they outlive the rows and survive a deletion applied to them. |
| Sampling or summarizing spans so no row text is carried at all | No primary data anywhere, and hosted tooling becomes available for every run. | Lost on diagnostic value: attributing a regression to a leg requires seeing which rows that leg returned, which is exactly the content the summarization removes. |

## Criteria

1. **Content** — what a span holds, and therefore which rules govern it.
2. **Retention** — whether span data can outlive the rows it was derived from.
3. **Blast radius of a configuration change** — how much traffic one edit can redirect.
4. **Diagnostic value** — whether a span answers the question it was collected for.
5. **Operational burden** — what an operator runs to get trace tooling.

**Content decides it.** The choice is routinely framed as a tooling preference, and it is
not: a span carrying a visitor's query and retrieved rows is primary data under the store's
own rules, and where that data may travel is settled by what it is rather than by which
tool produced it. Retention then rules out exporting unredacted spans even inside the
perimeter, and blast radius rules out the shared endpoint. Operational burden is the
criterion the chosen option loses on.

## Consequences

Trace data governed by the store's policy stays where that policy reaches, with the same
redaction and the same retention as the rows behind it. A configuration edit can misdirect
one environment rather than all of them. A run over public or synthetic fixtures keeps
hosted tooling available, so the restriction is scoped to the runs whose content earns it.

The cost accepted: an operator who wants hosted trace tooling runs a self-hosted collector
instead — a service to deploy, upgrade, back up and secure, in exchange for a capability
that is otherwise a configuration line. Every environment also carries its own endpoint
variable, which is more configuration to keep correct and one more thing to get wrong when
a new environment is stood up. The redaction pass ahead of export costs detail in the spans
themselves, which is the same trade the goldens make, on the data that is most useful when
it is most complete.

Reversing this is cheap to configure and irreversible in effect: pointing a deployed store's
traces at a hosted endpoint is one variable, and the spans that arrive there cannot be
recalled from a processor the operator never chose.

## Revisit triggers

- A hosted collector offers a deployment model where spans are processed entirely inside
  the operator's perimeter and the hosted side holds no row text.
- A redaction shape is found that preserves leg attribution without carrying row text at
  all, which would make the content criterion inapplicable and reopen hosted collection for
  every run.
- Per-environment endpoint configuration becomes a recurring source of misconfiguration,
  measured by environments stood up pointing at the wrong collector.
