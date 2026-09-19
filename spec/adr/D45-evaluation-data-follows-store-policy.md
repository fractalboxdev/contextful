# D45 — Evaluation data follows store policy

**Status:** accepted

## Context

An evaluation harness reads the store, writes ground truth into the tree, and emits traces. Each path is a way for primary data to escape the rules governing the rows it came from, or for the measurement to become circular.

## Decision

`assurance.evaluate` runs through the real access path and holds its artifacts under the store's own policy.

- A corpus carries zone and row-policy labels; an unlabeled corpus raises `EvalCorpusUnlabeled` rather than scoring the zero the fail-closed default produces.
- A golden set ingested into the store it measures raises `GoldenSetIngested`, held by a manifest lint and a connector-side path refusal.
- A golden mined from a live query or promoted from an outcome passes the store's write-time redaction and zone policy before commit; only an adjudicated label promotes.
- A run touching a deployed store exports redacted traces to a self-hosted collector inside the operator's perimeter, named per environment, retained under the store's zone policy.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Labeled corpora, two-guard golden isolation, store redaction, in-perimeter traces *(chosen)* | — | Labeling precedes the first number; the native golden set lives outside every ingestion root; a self-hosted collector is operated per environment. |
| Bypass enforcement for evaluation runs | Coverage | An over-restrictive policy would never surface as a recall regression. |
| Commit mined queries verbatim | Data movement | Customer text would replicate to every clone of the tree. |
| A redactor written for goldens | One implementation of the rules | A second redactor would diverge toward preserving more text. |
| Hosted trace collection for every run | Content | Primary data would leave the perimeter, beyond the reach of the store's deletion. |

## Consequences

- A zero in a report is a retrieval fact.
- Ground truth is a human's judgment, not the store's.
- Spans never outlive the rows they describe.
