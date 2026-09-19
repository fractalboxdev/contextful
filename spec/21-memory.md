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

## declare

| Clause | Statement | Why |
| --- | --- | --- |
| `read.declare.memory-shape` | Memory is five table shapes: `memory_episodes` per session window, `memory_facts` per durable claim, `memory_entities` per resolved thing, `memory_edges` per directed relationship, `memory_preferences` per subject-key pair. | A-read |
| `read.declare.shape-declaration` | A table names `shape` beside its columns, binding the schema validator to the shape's canonical columns and the tool surface to its ranking defaults. | — |
| `read.declare.canonical-column` | A table naming a shape and omitting one of its canonical columns raises `MemoryShapeColumnMissing` when the declaration loads. | P1 |
| `read.declare.fact-columns` | `memory_facts` carries `fact_id`, `subject_id`, `predicate`, `object`, `confidence`, `scope`, `tier`, `support`, `evidence_row_ids`, `prompt_version`, `dedup_key`, `superseded_by` and the declared validity pair. | — |
| `read.declare.entity-columns` | `memory_entities` carries `entity_id`, a display name, per-language aliases, an attribute map and a last-seen instant. | — |
| `read.declare.edge-columns` | `memory_edges` carries `src_id`, `dst_id`, `rel_type`, `weight`, the validity pair and a claim row's provenance columns. | — |
| `read.declare.episode-columns` | `memory_episodes` carries an episode id, the window start and end, a summary, the source tables read and the evidence row identifiers folded in. | — |
| `read.declare.preference-columns` | `memory_preferences` carries `subject_id`, a key, a value, `confidence` and `scope`, one live row per subject-key pair under {{read.revise.supersession}} | — |
| `read.declare.validity-pair` | A memory shape names its validity columns through the ordinary table declaration, and recall and a validity-bounded read resolve that one pair. | P3 |
| `read.declare.tier` | `tier` is `curated` for human-written rows, `derived` for synthesis over a declared source's rows, and `researched` for synthesis over rows the system fetched on its own initiative. | A-read |
| `read.declare.scope` | `scope` is an optional string stamped at synthesis or assertion, partitioning one subject's claims by the surface that produced them. | — |
| `read.declare.relation-type` | The relation vocabulary is a reserved core — `same_as`, `part_of`, `member_of`, `located_in`, `derived_from`, `precedes`, `causes`, `mentions` — unioned with the deployment's declared types, closed within that deployment. | A-read |
| `read.declare.predicate-cardinality` | Each predicate and relation type carries cardinality `functional` or `multi`. The core declares `part_of` and `located_in` functional and the rest multi; an undeclared predicate is `functional`. | because supersession on a multi-valued predicate retires a claim that still holds |
| `read.declare.undeclared-relation` | A candidate edge whose `rel_type` falls outside the union lands in the dead-letter table and raises `MemoryUndeclaredRelation`; no row is written under the unknown type. | A-read |
| `read.declare.relation-flags` | A declared relation type carries optional inverse and symmetric flags, read by traversal as hints and by nothing that infers a row. | — |
| `read.declare.place-tables` | The place dimension, its per-language aliases, and the place-to-entity map are three tables the deployment supplies as CSV. | A-read |
| `read.declare.extract-template` | The deployment supplies the extract template and the output schema candidates validate against. | — |

unsettled: Which relation types belong in the reserved core, and what declared path adds or renames one without stranding rows? owner: memory affects: read.declare

## synthesize

| Clause | Statement | Why |
| --- | --- | --- |
| `read.synthesize.synthesis-stage` | Synthesis runs Extract, Resolve, Consolidate. Extract reads rows landed since the pass's cursor; Resolve is deterministic; Consolidate deduplicates and writes. | — |
| `read.synthesize.extract` | Extract is the one stage calling a model, at temperature 0, emitting claim tuples, entity mentions and candidate edges, each validated against the declared output schema. | A-read |
| `read.synthesize.extract-attempts` | A schema-invalid response is re-prompted with its validation feedback, at most 3 attempts per batch in total. | — |
| `read.synthesize.dead-letter` | A batch exhausting {{read.synthesize.extract-attempts}} writes the response, template hash and drop reason to the dead-letter table, raises `MemoryExtractExhausted`, and leaves the cursor unadvanced. | A-read |
| `read.synthesize.dedup-key` | `dedup_key` is sha256 over a length-prefixed encoding of the prompt hash, the sorted source row ids, the canonical subject, `predicate` and `object`. | because an unprefixed concatenation is ambiguous and a key without `object` collides two distinct claims |
| `read.synthesize.idempotence` | Consolidate upserts on `dedup_key`; repeating a pass over the same rows under the same template writes nothing new. | — |
| `read.synthesize.prompt-version` | A changed template yields a different key; both generations coexist, told apart by `prompt_version`, until a rewrite command retires the earlier under a caller-named drop-or-merge strategy. | — |
| `read.synthesize.template-hash` | The prompt hash covers the operator's template alone, excluding the data markers around ingested values. | — |
| `read.synthesize.support` | Every claim carries `support = {contributors, records}`, computed at write and kept out of `dedup_key`: `records` counts evidence rows, `contributors` distinct declared contributor keys, else distinct source tables, else 1. | A-read |
| `read.synthesize.support-band` | Under banded reporting `support` renders as `0`, `1-9`, `10-99` or `100+`. | — |
| `read.synthesize.contributor-key` | A fetched result's contributor key is its registrable domain. | A-read |
| `read.synthesize.tier-derivation` | `tier` is computed at write from the writing grant and the run's source, never from the payload. A pass mixing declared-source rows with a self-directed fetch takes the lowest tier. | A-read |
| `read.synthesize.audit-record` | Each stage emits the run's spans plus endpoint, model id, template hash, token counts, candidate drop reason, entity action and claim action. | — |
| `read.synthesize.lineage` | Every memory row resolves to its pass, its model and its source rows through columns it carries. | — |
| `read.synthesize.commit` | A memory write runs the write path's post-run commit, surviving a restart and restored by a cold-start pull. | — |
| `read.synthesize.coverage-gap` | A coverage gap on a subject closes when a declared source covers it; an answer from a self-directed fetch leaves it open. | A-read |
| `read.synthesize.gap-record` | An open gap records the subject, the gap kind, the observing pass and the instant last observed open. | — |

unsettled: What sets the synthesis cadence per shape, and does a shape default give way to a deployment override? owner: memory affects: read.synthesize

unsettled: How is a model-emitted confidence rescaled into a comparable number, and what held-out set validates the rescaling? owner: memory affects: read.synthesize

## revise

| Clause | Statement | Why |
| --- | --- | --- |
| `read.revise.supersession` | A claim on a `functional` predicate retires the prior live claim on its `(subject_id, predicate, scope)` line, setting `superseded_by` to the landing `fact_id` and validity end to its start. Retired rows stay for audit. | A-read |
| `read.revise.multi-valued` | A claim landing on a `multi` predicate retires only a prior live claim on the same `(subject_id, predicate, object, scope)`. | A-read |
| `read.revise.confidence-decay` | Retirement multiplies the retired row's `confidence` by 0.5 when the object changed; a reaffirmation leaves it untouched. | — |
| `read.revise.validity-line` | Two claims revise each other when both carry an open validity end or both anchor to the same validity start. A past-anchored and a present claim coexist. | A-read |
| `read.revise.replay-skips-revision` | A candidate under an existing `dedup_key` resolves as an upsert and never reaches the revision rule. | — |
| `read.revise.batch-collision` | Claims on one line landing in one batch revise in order of the table's `order_by`, then `fact_id`, leaving one live claim. | because a batch keeping two live beliefs contradicts one live belief per line |
| `read.revise.tier-precedence` | A claim retires a prior of equal or higher standing. A lower-standing claim contradicting a live higher-standing one lands with a bounded validity end, never live. | A-read |
| `read.revise.direct-write` | The direct write accepts claims alone. Naming `memory_episodes`, `memory_entities`, `memory_edges` or `memory_preferences` raises `MemoryDirectWriteShapeRefused`; an entity row enters through {{read.resolve-entity.entity-upsert}} | A-read |
| `read.revise.anchor` | An `observed_at` anchor sets a claim's validity start and end to that instant while the ingestion clock stamps the present. The anchor stays outside the identity hash. | — |
| `read.revise.assertion-provenance` | A direct write records its stage as a direct assertion and stamps the author's subject tuple. | — |
| `read.revise.assertion-citation` | A direct assertion's textual citations follow the claim table's read rules and resolve as no evidence reference. | — |
| `read.revise.expiry` | A `researched` claim carries an expiry. A scheduled pass stamps a validity end on an expired claim no promotion covers, leaving the row in place. | A-read |
| `read.revise.retention` | An expired `researched` claim survives while an artifact's provenance cites it or a human promotes it. A `curated` or `derived` claim carries no expiry. | A-read |
| `read.revise.promotion` | Promotion to a higher tier patches the record and appends a marker pass; the synthesizing agent stays recorded and the promoting human is stamped beside it. | A-read |
| `read.revise.edge-revision` | An edge follows the claim revision rule over `(src_id, rel_type, dst_id, scope)`, with the same validity-line scoping and retirement stamps. | — |

unsettled: How are two unscoped writers colliding on one subject, predicate and scope surfaced to a human, rather than the later one landing not live? owner: memory affects: read.revise

unsettled: What decay half-life applies to a claim nothing reinforces, and is fading or hard retention the default? owner: memory affects: read.revise

## recall

| Clause | Statement | Why |
| --- | --- | --- |
| `read.recall.present-time` | Recall answers as of now and takes no time bound; an earlier belief is read by a bounded read over the claim mirror. | — |
| `read.recall.recall-clocks` | The session's vantage bounds when a row entered the store; the validity columns bound what the row claims holds true. | A-read |
| `read.recall.open-interval` | A present-time read returns open-ended intervals; a read at a one-day vantage adds the point intervals anchored to that day. | — |
| `read.recall.fold` | Recall keeps the latest row per `dedup_key` and drops superseded rows before ranking. | — |
| `read.recall.tombstone-filter` | Every read of a memory table — recall, knowledge card, and a read bounded by `as_of` or `pin` — drops a tombstoned row and its cascade-marked rows before ranking, whether or not the files still hold them. | because a tombstoned row stays in files until compaction and retention elapse, and time travel otherwise reaches it |
| `read.recall.tier-order` | Recall orders by `tier` first, score second. | A-read |
| `read.recall.score` | Within one tier the score is `confidence × (index + 1) / total`, where `index` is the claim's position in the live set ordered by validity start ascending. | — |
| `read.recall.scope-filter` | A recall with a scope filter returns claims stamped with that exact scope and no unscoped claim; one with no filter returns every scope. | — |
| `read.recall.evidence-gate` | A synthesized claim is served when every evidence row resolves and reads through the caller's own enforced session. | A-read |
| `read.recall.evidence` | An evidence reference names a table with a declared single-column primary key or an `id` column, and the row identifier within it. | — |
| `read.recall.evidence-unresolved` | An unreadable or masked source row, an unknown table, a reference into another memory row, or malformed lineage suppresses the claim and raises `MemoryEvidenceUnresolved`. | A-read |
| `read.recall.evidence-references` | A claim naming more than 256 entries of evidence is suppressed unresolved, raising `MemoryEvidenceOverflow`. | A-read |
| `read.recall.zone-check` | Recall checks each row's inference zone against the caller's asserted zone by the table-read rule. | P5 |
| `read.recall.only-door` | Memory tables are excluded from read-path candidate generation; a claim reaches an answer through recall alone. | P5 |
| `read.recall.empty-store` | A store holding no claim table recalls a clean zero as an answered read. | P2 |
| `read.recall.injection` | Recalled claims enter a grounded turn as an explicit block asking the turn to confirm or contradict each, as a visible trace step. | — |
| `read.recall.knowledge-card-link` | A recall result carries each claim's `fact_id`, `tier`, `support` and evidence identifiers. | — |

unsettled: What supplies a read-side usage ledger, so retention can ask whether a claim was ever recalled rather than whether something cited it? owner: memory affects: read.recall

## resolve-entity

| Clause | Statement | Why |
| --- | --- | --- |
| `read.resolve-entity.resolve` | Resolve matches mentions against `memory_entities` by alias, embedding similarity and deterministic keys, settling on a canonical `entity_id` or staging one, then tags each candidate claim with its subject. | — |
| `read.resolve-entity.entity-id` | `entity_id` is opaque, carrying no name, ticker or language; a display change rewrites no identifier and no row pointing at one. | — |
| `read.resolve-entity.alias-normalization` | Per-language aliases normalize script variants and full-width forms onto one identity. The engine owns the matcher; the deployment owns the alias sets. | — |
| `read.resolve-entity.alias-match-mode` | An ideographic alias matches by containment; a latin or numeric alias matches on ASCII word boundaries at both ends. | — |
| `read.resolve-entity.alias-stem` | A trailing `*` marks a prefix stem, dropping the right boundary; `\*` is a literal asterisk. A stem on an ideographic alias changes nothing and widens tagging alone. | — |
| `read.resolve-entity.ambiguous-mention` | A mention matching two canonical identities with no deterministic key separating them is recorded as an ambiguous skip, raises `MemoryEntityAmbiguous`, and dead-letters its candidate claim. | A-read |
| `read.resolve-entity.knowledge-card` | `entity.explain(reference)` resolves a ticker, name or alias in any script to one authorized identity, returning its id, display name, aliases, attributes, last-seen instant and authorized claims. | — |
| `read.resolve-entity.card-authorization` | The card requires a read grant on the entity table; without a separate claims grant it returns identity and no claims. | A-read |
| `read.resolve-entity.entity-upsert` | The entity upsert writes one entity row and is reachable from the command line. | — |
| `read.resolve-entity.place-id` | A row's place is an opaque `place_id` stamped by the matcher that stamps `entity_id`. | A-read |
| `read.resolve-entity.parent-closure` | Nearness is the materialized transitive closure over declared parent edges: near a place means at it or under it. | A-read |
| `read.resolve-entity.no-geometry` | The engine holds no geometry type, distance function or radius; a spatial question is answered by the parent closure or not at all. | A-read |
| `read.resolve-entity.place-left-join` | A row joins the latest place alias row per key by left join; an unmapped place lands a null tag and the row survives. | — |
| `read.resolve-entity.edge-materialization` | An edge is an explicit typed row, written when synthesis resolves both endpoints and carrying a claim's lineage and validity; no edge is inferred at query time. | — |
| `read.resolve-entity.edge-endpoint` | A candidate edge whose source or target resolves to no identity is dead-lettered, raising `MemoryEdgeEndpointUnresolved`. | A-read |
| `read.resolve-entity.traversal` | Multi-hop traversal is a recursive query over `memory_edges`, seeded by resolution and ranked recall, in the read path's own query language. | A-read |
| `read.resolve-entity.ownership-answer` | An ownership question resolves to an artifact and the person attached to it — a code-owners entry, a directory's commit history, an issue's routing team, a page's owner field. | — |
| `read.resolve-entity.person-artifact` | An ownership answer carries the artifact identifier, the field the attachment was read from, and the attached principal. | — |

unsettled: At what hop depth or edge count does an external graph engine become a served backend rather than a derived index? owner: memory affects: read.resolve-entity

unsettled: Does an ownership answer over an artifact with several attached principals return all of them, or the most recent attachment? owner: memory affects: read.resolve-entity

## settle

| Clause | Statement | Why |
| --- | --- | --- |
| `read.settle.prediction` | `predictions` holds one row per claim about the future: subject, target, resolution form, resolution key, resolution source, `predicted_at`, `deadline_at` and confidence. A calendar-day deadline canonicalizes to midnight UTC. | — |
| `read.settle.observation` | `outcomes` holds one row per observed result: the prediction it settles, verdict, signal, resolution source, settling citation and `observed_at`. | — |
| `read.settle.registration` | A registration names exactly one form (relative horizon, absolute deadline, open watch) and one source (`metric`, `adjudicator`, `manual`), with a comparator exactly when the source is `metric`; otherwise it raises `OutcomeRegistrationInvalid`. | A-read |
| `read.settle.prediction-id` | A prediction id hashes `(subject, target, resolution key, predicted_at)`, `predicted_at` defaulting to now; an explicitly anchored re-emit collapses onto the first row. | — |
| `read.settle.observation-id` | An observation id hashes every field it asserts; a byte-identical re-observation writes nothing, and two differing settlements of one prediction both land. | — |
| `read.settle.comparator-custody` | A metric comparator is stored verbatim and evaluated outside the engine, which prescribes no rating schema and no observation cadence. | A-read |
| `read.settle.source-mismatch` | An observation carrying a verdict whose resolution source is absent or differs from the registration's raises `OutcomeSourceMismatch`. | A-read |
| `read.settle.settling-citation` | An `adjudicator` or `manual` verdict without an `http` or `https` settling citation raises `OutcomeCitationMissing`; the citation rides the label view. | A-read |
| `read.settle.null-verdict` | A verdict is held null under an undeclared-rule signal when the prediction declared no source, a self-rated signal when derived from the store's own rating, and an `unresolved:<reason>` signal the application supplies. | A-read |
| `read.settle.outcome-label` | `outcome_labels` joins `predictions` to `outcomes` on prediction id, compares instants as `epoch(CAST(x AS TIMESTAMPTZ))`, derives lead time in seconds, and excludes self-rated rows from every calibration figure. | A-read |
| `read.settle.grace-window` | The label join keeps an observation from the prediction instant through the deadline plus an inclusive grace of 86400 s. | — |
| `read.settle.view-partition` | `outcome_labels_unresolved` negates the scored predicate verbatim, every clause null-guarded, and a column a store lacks projects as null; every row lands in exactly one view. | A-read |

unsettled: At what grain are calibration and confidence reported to a consumer, and which figures derive from the scored view? owner: memory affects: read.settle

## Shapes

A declaration naming a shape and its predicate cardinality:

```toml
[[table]]
name  = "team_memory"
shape = "memory_facts"

[relation]
declared   = ["reports_to", "owns_service"]
functional = ["reports_to"]
```

The scored label view:

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
