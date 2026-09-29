---
contract: read
owns:
  - declare
  - synthesize
  - revise
  - recall
  - resolve-entity
  - settle
---

# Synthesized memory

Memory is what the workspace concludes from what it ingested: durable claims, the things they
are about, the relationships between them, and whether a claim it emitted turned out to hold.
It lives on the same tables, commit and enforcement as ingested data, and is reached through
its own doors. Erasure of a memory subject is the disclosure contract's `erase` operation.

Memory's tables and what moves rows between them; each edge names the operation, each cylinder a table:

```mermaid
flowchart LR
  ROWS[("landed rows")]
  SYN["synthesis pass"]
  subgraph STOREC["memory tables"]
    FACTS[("memory_facts")]
    ENT[("memory_entities")]
    EDGES[("memory_edges")]
    DL[("dead-letter table")]
    PRED[("predictions")]
    OUT[("outcomes")]
    LAB[("outcome_labels")]
  end
  ERASER["erasure cascade"]
  FACE["read face"]
  APP(["application"])
  ROWS -->|"extract since cursor"| SYN
  SYN -->|"resolve mentions"| ENT
  SYN -->|"relate entities"| EDGES
  SYN -->|"consolidate, supersede"| FACTS
  SYN -->|"failed batch"| DL
  APP -->|"write claims directly"| FACTS
  FACTS -->|"recall with evidence"| FACE
  FACE -->|"grounded turn"| APP
  APP -->|"register prediction"| PRED
  APP -->|"observe outcome"| OUT
  PRED & OUT -->|"settle by join"| LAB
  ERASER -->|"tombstone subject"| FACTS
```

## declare

The five memory table shapes, their canonical columns, tier and scope, and the relation vocabulary with its cardinality.

- `canonical-column` — A table naming a shape and omitting one of its canonical columns raises `MemoryShapeColumnMissing` when the declaration loads.
  *P1*
- `undeclared-relation` — A candidate edge whose `rel_type` falls outside the union lands in the dead-letter table and raises `MemoryUndeclaredRelation`; no row is written under the unknown type.
  *A-read*

unsettled: Which relation types belong in the reserved core, and what declared path adds or renames one without stranding rows? owner: memory affects: read.declare

## synthesize

Extract, Resolve and Consolidate; the dedup key, evidence support, the audit record and coverage gaps.

- `extract-attempts` — A schema-invalid response is re-prompted with its validation feedback, at most 3 attempts per batch in total.
- `dead-letter` — A batch exhausting {{read.synthesize.extract-attempts}} writes the response, template hash and drop reason to the dead-letter table, raises `MemoryExtractExhausted`, and leaves the cursor unadvanced.
  *A-read*
- `pass-cursor` — A pass reads the source's committed rows its cursor has not recorded, through the writing credential's own session, in batches under {{read.synthesize.prompt-bound}}, recording each batch once its claims commit.
- `prompt-bound` — A batch packs fenced rows up to 32 KiB; a fenced row over that bound travels in a batch of its own.
  *because one oversized row neither stalls the pass nor drags its neighbours past the bound*
- `row-cap` — A row is fenced at 16384 chars under {{connector.infer.value-cap}}; a truncated row still reaches the model, its batch commits and advances the cursor, and the pass report counts it in `truncated`.
  *because a silent cut hides that a conclusion rests on a partial row*
- `attribution` — Every landed claim carries `grant_id`, the chain-final revocation identifier of the credential that wrote it, `agent`, that credential's agent member, and `_authored_by`, its on-behalf-of principal.
  *because a reader weighs a conclusion by which grant could have produced it*
- `claim-taint` — A source row carries its own `_taint`, else `ingested:third-party`. Every row a batch commits lands under {{store.reserve.taint}} with the least-trusted label among the batch's rows.
  *because a row no label vouches for is trusted least, and a claim re-synthesized from claims stays as low as its origin*

One synthesis batch, from landed rows to a committed claim or the dead-letter table:

```mermaid
flowchart LR
  ROWS[("landed rows")] -->|"batch since cursor"| MODEL(["extraction model"])
  MODEL -->|"response"| VAL{"output valid?"}
  VAL -->|"no, attempts left"| MODEL
  VAL -->|"no: MemoryExtractExhausted"| DL[("dead-letter table")]
  VAL -->|"yes"| RES{"mentions resolved?"}
  RES -->|"ambiguous: MemoryEntityAmbiguous"| DL
  RES -->|"endpoint missing: MemoryEdgeEndpointUnresolved"| DL
  RES -->|"yes"| REL{"relation type declared?"}
  REL -->|"no: MemoryUndeclaredRelation"| DL
  REL -->|"yes, consolidate and commit"| FACTS[("memory_facts")]
```

unsettled: What sets the synthesis cadence per shape, and does a shape default give way to a deployment override? owner: memory affects: read.synthesize

unsettled: How is a model-emitted confidence rescaled into a comparable number, and what held-out set validates the rescaling? owner: memory affects: read.synthesize

## revise

Supersession within one validity line, confidence decay, the direct write and its anchor, expiry, retention, promotion.

- `tier` — `tier` is stamped from the write path and grounding, never the payload: a synthesis pass stamps `derived`, a direct write `curated`, a fetched result `researched`, and mixed grounding takes the lowest.
  *A-read*
- `supersede` — A claim of equal or higher tier retires a live prior of its subject, predicate and scope when both are open-ended or share a valid-from instant: the prior's `valid_to` becomes the claim's `valid_from`, and `superseded_by` names it.
  *A-read*
- `direct-write` — The direct write accepts claims alone. Naming `memory_episodes`, `memory_entities`, `memory_edges` or `memory_preferences` raises `MemoryDirectWriteShapeRefused`; an entity row enters through the entity upsert.
  *A-read*

unsettled: How are two unscoped writers colliding on one subject, predicate and scope surfaced to a human, rather than the later one landing not live? owner: memory affects: read.revise

unsettled: What decay half-life applies to a claim nothing reinforces, and is fading or hard retention the default? owner: memory affects: read.revise

## recall

Serving live memory at present time: the two clocks, tier-first ordering, the scope filter, the tombstone filter, the evidence gate.

- `evidence-unresolved` — An unreadable or masked source row, an unknown table, a reference into another memory row, or malformed lineage suppresses the claim and raises `MemoryEvidenceUnresolved`.
  *A-read*
- `evidence-references` — A claim naming more than 256 entries of evidence is suppressed unresolved, raising `MemoryEvidenceOverflow`.
  *A-read*
- `ranked-arm` — A `corpus.retrieve` arm over a `memory_facts` table serves only live claims — no `superseded_by`, and a `valid_to` null or past the read's anchor — whose evidence passes the gate.
- `suppression-count` — A suppressed claim is absent from the rows; the `contextful.recall` block counts suppressions per error identifier and names no claim.
  *because a count discloses that a conclusion was withheld, never what it concluded*

The evidence check a claim passes on its way to a grounded turn:

```mermaid
flowchart LR
  FACTS[("memory_facts")] -->|"live claim"| EV{"evidence readable in session?"}
  EV -->|"yes, grounded turn"| APP(["application"])
  EV -->|"overflow: MemoryEvidenceOverflow"| HELD["withheld claim"]
  EV -->|"unreadable: MemoryEvidenceUnresolved"| HELD
```

unsettled: What supplies a read-side usage ledger, so retention can ask whether a claim was ever recalled rather than whether something cited it? owner: memory affects: read.recall

## resolve-entity

Mention-to-entity matching, aliases, the knowledge card, place identity and nearness, edges, and ownership answers.

- `ambiguous-mention` — A mention matching two canonical identities with no deterministic key separating them is recorded as an ambiguous skip, raises `MemoryEntityAmbiguous`, and dead-letters its candidate claim.
  *A-read*
- `edge-endpoint` — A candidate edge whose source or target resolves to no identity is dead-lettered, raising `MemoryEdgeEndpointUnresolved`.
  *A-read*

unsettled: At what hop depth or edge count does an external graph engine become a served backend rather than a derived index? owner: memory affects: read.resolve-entity

unsettled: Does an ownership answer over an artifact with several attached principals return all of them, or the most recent attachment? owner: memory affects: read.resolve-entity

## settle

Predictions, observations, the registration, the citation a verdict owes, and the label views.

- `registration` — A registration names exactly one form (relative horizon, absolute deadline, open watch) and one source (`metric`, `adjudicator`, `manual`), with a comparator exactly when the source is `metric`; otherwise it raises `OutcomeRegistrationInvalid`.
  *A-read*
- `source-mismatch` — An observation carrying a verdict whose resolution source is absent or differs from the registration's raises `OutcomeSourceMismatch`.
  *A-read*
- `settling-citation` — An `adjudicator` or `manual` verdict without an `http` or `https` settling citation raises `OutcomeCitationMissing`; the citation rides the label view.
  *A-read*
- `grace-window` — The label join keeps an observation from the prediction instant through the deadline plus an inclusive grace of 86400 s.

Registration, observation and the label view:

```mermaid
flowchart LR
  APP(["application"]) -->|"registration"| RV{"registration well formed?"}
  RV -->|"yes"| PRED[("predictions")]
  RV -->|"no: OutcomeRegistrationInvalid"| APP
  SRC(["resolution source"]) -->|"observation"| OV{"source and citation match?"}
  OV -->|"yes"| OUT[("outcomes")]
  OV -->|"source differs: OutcomeSourceMismatch"| SRC
  OV -->|"no citation: OutcomeCitationMissing"| SRC
  PRED -->|"joined within grace"| LAB[("outcome_labels")]
  OUT -->|"joined within grace"| LAB
```

unsettled: At what grain are calibration and confidence reported to a consumer, and which figures derive from the scored view? owner: memory affects: read.settle

## Shapes

A declaration naming a shape and its declared relation types:

```toml
[[table]]
name  = "team_memory"
shape = "memory_facts"

[relation]
declared = ["reports_to", "owns_service"]
```

The label view:

```sql
CREATE VIEW outcome_labels AS
SELECT p.prediction_id, p.subject, p.resolution_source,
       o.verdict, o.signal, o.settling_citation,
       epoch(CAST(o.observed_at AS TIMESTAMPTZ)) - epoch(CAST(p.predicted_at AS TIMESTAMPTZ)) AS lead_time_s
FROM predictions p
JOIN outcomes o ON o.prediction_id = p.prediction_id
WHERE o.observed_at IS NOT NULL
  AND o.signal IS DISTINCT FROM 'self-rated'
  AND epoch(CAST(o.observed_at AS TIMESTAMPTZ)) >= epoch(CAST(p.predicted_at AS TIMESTAMPTZ))
  AND epoch(CAST(o.observed_at AS TIMESTAMPTZ)) <= epoch(CAST(p.deadline_at AS TIMESTAMPTZ)) + 86400;
```
