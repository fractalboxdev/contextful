# 0120 — A body's decode is declared, and a spelling outside the declared vocabulary refuses at build

**Status:** accepted 2026-09-18
**Decides:** `connector.source.refusal.header-spelling`, `connector.source.refusal.format-key-mismatch`, `connector.source.refusal.declared-encoding`, `connector.source.refusal.clock-column-spelling`

## Context

The generic HTTP source reaches a credentialed vendor API through configuration alone: a
headers table whose values are literal text carrying reference placeholders, a `format` key
deciding how the body decodes, and a pagination shape. One decoder serves the HTTP, file
and object sources alike, so one spelling means one thing wherever a document arrived from.
That makes the manifest the whole description of a read — and makes every ambiguity in the
manifest a way for a read to change meaning without the file changing.

Inference is the main source of that ambiguity. A URL path extension predicts nothing about
a response body: a vendor serving `/export.csv` can answer with JSON, and a vendor serving
a bare path can start answering with something else next quarter. A manifest that infers
would silently re-decode without an edit, and the operator's first signal is a table whose
columns moved.

The manifest is also where credentials are named. A header value is text, and text
accommodates both a bare name and an environment scheme as easily as it accommodates a
reference placeholder — each of which would resolve material through a path the reference
scheme does not govern.

Delimited input carries the same problem one level down. A non-UTF-8 body decoded leniently
lands replacement characters, and a replacement character inside an identifier produces a
table that looks landed and joins to nothing. Per-cell type inference produces the matching
failure for identifiers: a leading-zero value reads as a number in one export and as text in
the next, so a dimension file's schema depends on its data. A clock column is the column the
watermark compares, and comparing text across two spellings of an instant advances a
watermark wrongly.

## Decision

A bare name and an environment scheme inside a header value raise
`ConnectorHeaderSchemeRejected` at parse, as does a malformed placeholder — unclosed, empty
or nested. Three key families raise `ConnectorFormatKeyRejected` at build against a
non-JSON format: the JSON record path, every pagination shape, and any decode key the chosen
format does not read. A non-UTF-8 delimited body decodes through a declared encoding label
and raises `ConnectorEncodingInvalid` where the bytes are not valid under it. A delimited
clock column is an RFC 3339 instant or a fixed-width digit stamp; another spelling raises
`ConnectorClockColumnRejected`. Over HTTP the format is explicit and is not inferred from
the URL; the two file-shaped sources infer it from the extension.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Declare the format and the encoding; refuse a key the format does not read** *(chosen)* | An unchanged manifest decodes the same way tomorrow; a mismatched key is a build diagnostic rather than a silently ignored line. | An operator writes a cast for a date-dense series, and a vendor changing its content type needs a manifest edit. |
| Infer the format from the URL path | Shorter manifests; the common case needs no key. | Lost on stability of meaning: an extension does not predict a response body, and inference would silently re-decode an unchanged manifest. |
| Infer the format from the response content type | Tracks what the vendor actually sent. | Lost on stability of meaning, one level out: the vendor, not the manifest, then decides what the pipeline does, and a content-type change becomes a schema change nobody authored. |
| Ignore keys the chosen format does not read | Tolerant manifests; a format switch needs no cleanup. | Lost on diagnosability: a record path left beside a delimited format reads as governing the parse and governs nothing, which is the same failure as an unbound binding. |
| Decode leniently with replacement characters | No read ever fails on encoding. | Lost on distinguishing a lossy landing from a clean one: a replacement character inside an identifier produces a table that looks landed and joins to nothing. |
| Infer cell types on delimited input | Numbers arrive as numbers; fewer casts downstream. | Lost on schema stability: a leading-zero identifier reads as a number in one export and as text in the next, so the schema depends on the data. |

## Criteria

1. **Whether an existing manifest can change what it decodes without the manifest
   changing.** *This criterion decided it.* The manifest is the reviewed statement of what a
   pipeline does, and every control above it — the allowlist, the bindings, the pin — rests
   on that statement staying true between reads. Convenience at authoring time is paid once;
   a manifest whose meaning drifts is paid on every read afterwards, by whoever is reading
   the resulting table.
2. **Whether a lossy landing is distinguishable from a clean one.** Whether a damaged value
   is visible or merely present.
3. **Stability of a landed schema across two exports of the same data.**
4. **Custody of material named in a header.** Whether a value can resolve through a path the
   reference scheme does not govern.

## Consequences

A manifest decodes a body one way until an operator edits it. A format switch surfaces every
key that no longer applies, at build, by name. A delimited source lands every cell as a
string and an empty unquoted field as null, so an identifier of `07` reads the same across
two exports and a dimension file's schema does not depend on its contents. The watermark
compares text over a clock column whose spelling is one of two known forms.

The accepted cost falls on the operator in two recurring places. A date-dense series lands as
text and needs a cast written in the read, which is friction on exactly the tables where it
is most annoying. And a vendor that changes its content type takes the pipeline down until
someone edits the manifest — a failure that inference would have absorbed, at the price of
absorbing it silently.

## Revisit triggers

- Casting a delimited clock column becomes the dominant thing operators write in reads,
  which would argue for a declared per-column type map rather than an all-string landing.
- A vendor population is observed changing content types often enough that the manifest
  edit is a recurring operational cost rather than a rare one.
- A third clock spelling appears widely enough that the two-form vocabulary refuses more
  real data than it protects.
