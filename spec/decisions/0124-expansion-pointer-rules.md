# 0124 — A row's document is reached exactly one way, and a failed follow-up fails the read

**Status:** accepted 2026-09-18
**Decides:** `connector.source.refusal.pointer-ambiguity`, `connector.source.refusal.follow-up-failure`, `connector.source.refusal.template-shape`, `connector.source.refusal.pointer-column-missing`, `connector.source.refusal.target-column-occupied`

## Context

Vendor APIs habitually split a stream into an index and a detail: a list endpoint returns
identifiers and a handful of summary fields, and the content lives one request deeper. An
expansion block lets one pipeline land both — the walk pulls the index, and for each landed
row it fetches the row's document into a named column.

The pointer is named one of two ways, and they are genuinely different mechanisms. A
template binds placeholders from the landed row's own scalar columns, so the pipeline
constructs the URL. A pointer column holds a URL the vendor already wrote, resolved against
the configured endpoint when relative and taken as written when absolute. A manifest can
carry both keys, and then two different URLs are each a legitimate answer to "where is this
row's document".

Expansion runs after the watermark filter, so the rows that land are the rows that are
expanded and a filtered-out row costs no vendor request. That ordering makes the position
the thing at risk. The read commits a position covering the rows it landed. If an index row
lands with its detail column absent because one follow-up failed, the position has advanced
past a row whose document nothing has, and the walk is forward-only: no later run revisits
it. The row is stranded — present in the store, permanently incomplete, and indistinguishable
from a row whose detail is legitimately empty.

The template is also a place where a response body can name a destination. Placeholders bind
from row values, which are vendor-supplied text. A template binding a placeholder into the
URL's authority — `https://{host}/items/{id}` — lets a response body choose where the
request carrying the operator's credential goes. Percent-encoding defends nothing here: a
hostname is unreserved characters throughout, so an encoded hostname is the same hostname.
A template carrying no placeholder at all is the opposite error: it fetches one fixed
document once per landed row.

Two more facts are unknowable at build time and knowable early at run time. Which columns a
row holds is unknown until one arrives, so a placeholder naming an absent column is caught
on the first row. Whether the index already carries the target column is knowable across the
whole landed set before any follow-up is spent — and an index whose hundredth row carries
the target column would otherwise cost ninety-nine follow-ups before the collision surfaced.

## Decision

An expansion names the pointer exactly one way; declaring both raises
`ConnectorPointerAmbiguous`. A failed follow-up raises `ConnectorExpansionFailed` for the
whole read rather than landing the index row with its target column absent. At build, a
pointer template carrying no placeholder, or binding one into the URL's authority, raises
`ConnectorTemplateRejected`. On the first landed row, a placeholder naming a column the rows
do not carry — or carry as an array — raises `ConnectorPointerColumnMissing` ahead of that
row's request. Ahead of the walk, judged over every landed row, a target column the index
already carries raises `ConnectorTargetColumnOccupied`.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **One pointer form; whole-read failure; template and column checks at the earliest knowable point** *(chosen)* | A landed index row always carries its document, and every refusal fires before spending what it would take to discover the problem later. | One vendor hiccup fails the whole read, and an expansion follows at most one pointer per row. |
| Landing the index row with the detail column null | One transient 503 costs one row's detail rather than a whole read; the rest of the batch lands. | Lost on position integrity: the position advances past a row nothing will revisit, and the null is indistinguishable from a legitimately empty detail. |
| Retrying the follow-up and landing null after exhaustion | Absorbs transient vendor failures before giving up. | Lost on the same criterion, one step later: the stranded row is stranded just as permanently, and the retry budget is already the step's own retry schedule. |
| Percent-encoding an authority binding and allowing it | Covers a vendor whose detail host varies per row. | Lost on credential containment: a hostname is unreserved characters throughout, so encoding changes nothing about where the request goes. |
| Discovering a missing target column lazily, per row | No pre-walk pass over the landed set. | Lost on spend for the target-column case: an index whose hundredth row carries the column costs ninety-nine follow-ups before the collision is found. For the pointer column the lazy answer is kept — first row, ahead of its request — since no cheaper point exists. |

## Criteria

1. **Position integrity.** Whether a committed position can cover a row whose data is
   incomplete on a forward-only path.
2. **Credential containment.** Whether a response body can name the destination of a request
   carrying the operator's credential.
3. **Spend before a refusal.** How many vendor requests a manifest error or a schema
   surprise costs before it is named.
4. **Tolerance of transient vendor failure.** How much of a read one 503 destroys.

Criterion 1 decides between whole-read failure and the null-column options; criterion 2
decides the authority binding outright and is not negotiable against convenience. Criterion
4 is the one this decision loses on, deliberately: a failed read costs the work already done
in that read, all of which is re-doable because the position did not advance, while a
stranded row costs data that no re-run repairs. Re-doable work beats unrepairable data.

## Consequences

Every index row in the store carries its document, so a downstream reader never distinguishes
"detail missing because a fetch failed" from "detail legitimately empty" — the first state
does not exist. Manifest errors in the template surface at build, schema surprises on the
first row, and column collisions before any follow-up is spent.

The cost accepted: one vendor hiccup anywhere in a batch fails that whole read, so an
expanding pipeline against a flaky vendor makes less progress per run than a lenient one
would, and against a persistently flaky vendor may make none. The expansion budget bounds
how much work one failure discards — at most 200 follow-ups, 256 MiB buffered and 600 s —
but within that bound the loss is total. An expansion also follows exactly one pointer per
row, so a vendor splitting a record across two detail endpoints needs two pipelines or an
authored connector.

The pre-walk pass over the landed set to judge the target column costs one traversal of the
batch's column names before the first follow-up, which is cheap relative to a single request.

## Revisit triggers

- A vendor in use requires per-row detail hosts, making the authority-binding refusal the
  reason a pipeline cannot exist.
- Observed expansion failure rates are high enough that expanding pipelines stop making
  progress, which would argue for a resumable expansion position rather than for landing
  nulls.
- A row-level position becomes available, so a failed follow-up can hold one row back
  without stranding it — which removes the reason whole-read failure is correct.
