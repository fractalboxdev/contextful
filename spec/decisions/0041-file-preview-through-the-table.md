# 0041 — A file preview resolves its path to a table and reads rows back through that table's relation

**Status:** accepted 2026-09-18
**Decides:** `read.register.refusal.file-preview-target`

## Context

The read face offers a data-file gallery: `context.files` lists paths relative to the store
root for the tables a caller reads, and `context.file` previews one of them. A reader
inspecting what a run landed uses this to see the actual bytes rather than an aggregate.

Every restriction this face applies lives in the relation, not in the path. A registered
relation for one caller arrives carrying its row predicate, its column masks and its zone
gate, composed in a single pass ahead of any top-K cut. A bare table name in a caller
statement, a template body or a ranking arm resolves to that caller's registered relation and
inherits all of it. Nothing about a filesystem path participates in that composition — a
path names a set of parts, and parts carry every row and every column the run wrote.

So a preview implemented as a file read has no access to the restriction that governs the
same rows read as a table. It would return masked columns unmasked and predicate-excluded
rows in full, for a caller whose query on that table returns neither.

Not every path resolves to a table. A snapshot part belongs to a fold over many runs rather
than to one landed run, a request-ledger file belongs to a child relation registered on the
owner read alone, a traversal or an absolute path names something outside the store, and none
of the four has a relation whose restriction could be applied.

## Decision

`context.file` resolves a path to its `(table, run_id)` pair and reads the rows back through
that table's registered relation, so row restriction, masks and the zone gate apply to a
preview identically to a query. A snapshot part, a traversal, an absolute path and a ledger
file resolve to no table and raise `FilePreviewNotATable`.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Resolve the path to a table and read through its relation** *(chosen)* | A preview and a query on the same rows return the same rows, with no restriction stated twice. A later restriction added to the relation reaches the preview with no change here. | A file carrying rows of more than one table, and a file outside a committed run, have no preview at all. |
| Read the file directly under a grant check on the path | The gallery previews anything on disk, including snapshot parts and ledger files. | Row predicates and column masks live in the relation and not in the path, so a path-level grant check admits rows and columns the same caller's query withholds. |
| Refuse previews entirely | No second read route exists, so nothing can diverge from the query path. | A reader inspecting a landed file has no route at all, and the file listing becomes a list of paths nothing can open. |

## Criteria

1. **Equality with the query path** — whether a preview can return what a query on the same
   table would withhold. The direct-read option fails this outright.
2. **Usefulness of the gallery** — whether a reader inspecting a landed file has a route.
   Refusing previews fails this.
3. **Single statement of restriction** — whether a restriction has to be written once or
   twice. A path-level check would re-state in path terms what the relation already states in
   row and column terms, and the two would drift.
4. **Coverage of the gallery** — what fraction of listed paths can actually be previewed. This
   is the criterion the chosen option loses on.

Equality with the query path decides it. Two read routes over one store that disagree about
what a caller may see is a defect no configuration fixes, and the caller who benefits from the
divergence has no reason to report it.

## Consequences

Every restriction added to a relation later — a new mask, a tighter predicate, a zone change —
applies to previews with no further work, which is what makes adding a restriction safe. The
preview path holds no restriction logic of its own to audit.

The accepted cost is coverage. A snapshot part has no single run and therefore no preview,
even though it is the file a reader is most likely to be curious about after a fold. A ledger
file has a relation but one registered on the owner read alone, so a preview of it would have
to reproduce that decision rather than inherit it, and it is refused instead. Reversing this
is expensive: a direct-read preview would need the full restriction stack expressed over
paths, which is the duplication this avoids.

## Revisit triggers

- Snapshot parts acquire a stable mapping back to a single table's relation, making a fold's
  output previewable under the same rule as a run's output.
- Operators report the gallery as unusable on stores where most listed paths are snapshot
  parts rather than run parts.
- A file format lands that carries rows of more than one table in one object, which would make
  the one-path-one-table assumption false rather than merely restrictive.
