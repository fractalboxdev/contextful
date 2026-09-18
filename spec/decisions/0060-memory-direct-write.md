# 0060 — The direct write asserts claims and names the entity upsert as the door for an identity row

**Status:** accepted 2026-09-18
**Decides:** `memory.revise.interface.direct-write`, `memory.revise.refusal.direct-write-shape`

## Context

Memory is five table shapes over one substrate, and each has its own identity and its own
supersession behavior. A claim is keyed on a subject, a predicate and a scope, and revision
retires the prior live claim on that triple. An entity is a resolved identity whose
`entity_id` is opaque and whose display name changes without rewriting a row that points at
it. An edge follows the claim rule over a source, a type, a target and a scope. An episode
covers a window. A preference holds one live row per subject-key pair.

Most of these already have a door. Resolve settles mentions onto canonical identities and a
companion upsert writes one entity row, reachable from the command line so a pipeline seeds
identities without opening a tool session against the engine it runs inside. Edges are
written when synthesis resolves both endpoints. Episodes are produced by a pass over a
window.

What has no door is the assertion a caller makes directly: a console turn that distils a
conclusion, or a human stating a fact, arriving as a claim with no synthesis pass behind
it.

A direct write can also be about a past moment, carrying an `observed_at` anchor that sets
the claim's validity start and end to that instant while the ingestion clock stamps the
present.

A general write that accepted any of the five shapes would carry five identity rules, five
validation paths and five supersession behaviors behind one verb.

## Decision

The direct write accepts claims. A write naming `memory_episodes`, `memory_entities`,
`memory_edges` or `memory_preferences` raises `MemoryDirectWriteShapeRefused`, and the
refusal names the entity upsert as the door for an entity row. An `observed_at` anchor sets
the claim's validity start and end to that instant, a point interval, while the ingestion
clock stamps the present, and the anchor stays outside the deduplication key. A direct
write records its stage as a direct assertion and stamps the author's subject tuple, so an
asserted claim and a synthesized claim are told apart in the audit record.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **One shape per write surface, with a named door for each other shape** *(chosen)* | One identity rule and one revision rule behind one verb; a refusal that tells the caller where to go instead. | A caller writing claims and identities makes two calls rather than one. |
| A general write accepting any memory shape | One verb covers everything; a caller learns one call. | Loses on the revision rule: episodes, entities and edges each need different identity and supersession handling, so one verb hides five behaviors and a caller cannot predict which one ran. |
| Accept every shape and refuse only on a malformed payload | Maximum reach; the validator does the discriminating. | Lost on one rule per surface: the validator would choose an identity rule from the payload's shape, so one verb still carries five supersession behaviors and the choice is made implicitly rather than named by the caller. |
| Let the anchor enter the deduplication key | Two anchored assertions of one claim stay distinguishable with no further rule. | Loses on collision: an otherwise identical claim re-asserted at two vantages would be two rows by accident of the vantage rather than by anything the caller asserted. |
| Refuse silently, or refuse without naming a door | Smaller surface; no coupling between the write verb and the upsert. | Loses on recoverability of the caller: a refusal that names no alternative leaves the caller guessing at a door that exists. |

## Criteria

1. **One rule per surface** — whether a caller of one verb can predict the identity and
   supersession behavior that will run. *This criterion decided it.* Revision is the part
   of memory that is hardest to reason about after the fact, and a verb whose revision
   behavior depends on which shape the payload named makes every audit start by
   reconstructing which branch ran.
2. **Identity stability** — whether the deduplication key stays a statement about content
   rather than about when a caller happened to assert it. This kept the anchor out of the
   key.
3. **Caller recoverability** — whether a refused caller learns where to go. This made the
   refusal name the entity upsert rather than simply rejecting.
4. **Call count for a common workflow** — how many calls a caller writing both claims and
   identities makes. The chosen option scores worst; this is the cost accepted.

## Consequences

A caller that writes both claims and identities makes two calls against two surfaces. That
is the accepted cost, and it is paid by exactly the workflow — a console turn distilling
conclusions about entities it also wants to seed — that is most likely to hit it.

A caller asserting the same claim across snapshots either varies the claim's evidence
qualification by hand to keep two anchored assertions apart, or collapses the assertions
onto one row. The anchor being outside the key means the vantage alone does not separate
them.

Direct assertions carry their own provenance stage and the author's subject tuple, so an
asserted claim and a synthesized claim are distinguishable in the audit record without
inspecting the lineage columns. Textual citations in a direct assertion are governed by the
claim table's own read rules and are not resolved as evidence row references, so an
assertion does not pass through the evidence gate on the strength of prose.

Adding a sixth memory shape later means deciding its door explicitly rather than inheriting
one, which is more work per shape and is the point.

## Revisit triggers

- A caller workflow appears that writes claims and identities together often enough that
  the two-call cost dominates, which argues for a compound door rather than a general one.
- A second shape acquires a revision rule identical to the claim rule, at which point one
  surface could cover both without hiding a branch.
- Callers are observed asserting one claim at many vantages and hand-varying the evidence
  qualification to keep the rows apart, which says the anchor belongs somewhere in identity
  after all.
