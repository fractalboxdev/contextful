# D10 — The read surface admits one read-only statement over registered relations

**Status:** accepted

## Context

Callers send SQL. The embedded engine can read files, attach databases, install extensions and change settings, and a text blocklist cannot enumerate every spelling of those. A second parser can disagree with the executor about what a statement means.

## Decision

- `read.guard` walks the syntax tree the executor itself serializes. Admitted text is exactly one read-only `SELECT`. Every base relation names a view registered for this caller or a common table expression the statement declares; a table function or a reach into a system catalog refuses separately. A refusal echoes what the statement asked for, never another caller's relation.
- `read.register` lists only the templates a caller's grants cover, and a guessed template id refuses. A template's SQL names only the store's own tables as plain identifiers, checked at validation and at startup. Argument binding is strict, with no coercion.
- A filter's budget is checked once over the whole filter; an oversized condition refuses the whole read.
- `read.respond` resolves a file preview to its table and run and reads through that table's relation; the request ledger registers on the owner read alone.
- The process transport requires a token or an explicit owner flag. The run-stream socket authenticates before the upgrade and carries snapshots only.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Executor's own tree plus a relation allowlist *(chosen)* | — | Each statement is serialized once before execution, and a caller cannot name an unregistered relation even when harmless. |
| A token or regex blocklist over the text | Completeness | Every new engine verb or dialect spelling is a bypass until listed. |
| An independent SQL parser | Parser agreement | What the guard admits is not what the executor runs. |
| Refuse table-function nodes alone | Coverage | A schema-qualified name or catalog reach still reaches storage. |
| Check only top-level `FROM` items | Coverage | A subquery or common table expression carries the forbidden relation. |

## Consequences

- Read-only-ness is a property of the executed representation, not of a maintained list.
- A preview and a query apply identical row restriction, masks and zone gate.
- A socket connection mutates nothing; stop and approve travel as authenticated routes.
