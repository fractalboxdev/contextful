# 0126 — The engine reads a workbook by named archive parts and answers a compound-binary container with a conversion command

**Status:** accepted 2026-09-18
**Decides:** `connector.source.refusal.conversion-required`, `connector.source.refusal.cell-out-of-range`, `connector.source.refusal.external-reference`, `connector.source.refusal.workbook-incremental`

## Context

Spreadsheets are how most organizations actually hold tabular data, so a workbook is a
source the engine reads directly rather than one an operator converts by hand. Two container
families carry the same logical content. The modern one is a ZIP archive of XML parts, read
by naming the parts: four for a workbook, two for a word-processor document, so charts,
pivot caches, macros and drawings are absent structurally rather than by a filter. The
other is a compound-binary container — a different format entirely, with its own record
grammar, its own encryption, and a parsing surface that has been a steady source of memory-
safety findings for thirty years.

A workbook is also a URL a scheduled job reads, not a document a person hands over once. The
same file is fetched weekly and re-read. That cadence is what decides whether a conversion
step is acceptable: a conversion that runs once at import is a minor inconvenience, and a
conversion that runs on every scheduled read is either a manual step before each run or a
headless office suite the deployment now operates — a dependency an order of magnitude
larger than the engine itself.

Workbooks reach outward in two ways that the connector's outbound mediation never sees.
External references and external-links parts name other workbooks; relationships marked
external name arbitrary targets. Following one reaches a host the manifest never named.
Not following it and landing the cached value instead lands another workbook's data wearing
this one's provenance — a number whose lineage is silently wrong.

Incremental reads against a workbook fail for a narrower reason. Cell styles are unread, so
a date cell lands as its serial number, which is a variable-width decimal. A lexicographic
watermark over variable-width numerals orders `9` after `10`, which advances the watermark
past rows the run never landed.

## Decision

The engine reads an office container by exact part name. A compound-binary office container
raises `ConnectorConversionRequired` naming the conversion command, detected by magic bytes
as well as by extension so neither misnaming changes the answer. A cell past the header's
width raises `ConnectorCellOutOfRange`, and the worksheet header inherits the empty-name and
repeated-name refusals of the delimited reader verbatim. An external-reference declaration,
an external-links part, and a relationship marked external each raise
`ConnectorExternalReference` permanently. An incremental position against a workbook raises
`ConnectorIncrementalUnsupported`; the pairing becomes legal once the styles part lands.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Read archive parts natively; answer a compound-binary container with a conversion command** *(chosen)* | The common format reads on every scheduled run with no external dependency, and the risky format is answered with an actionable message rather than a parser. | Cell styles go unread, so every cell lands as text and a date lands as a serial number; the incremental pairing stays illegal until the styles part lands. |
| Parsing the compound-binary container natively too | Every workbook in an organization's history reads with no operator action. | Lost on the parsing surface: a second binary grammar with its own encryption and a long memory-safety record, inside a component that reads attacker-reachable files. |
| Refusing both container families and naming a headless converter | One uniform answer, no in-engine spreadsheet parser at all. | Lost on cadence: a workbook is a URL read weekly, so conversion becomes a manual pre-run step or an office-suite dependency the deployment runs and operates. |
| Following an external reference | The landed number matches what the spreadsheet displays. | Lost on outbound containment: it reaches a host the manifest never named, through a path the connector's request mediation does not cover. |
| Landing the cached external value | No request, and the cell is not empty. | Lost on provenance: it is another workbook's data wearing this one's, and nothing downstream can tell. |
| Reading the styles part now to type dates properly | Dates land as instants; incremental reads become legal immediately. | Lost on scope, not on merit: a number-format id is the author's display choice rather than a type, so a correct mapping is its own body of work. Deferred, not rejected. |

## Criteria

1. **Where the conversion would have to run, and how often the document arrives.** A weekly
   scheduled read makes a per-read conversion a standing operational cost.
2. **Parsing surface exposed to attacker-reachable input.** How many binary grammars the
   engine implements over files a walked directory can contain.
3. **Whether a landed value's provenance is what it claims.** External values and
   watermark-skipped rows both violate this.
4. **Whether a watermark can advance past rows that never landed.**

Criterion 1 decides between the chosen option and refusing both families. Criterion 2 is
what rules out the native compound-binary parser and is the reason the asymmetry between the
two families exists at all. Criteria 3 and 4 decide the external-reference and incremental
refusals independently of the container question; neither has a lenient answer that keeps a
landed number honest.

## Consequences

The common workbook format reads on a schedule with no dependency beyond the engine, and the
parts read are enumerated rather than filtered, so a format addition does not silently widen
what is parsed. Decompression, cell-text and row bounds are judged against the archive's
own claims and against what actually arrives, so a shared-string fan-out cannot turn a small
input into an unbounded landing.

The cost accepted: cell styles go unread. Every cell lands as a string and a date cell lands
as its serial number, so a downstream reader converts dates itself and the incremental
pairing is refused rather than approximated. Operators holding compound-binary workbooks
convert them once, by hand or by their own script, before the engine sees them — which is
real work for an organization whose archive predates the modern format.

Reading by exact part name means a container whose producer names parts differently is
unreadable even though a general ZIP-and-XML reader would manage it.

## Revisit triggers

- The styles part lands and number formats map to types, which makes the incremental
  refusal removable and changes what a date cell holds.
- Compound-binary containers appear often enough in operator collections that the manual
  conversion is a recurring blocker rather than a one-time archive cleanup.
- A memory-safe, sandboxed compound-binary reader exists as a dependency, which changes the
  parsing-surface answer without changing the cadence one.
