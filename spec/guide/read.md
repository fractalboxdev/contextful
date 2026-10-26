---
contract: read
---

# Reading and memory

## The problem the read face solves

A caller, such as an agent holding a capability token, asks questions of stored tables.
The caller sees only rows its grant admits, and no read writes a row. SQL queries,
ranked retrieval and memory recall all pass through the same registered relations and
return the same response shape.

The example agent wants the current status of one resource.

## Naming what a caller reads

Each session connection registers one view per manifest table
({{read.register.connection-views}}). A bare table name in the caller's SQL resolves
to that caller's restricted view ({{read.register.bare-name}}). The tool set is closed,
so a caller reaches nothing outside it ({{read.register.tool-set}}).

The agent first calls `context.describe` and learns row counts, declared indexes and
the published row ceiling ({{read.register.describe-payload}}).

## Which statements run

The guard reads the SQL engine's own parse tree, so no keyword list decides admission
({{read.guard.engine-own-parse}}). The text has to parse to exactly one read-only
SELECT ({{read.guard.single-read-only-statement}}).

The agent joins `documents` to a `read_parquet` call, and the guard refuses the table
function ({{read.guard.table-function}}). It retries with a join to a table it holds
no grant on; the error echoes only the requested name ({{read.guard.unregistered-relation}}).

Operator statements run without that guard, and no network interface exposes them
({{read.guard.statement-provenance}}). The operator runs a raw statement with `contextful query` ({{read.query.operator-verb}}).

```mermaid
sequenceDiagram
  participant A as agent
  box read
    participant T as tool server
    participant G as statement guard
    participant V as caller views
  end
  participant S as store
  A->>T: context.query with a token
  T->>V: register one view per table
  T->>G: parse and walk the statement
  alt write, table function or unregistered name
    G-->>A: error naming only the request
  else one read-only SELECT
    G->>V: run over restricted relations
    V->>S: scan committed parts
    S-->>V: rows
    V-->>T: admitted rows and cells
    T-->>A: one envelope, exact truncated flag
  end
```

## The response

Every transport serializes one projection: columns, rows, `truncated` and optional
blocks ({{read.respond.one-projection}}). `truncated` is set from an over-fetched probe
row, never from comparing counts ({{read.respond.truncation-is-exact}}). Zero rows is a
success, not an error ({{read.respond.zero-rows-is-success}}).

## Ranked retrieval

The agent switches to `corpus.retrieve` with the query "resource status" and a filter on
`resource_id` ({{read.retrieve.ranked-call}}). A table lacking `resource_id`
contributes no rows instead of running unfiltered ({{read.retrieve.unsatisfiable-arm-drops}}).

Ranking fuses vector cosine with lexical BM25 scores ({{read.rank.fusion}}). A
question's timeframe orders rows but drops none ({{read.rank.question-window-is-a-tier}}).
Each stored vector sits beside an `embedding_model` column naming the model that
produced it ({{read.embed.model-identifier}}).

To report that no document mentions a status change, the agent counts rows. A low top
score proves no absence ({{read.respond.coverage-is-a-count}}).

```mermaid
sequenceDiagram
  participant A as agent
  box read
    participant R as retrieval
    participant W as table arm
    participant X as vector sidecar
  end
  A->>R: corpus.retrieve with query and filter
  R->>R: split query into content tokens
  loop each registered table
    R->>W: recency-ordered candidate window
    opt sidecar under its size cap
      W->>X: nearest neighbours
      X-->>W: candidates re-joined through enforcement
    end
    W-->>R: candidates above the relevance floor
  end
  R->>R: fuse cosine and BM25, window first
  R-->>A: ranked rows with the retrieval block
```

Reads under one key reuse a pooled session and its connections
({{read.cache.session-pool}}).

## Reading at an earlier time

Every read tool takes three bounds: `as_of` freezes the corpus, `valid_as_of` picks what
was true, and `available_as_of` sets a simulated present. An `ASOF JOIN` matches each
row on availability ({{read.as-of-join.per-row-bound}}).

A read naming a published model build resolves that build, and an unknown build fails
instead of widening to the latest state ({{read.resolve-pin.unknown-build}}).

A credential carrying a horizon caps the simulated present. A call asking for a later
`available_as_of` fails, naming the argument ({{read.horizon.exceeds-horizon}}).

```mermaid
sequenceDiagram
  participant A as agent
  box read
    participant T as tool server
    participant J as as-of join
  end
  box store
    participant S as committed parts
  end
  A->>T: context.query with three bounds
  T->>S: scan snapshots committed by as_of
  Note over T,S: as_of freezes the corpus for the whole eval
  T->>S: keep rows valid at valid_as_of
  T->>S: keep rows available by available_as_of
  Note over T,S: available_as_of is the simulated present
  T->>J: driving rows and right relation
  J->>S: right rows available by each driving instant
  S-->>J: latest available right row per key
  J-->>T: joined rows
  T-->>A: rows with the three bounds echoed
```

`context.reference` resolves one stored citation, and an erased source answers
`erased`, distinct from `missing` ({{read.reference.erased-verdict}}).

## Notifications

A subscription notifies each committed run once, in commit order, with no row content
({{read.subscribe.commit-order}}). A resume position naming no committed run fails
and names the oldest one available ({{read.subscribe.position-unavailable}}).

## Memory

Memory is claims about entities, kept in ordinary store tables with reserved relation
types ({{read.declare.reserved-relations}}). A synthesis pass extracts claims from
source rows and retries an invalid response a bounded number of times
({{read.synthesize.extract-attempts}}). An exhausted batch goes to a dead-letter
table, and the cursor holds ({{read.synthesize.dead-letter}}).

```mermaid
sequenceDiagram
  box run
    participant P as synthesis pass
  end
  participant M as model endpoint
  box store
    participant T as memory tables
  end
  box read
    participant R as recall
  end
  participant A as agent
  P->>M: extract from rows since the cursor
  M-->>P: extracted claims
  alt invalid after bounded attempts
    P->>T: dead-letter, cursor held
  else valid
    P->>T: commit resolved, consolidated facts
  end
  A->>R: recall about a vendor
  R->>T: read candidate facts
  T-->>R: facts with evidence references
  R->>R: read evidence through the caller's session
  R-->>A: facts whose evidence the caller reads
```

A mention matching two identities dead-letters its claim instead of guessing
({{read.resolve-entity.ambiguous-mention}}). A writable server accepts operator claims
through one direct-write route ({{read.revise.served-write}}).

The agent asks memory what the workspace concluded about the resource. One fact cites a
row masked for this caller, so recall suppresses that fact and answers with the rest
({{read.recall.evidence-unresolved}}).

An edge walk follows only edges the caller reads, with citations that resolve
({{read.traverse.enforced-edges}}). A prediction registers through
`contextful memory predict` and settles against later observations
({{read.settle.predict-verb}}).
