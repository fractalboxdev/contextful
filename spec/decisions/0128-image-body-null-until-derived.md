# 0128 — An image source lands provenance with a null body, paired with a quotability marker

**Status:** accepted 2026-09-18
**Decides:** `connector.source.refusal.image-header`, `connector.source.refusal.marker-body-pairing`

## Context

An analyst's evidence is frequently an image: a chart in a deck, a scanned filing, a
photographed whiteboard, a page rendered out of a PDF. Excluding images from the store
excludes exactly the material a citation most often points at.

An image's text arrives two ways, and the two are not the same kind of fact. Optical
character recognition over a scan produces the document's own words — a citation may quote
them, because the quote is what the page says. A model's reading of a chart produces an
interpretation: "revenue grew through Q3" is not printed anywhere on the chart. Quoting an
interpretation as the document's own words is a fabricated quote with a real citation
attached to it, which is the worst failure the read path can produce, because it is
indistinguishable from a real quote at the point where a reader checks.

Neither kind of text is available on the ingest path. Extraction and model reading are
deferred work that runs over rows already landed. What the ingest path can read cheaply is
the file header: dimensions, a content hash, an embedded capture instant, the file
modification time. That is provenance, and it is enough to make the row addressable so
deferred work has something to fill.

A header that does not parse leaves the ingest path with a choice. Landing a row of zeroes
puts `width: 0, height: 0` in the store, and zeroes read as facts: a query filtering images
above a size threshold silently excludes them, and nothing says the values were never read.

The body and its quotability marker are two columns holding one fact. Any state where one is
set and the other is not is a citation hazard: a body with no marker is text of unknown
provenance that a citation will quote, and a marker with no body is a claim about text that
does not exist.

## Decision

An image source lands the provenance row — path, content hash, width and height read off the
file header, a capture instant from embedded metadata where present, the file modification
time, a modality marker — with a null body and a null quotability marker. An image header
that does not parse raises `ConnectorImageHeaderUnreadable` naming the path, and no pixel
decode happens on the ingest path. The quotability marker and the body are null together and
non-null together; any other combination raises `ConnectorQuotabilityMismatch`.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Provenance row, null body, marker paired with the body by refusal** *(chosen)* | A citation can always tell the document's own words from a model's reading of it, at the row level. | Two columns are kept consistent by a refusal rather than by construction, and an image table is not useful for retrieval until deferred work has run over it. |
| One text column with no marker | One column, no pairing rule, and retrieval treats image text like any other text. | Lost on citability: a citation quotes a paraphrase as source text, which is a fabricated quote carrying a real provenance chain. |
| Refusing images entirely | No unquotable text in the store, no marker, no deferred work. | Lost on coverage: a chart in a deck and a scanned filing are precisely the material an analyst cites, so the store is missing the evidence most often asked about. |
| Landing a zero-dimension row on an unreadable header | The file is present with its path and hash; deferred work can still try. | Lost on honesty of landed values: zeroes read as facts and silently change the result of any query predicating on size. |
| Decoding pixels at ingest to recover dimensions | Recovers dimensions for a file whose header is damaged but whose data is intact. | Lost on ingest cost and on parsing surface: a full decode per image on the ingest path, over attacker-reachable files, to salvage a rare case. |
| A single body column typed as a union of the two kinds | One column, and the kind travels with the value. | Lost on downstream ergonomics: every reader unwraps a union before it can use text, and a reader that forgets is back to the one-column failure. |

## Criteria

1. **Citability.** Whether a citation can quote text as the document's own words and be
   right. This is the property the read path's trustworthiness rests on.
2. **Coverage of the material analysts actually cite.**
3. **Honesty of landed values.** Whether a column's value was read or invented.
4. **Ingest-path cost and parsing surface.**

Criterion 1 decides. Coverage argues against refusing images and criterion 4 argues against
decoding pixels, but neither is close to the weight of the first: a wrong answer is a bug an
analyst notices and corrects, while a fabricated quote with a valid citation is a bug that
survives review precisely because the citation checks out.

## Consequences

Every image in the store is addressable by path and hash and carries the provenance a
citation needs, and no image row can carry text whose quotability is unstated. Deferred work
fills body and marker together, and any implementation that fills one alone fails loudly.
The ingest path stays cheap — header reads, no decode — so a directory of ten thousand images
ingests at file-stat speed.

The cost accepted: an image table is not useful for retrieval until deferred work has run
over it, so an operator who points the source at a collection and queries immediately finds
rows with no text. The gap between ingest and usefulness is a real operational surprise, and
nothing in the ingest run's outcome says "this table is not queryable yet".

The pairing is an invariant held by a refusal rather than by a type, so it is enforced at
write time rather than made unrepresentable. A type carrying both together would be stronger;
the columnar landing shape is what makes two columns the natural representation.

## Revisit triggers

- A third kind of image-derived text appears — a caption written by a human, say — which the
  two-valued marker cannot express.
- The store gains a composite column type that makes the body-and-marker pairing
  unrepresentable when broken, removing the need for the refusal.
- Header-unreadable refusals turn out to be common in real collections, making the
  pixel-decode salvage worth its ingest cost.
