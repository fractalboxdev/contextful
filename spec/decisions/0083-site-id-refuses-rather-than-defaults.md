# 0083 — An unresolved site id refuses startup, and declaring two sources refuses too

**Status:** accepted 2026-09-18
**Decides:** `run.record.refusal.site-id-unresolved`

## Context

The site id names the machine or deployment a row was written by. It is an engine-injected
provenance column beside the ingestion stamp and the run id, and it is a column on every run
record row. It is also structural rather than decorative: it is interpolated into each table's
run directory and into every request-ledger filename, which is why it is bounded to letters,
digits, dot, underscore and hyphen, from 1 to 64 characters.

That interpolation is what makes a wrong value expensive. Two sites sharing one identity do not
produce two confusable columns — they produce one directory tree that both of them write into,
with run directories and ledger filenames colliding. What lands there afterwards is not a
labelling mistake that a later query can untangle; it is two sites' output interleaved under
one name, and no row carries anything that distinguishes them.

The value also has two plausible sources. A manifest can carry it, which suits a deployment
described entirely in checked-in configuration. An environment binding can carry it, which
suits a fleet where one manifest is deployed to many machines and each supplies its own
identity. Both are reasonable, and a system that accepts both has to answer which one wins when
both are present — an answer nobody reads before they need it.

## Decision

A site id comes from the manifest or from a named environment variable. An unset variable
raises `SiteIdUnresolved` at startup, and declaring both sources raises it too, since two
sources for one value is a question about which one won. The refusal happens at startup, before
any run opens and before any directory is named, so a deployment that has not resolved its
identity does not run.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse at startup; refuse two declared sources** *(chosen)* | The identity is established before anything is named after it, and there is never a precedence question to look up. | A deployment that forgets the binding does not start, and a duplicate id across two sites is undetectable by this rule. |
| Fall back to a fixed default | Nothing ever fails to start, and a single-machine deployment needs no configuration at all. | Loses on the shape of the failure. Two sites sharing one identity merge into one apparent site, their run directories and ledger filenames collide, and nothing in the data says so. |
| Generate a value when unset — a hostname or a random id | Uniqueness without configuration, so collisions are avoided by construction. | Loses on stability. A generated id changes across a rebuild or a rescheduled container, so one site's history fragments into many, and the column stops being a dimension anyone can group by. |
| Accept both sources under a precedence rule | Deployments can override a manifest default per machine, which is a real pattern. | Loses on legibility. The value that decides where files are written is then the answer to a question the operator has to reconstruct from two places. |

## Criteria

1. **Shape of the failure a wrong value produces.** Whether the mistake is visible in the data
   or silent in the file tree.
2. **Recoverability by reading.** Whether an operator can work out what happened after the
   fact from what was written.
3. **Stability of the identity over time.** Whether one site keeps one id across restarts and
   rebuilds.
4. **Cost of the refusal to a correct deployment.** How much configuration a working setup
   owes.

Criteria 1 and 2 together decide it: a silent collision that cannot be undone by reading is the
worst available outcome, and every answer that supplies a value rather than refusing one can
produce it. Criterion 3 rules out generation independently. Criterion 4 is the price, and it is
one binding.

## Consequences

Every deployment states its identity explicitly and states it once. Provenance on a row is
therefore a value somebody chose, and grouping by it is meaningful across the whole history of
a store. A row written without a site id reads null under schema union and renders as
unattributed rather than as a fabricated site — the absence is legible rather than filled in.

The cost accepted is a startup failure on a missing binding. A deployment that adds a machine
and forgets the variable does not run, which is loud at exactly the moment a new machine is
being brought up.

This rule establishes that a value exists, not that it is unique. Two sites configured with the
same id pass every check here and produce exactly the collision the default-value option was
rejected for. Nothing in the run path detects that, and the uniqueness of the id across a fleet
is unenforced.

## Revisit triggers

- A duplicate id across two sites is observed in a real deployment, which would argue for a
  registration or a claim on first write.
- The site id stops being interpolated into directory and filename paths, which removes the
  collision that makes a default unsafe.
- Fleet deployments grow a pattern where a manifest default plus a per-machine override is the
  common case, making the two-source refusal the dominant configuration cost.
