# 0044 — The relation-name allowlist, rather than the table-function check, closes the file-read path

**Status:** accepted 2026-09-18
**Decides:** `read.guard.refusal.unregistered-relation`, `read.guard.refusal.table-function`

## Context

The engine reads Parquet natively and treats a filesystem path as a relation. A statement
naming a bare path in a FROM clause parses as an ordinary base relation — the parse tree shows
a table reference with a name, indistinguishable in shape from a reference to a registered
view — and becomes a file read at bind time, when the engine resolves the name and finds a
file rather than a view.

That timing is the whole problem. A guard that inspects the tree for table-function nodes
looks at a representation in which the dangerous construct appears as nothing more than an
ordinary name. Rejecting table functions alone sails straight past a bare path, and the resulting
read reaches any file the process can open, which on a store machine includes every table the
caller has no grant for.

A registered relation is the unit that carries restriction. Ahead of a statement the engine
issues one create-or-replace view per table directory the caller's manifests name, each
arriving with its row predicate, its masks and its zone gate composed in. A name that resolves
to one of those views is a name whose restriction is already applied; a name that resolves to
anything else has no restriction at all.

Relations do not appear only at the top of a tree. A scalar subquery inside a select list, a
union arm and a pivot source each carry base relations, and a common table expression declared
anywhere in the statement may be referenced by a node beneath a different ancestor.

## Decision

Every base relation in an admitted statement names either a view registered for this caller or
a common table expression the statement itself declares; anything else raises
`RelationNotRegistered`, and the message names what the statement asked for while echoing no
relation belonging to another caller. The walk covers the entire tree and gathers
common-table-expression names across the tree ahead of checking any base relation. A table
function anywhere in the tree, and a schema-qualified reach into a system catalog, raise
`TableFunctionRefused` as a separate refusal.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Allowlist every base relation name, tree-wide; refuse table functions separately** *(chosen)* | A file read is unreachable: a path is a name, and an unregistered name is refused before bind time. | The walk visits every node of every admitted statement, and CTE names are gathered tree-wide ahead of any check, which costs one extra pass. |
| Reject table-function nodes alone | One narrow check, cheap and obvious. | Loses on reachability — a bare path parses as an ordinary base relation and becomes a file read at bind time, so the check never sees it. |
| A denylist of path-looking strings | No allowlist to maintain per caller. | Loses on the same criterion for an encoded, computed or unusually spelled path. |
| Check only top-level FROM items | A single pass over a small part of the tree. | Loses on hiding places: a scalar subquery, a union arm and a pivot source each carry relations the check never reaches. |

## Criteria

1. **Reachability of a file read** — whether any admitted statement can cause the engine to
   open a file the caller has no relation for. Three of the four options fail this.
2. **Completeness of coverage over the tree** — whether a construct can hide from the check by
   position rather than by form. Top-level checking fails this.
3. **Precision of the refusal message** — whether the error discloses relations belonging to
   another caller. Naming only what the statement asked for satisfies this.
4. **Cost of admission** — how much walking each request pays for. This is the criterion the
   chosen option loses on.

Reachability of a file read decides it. The other options each close a subset of the paths to
an unrestricted file read, and a subset is worth nothing here: the caller picks which path to
take.

## Consequences

The admitted set has a simple description an operator can state — a caller may read the
relations registered for it and the CTEs its own statement declares — and that description is
true rather than approximately true. The table-function refusal stays as its own rule for its
own reason, closing the schema-qualified reach into a system catalog that an allowlist over
base relation names would not address.

The accepted cost is two passes over every admitted statement, which is paid per request on
every read. For the statement sizes a caller writes by hand this is small; for machine-generated
statements with deeply nested subqueries it is unmeasured.

Reversing this is expensive in the direction that matters: loosening the allowlist to admit a
name class re-opens the bind-time file read, and there is no narrower version of that
loosening.

## Revisit triggers

- The engine gains a way to resolve names against an explicit catalog only, so an unresolvable
  name cannot fall through to a file read at bind time.
- Per-request walk cost is measured as material for machine-generated statements.
- A construct is found that introduces a base relation without appearing as a base relation
  node in the serialized tree, which would mean the walk's coverage is incomplete.
