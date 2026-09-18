# 0123 — A table pattern binds table names into a request URL, and an unmatched table refuses before any request

**Status:** accepted 2026-09-18
**Decides:** `connector.source.refusal.table-unmatched`, `connector.source.refusal.placeholder-unbound`

## Context

Most vendor APIs expose many streams behind one URL shape: `/v1/{table}`, `/v1/accounts/
{account}/{table}`, `/v2/{table}/records`. Without a way to bind a table name into the URL,
each stream is its own pipeline with its own endpoint, its own credential binding and its
own schedule — twenty near-identical manifests for twenty streams of one vendor. A table
pattern makes one pipeline pull many streams, and each table holds its own position so
streams advance independently and adding one disturbs no other's watermark.

Binding introduces two ways for a manifest to be internally inconsistent. A URL can name a
placeholder no pattern binds, which is a build-time fact about two keys that disagree. And a
table can fail to match the declared pattern at run time, which is a fact about a table name
and a pattern that disagree.

Both have a tempting lenient answer, and both lenient answers produce a run that reports
success. Falling back to the unbound URL when a table does not match pulls one endpoint
under every table name: twenty tables land twenty copies of the same stream, each under a
different name, each with a position advancing as if it were a distinct stream. Skipping an
unmatched table lands nothing under it while the run's outcome is success, and a stream that
silently never lands looks exactly like a stream a vendor has no data for.

The table name is also manifest text that becomes part of a URL. A name carrying a slash, a
query delimiter or a percent sequence changes the request path rather than filling a segment
of it, which is why every value bound into a request line is percent-encoded.

## Decision

A table pattern binds table-name segments into the request URL. A table that does not match
the declared pattern raises `ConnectorTableUnmatched` ahead of any request. A URL naming a
placeholder with no pattern to bind it raises `ConnectorPlaceholderUnbound` at build. Every
bound value is percent-encoded.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the unmatched table before the request; refuse the unbound placeholder at build** *(chosen)* | Both inconsistencies are named failures, and the unmatched case costs no vendor request. | A stream whose URL shape differs from its siblings needs a second pipeline rather than one more entry in the table list. |
| Falling back to the unbound URL for an unmatched table | Every table list runs, whatever its members look like. | Lost on landing the right rows: one stream is pulled under many table names, each with its own advancing position, and the store holds n copies of one thing under n names. |
| Skipping the unmatched table and continuing | A typo costs one stream rather than the whole read. | Lost on distinguishability: nothing lands under that table and every run reports success, which is indistinguishable from a vendor with no data. |
| Inferring the pattern from the endpoint's placeholders | One key instead of two; they cannot disagree. | Lost on expressiveness: the pattern also constrains which table names are legal, which an endpoint's placeholder set does not express. Inference makes every name legal, which is the fallback option under a different name. |
| Validating table names against a discovery call | Catches a typo against the vendor's own stream list. | Lost on when the failure is discoverable and on cost: discovery is a live request per validation, and not every vendor exposes one. The pattern check needs no vendor at all. |

## Criteria

1. **Whether a mis-bound table lands wrong rows.** One stream arriving under several names
   is a corrupted store, not a failed run.
2. **Whether a non-landing table is distinguishable from an empty one.** A success outcome
   over zero rows is the ambiguity to avoid.
3. **Vendor spend on a manifest error.** Whether a typo costs requests.
4. **Manifest economy.** How many pipelines a many-stream vendor needs.

Criterion 1 decides. Criterion 2 rules out skipping and criterion 3 favors the pre-request
check, but the fallback option is the one that damages data rather than the run: rows that
landed under the wrong table name are already committed, already have positions advanced
past them, and are not distinguishable after the fact from rows that belong there. A failed
run is recoverable by fixing the manifest and re-running; a store holding twenty copies of
one stream is recovered by dropping tables and re-ingesting.

## Consequences

One pipeline covers a vendor's whole stream list as long as the streams share a URL shape,
and adding a stream is one entry in the table list. A manifest whose endpoint and pattern
disagree fails at build with both keys named. A misspelled table name fails before a socket
is opened.

The cost accepted: a stream whose URL shape differs from its siblings — one endpoint under
`/v1/legacy/{table}` while the rest are `/v1/{table}` — cannot join the list and needs a
second pipeline, with its own credential binding, schedule and destination configuration.
Vendors accumulate exactly this kind of exception as they version, so the cost is recurring
rather than one-off.

Percent-encoding every bound value means a table name is a path segment and nothing else; a
vendor whose stream identifiers legitimately contain slashes is unreachable through the
pattern.

## Revisit triggers

- More than one vendor in use needs a second pipeline solely for a differently-shaped stream
  URL, making the per-table URL override cheaper than the pipelines it replaces.
- A vendor's stream identifiers contain characters that percent-encoding renders unusable.
- The position store gains cross-table coordination, at which point the independence that
  per-table positions buy is no longer what makes this shape correct.
