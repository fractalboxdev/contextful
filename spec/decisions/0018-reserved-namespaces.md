# 0018 — The engine reserves a column prefix and a set of table namespaces and refuses them to an application

**Status:** accepted 2026-09-18
**Decides:** `store.reserve.refusal.reserved-column-name`, `store.reserve.refusal.reserved-table-name`

## Context

The engine writes into the same tree an application writes into. It injects provenance
columns on every row — the ingest stamp, the run identifier, the batch sequence, the site,
the authorizing subject — and those are engine knowledge rather than claims carried in
from a source: a producer that sets one sees its value replaced. It also owns tables of
its own: the durable run record, and the whole prefix the visibility engine mirrors access
data under.

Both collisions are silent in the same way. A producer column spelled inside the reserved
column namespace is overwritten by the injected value at write time, so the producer's
data is gone and the column still exists with plausible content. An application table
declared with the run record's name receives engine rows on top of the application's, with
no error and no marker distinguishing the two populations.

Neither failure surfaces in a count, a schema check or a manifest diff. The shapes match,
the rows are present, and the query that reads the column returns a value — the wrong one.
That is what separates this from an ordinary naming conflict, where the second writer
fails and somebody notices.

The reserved column namespace has a deliberate opening: four reserved optional columns a
producer may set, which travel with a row and surface in the provenance envelope. So the
rule is not "the prefix is closed" but "the prefix is closed except for an enumerated set",
and the refusal has to be written against that set rather than against the prefix alone.

## Decision

The leading-underscore column namespace belongs to the engine. A producer column spelled
inside it and outside the reserved optional set raises `StoreReservedColumnName` at schema
reconciliation, ahead of any Parquet. The engine reserves two table namespaces — the
durable run record and the prefix the visibility engine mirrors access data under — and a
pipeline declaring a table inside either raises `StoreReservedTableName` when the manifest
is assembled, naming the reservation it collided with.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Reserve the prefix and the namespaces; refuse a collision** *(chosen)* | A collision fails at declaration time, where it is one rename | An application inheriting a schema that uses a reserved name renames its own table or column, and the reserved set is a compatibility surface that grows with the engine |
| Namespace the engine's tables under a vendor prefix and reserve nothing | No application name is ever refused | Lost on ergonomics of the read path: the reserved names are the ones a consumer queries by name, and a vendor prefix makes every read path carry it forever to avoid a collision that a refusal prevents once |
| Accept an application declaration and rename it on collision | Every manifest applies; nothing fails | Lost on correspondence: the renamed table is not the one the manifest names, so every downstream reference to that name resolves to something else or to nothing |
| Warn on a collision and let the write proceed | Preserves both the declaration and the deployment | Lost on undetectability: a warning at validation supplies exactly the signal the collision already lacks, and the overwritten column still reads as plausible data |

## Criteria

1. **Undetectability of the collision** — whether any observable artifact differs when it
   happens. *(the one that decided it)* An application table sharing the run record's name
   is overwritten by engine rows with no error, and a producer column inside the reserved
   prefix is replaced by an injected value; neither shows in a count or a schema check.
   Breadth of accepted names was the competing criterion and lost, because a refused name
   costs one rename at declaration time and an undetected collision costs data that still
   reads as correct.
2. **Correspondence between a manifest and the tree** — whether the name declared is the
   name written.
3. **Read-path ergonomics** — what every consumer pays to address engine tables.
4. **Breadth of accepted application names** — how many existing schemas apply unchanged.

## Consequences

The injected provenance set is trustworthy by construction: a column inside the reserved
prefix was written by the engine, and a reader needs no per-table knowledge to rely on
that. A manifest that assembles is a manifest whose table names are the ones on disk.

The cost accepted is migration friction and a growing compatibility surface. An application
arriving with a schema that already uses a reserved name renames its own table or column
before it can land a row, and each future engine table or injected column narrows the space
of legal application names — so a name that is legal today can be refused by a later engine
version, on a store that already writes.

Adding to the reserved set is therefore a breaking change to declaration, not an additive
one. The reserved optional column set is the release valve, and expanding it is the cheaper
move when a producer genuinely needs to write provenance.

## Revisit triggers

- A reserved table namespace acquires no engine writer, making the reservation a
  restriction with nothing behind it.
- Producers repeatedly need to set provenance the reserved optional set cannot express,
  indicating the opening is too narrow rather than the prefix too broad.
- A later engine version needs to reserve a name that existing stores are known to use, at
  which point the additive-reservation assumption is what has to be re-opened.
