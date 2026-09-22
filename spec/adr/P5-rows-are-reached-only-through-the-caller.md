# P5 — Rows are reached only through the caller's registered relation; one implementation per capability

**Status:** accepted

## Context

Enforcement holds only if it is always invoked. A path that reaches stored rows around the enforced relation, such as a file preview, a maintenance pass or a faster route for one surface, is where row predicates and masks stop applying. A second implementation of a rule, in another surface or language, drifts from the first, and the drift is found in production.

## Decision

Every read of stored rows, caller-facing or internal, traverses the caller's registered relation, into which the engine compiles grant filters, masks and a visibility semi-join. A path around it is a defect, never a documented exception. Enforcement, control-plane state, credential resolution, statement guarding and visibility each have one implementation, reached by a call or by querying the registered view.

- `authority.compose` runs one decision module, compiled native and to WebAssembly from one pinned source where a gateway decides.
- `disclosure.reach` binds visibility to the table, not the face; an unknown access class reaches no subject, and a grant row with no resource is refused at commit.
- `surface.ground` refuses a table function resolving a path straight against stored bytes.
- `assurance.structure-tree` requires a staleness check on every generated artifact and refuses a hand-written copy of an engine constant.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| One enforced relation, one implementation per capability *(chosen)* | — | No fast path around enforcement; a gateway carries a WebAssembly build of the engine module. |
| Exempt a documented set of internal paths | Coverage | The exemption list is where rows escape, and it grows with each maintenance feature. |
| A filter per surface held in line by a conformance suite | Failure mode | Two implementations of one rule diverge on the case the suite omits. |
| Post-filter returned rows | Leakage | Aggregates, errors and timing are computed over rows the caller cannot see. |
| Filter at ingestion, one copy per audience | Revocation latency | A grant change requires rewriting stored copies. |

## Consequences

- A surface that cannot call the engine implements an adapter against its ports, never a restatement of its contract.
- Every maintenance and compaction path pays the relation's cost.
- A visibility audit reads one binding per table.
