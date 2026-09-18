# 0125 — A document that cannot be read whole refuses rather than landing the part that parsed

**Status:** accepted 2026-09-18
**Decides:** `connector.source.refusal.document-unreadable`, `connector.source.refusal.document-truncation`, `connector.source.refusal.input-unreadable`, `connector.source.refusal.frontmatter-shape`

## Context

The document sources walk an operator-pointed directory and land one row per page, or per
heading past a length threshold. The material is whatever a real collection holds: exported
filings, scanned contracts, password-protected attachments, notes with hand-written
frontmatter, and files whose extension lies about their contents.

Document parsers fail partially by nature. A text extractor reaches page 40 of 120 and hits
a malformed object; an encrypted file yields structure but no text; a spreadsheet reader
reads three worksheets and dies on the fourth. Each of these has a partial result sitting in
memory, and landing it is the path of least resistance.

The problem is what a partial result means downstream. Rows land, the run reports success,
and the store holds a document of forty pages under an identity that says it is the whole
document. Nothing distinguishes it from a forty-page document. A reader asking what a filing
says gets an answer grounded in a third of it, with no signal that two thirds are missing —
which is worse than no answer, because the answer is confidently wrong and the grounding
looks complete.

The empty-body variant is the same failure wearing a different shape. An encrypted document
that lands a row per page with an empty body reads downstream as a document the store holds
and has nothing to say about, which is a real state for a blank scan and a false one here.

A note's frontmatter is the same question in the schema rather than in the body. Frontmatter
is a flat scalar-and-list subset because those keys become columns. A nested map or a block
scalar has no flat column shape, and dropping it silently loses a column the note's author
wrote — a filter, a status, a date that a later query predicates on and finds nothing for. A
key carrying the reserved producer prefix shadows a column the engine sets, so the note's
own value overwrites provenance.

Failure also has to be typed correctly against the retry schedule. Input a parser cannot
read is data, not a fault: retrying it three times reads the same bytes and fails the same
way, and each attempt costs the whole walk.

## Decision

An encrypted document, and one carrying no extractable text on any page, raise
`ConnectorDocumentUnreadable` permanently, naming the path. A reader that reaches part of a
document and stops raises `ConnectorPartialParse` for the whole. Input a parser cannot read
raises `ConnectorInputUnreadable` permanently, naming the path and, where the input has
internal structure, the position inside it — the page, the worksheet, the entry. Permanent is
terminal against the retry schedule. A note's frontmatter outside the flat scalar-and-list
subset — a nested map, a block scalar, or a key carrying the reserved producer prefix —
raises `ConnectorFrontmatterRejected`. The failing unit is that table's read.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the whole document, permanently, naming path and position** *(chosen)* | A landed document is a whole document; one bad file is answerable by name from the run record. | One unreadable file fails its table's read, so a large collection needs the offending file removed or converted before the next run makes progress. |
| Landing the pages that parsed | The collection ingests; 118 of 120 pages is most of the value. | Lost on distinguishability: the store reports holding a document whose remaining pages nothing has seen, and no downstream reader can tell. |
| Landing an empty body on every page of an unreadable document | The document is at least present, with its provenance and page count. | Lost on the same criterion: it reads as a document the store holds and has nothing to say about, which is a real state elsewhere. |
| Skipping the file and tallying it on the run record | The walk finishes; the operator sees a count of skipped files. | Lost on where the signal lands: a tally is a number in a run record nobody reads on a green run, and the collection quietly has a hole. The declined-extension tally exists for files the walk never tried to parse, which is a different fact. |
| Dropping an unsupported frontmatter key | The note lands; one key is lost. | Lost on recoverability: the note's schema silently loses a column, and a query predicating on it returns nothing with no error anywhere. |
| Treating an unreadable input as transient | Absorbs a truly intermittent read failure. | Lost on retry economics: the bytes do not change, so every retry fails identically and each one costs the walk. |

## Criteria

1. **Whether a truncated ingest is distinguishable downstream from a complete one.**
2. **Whether a silently dropped column or page is recoverable after the fact.**
3. **Whether a failure is answerable by name.** One unreadable document among five hundred
   should be identifiable from the run record without re-running the walk.
4. **Progress under a partly-bad collection.** How much of a directory lands when one file
   is broken.

Criterion 1 decides. Criterion 4 is where this loses, and it loses knowingly: an operator
facing a failed read has the offending path in the error and fixes it in minutes, while a
reader facing a silently truncated document has no signal at all and may never learn. The
asymmetry is between a visible cost paid once and an invisible cost paid on every later
question.

## Consequences

Every document in the store is whole, and every frontmatter key a note declares is either a
column or a named refusal. Run records name failing paths and inner positions, so triaging a
five-hundred-file collection is reading one error rather than diffing the store against the
directory. Permanent typing keeps a bad file from consuming a retry schedule.

The cost accepted: one unreadable document fails its table's read, so a collection
containing a single encrypted attachment does not ingest until someone removes or converts
it. For a directory under active human curation this is a recurring interruption, not a
one-time cleanup. The failing unit is scoped to the table rather than the run, which limits
the reach but does not remove it.

Requiring a flat frontmatter subset means notes written for other tools — which commonly
nest — need editing before they ingest.

## Revisit triggers

- Operators routinely respond to `ConnectorInputUnreadable` by moving files out of the
  walked root, which converts the refusal into a manual skip list and argues for a declared
  exclusion instead.
- A document format in use carries a per-section integrity signal, making a partial parse
  self-describing and therefore honestly landable.
- A per-document failure unit becomes available, so one bad file holds itself back without
  failing its table's read.
