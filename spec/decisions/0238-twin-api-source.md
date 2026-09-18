# 0238 — An organization-twin API is refused as an ingestion path for every source

**Status:** accepted 2026-09-18
**Decides:** `enforcement.resist.refusal.twin-api-as-a-source`

## Context

Every major workspace vendor offers an administrative export interface — security APIs,
eDiscovery endpoints, legal-hold exports. They exist for a specific purpose: a lawfully
compelled investigation, in which a designated party reads content that no ordinary
employee, including the workspace administrator in their daily role, is permitted to read.
The licence that grants that access is the whole point of the interface.

Such an interface is also the easiest way to ingest a workspace completely. One connection,
one credential, every message and every file, with no per-user consent to collect and no
per-user connection to maintain. The coverage is exactly what a context engine over an
organization wants.

The property at stake is the system's ceiling. Mirrored visibility and the compiled
semi-join constrain what each reader gets out, and they do that faithfully — a reader
receives what the source's own access lists grant them. But the store's contents are the
union of what was ingested, and ingesting through a twin API sets that union above what any
human in the organization can see. The restriction at query time bounds each reader; it
does not lower the ceiling, and nothing downstream does. A store built this way is an
organization-wide surveillance corpus whose safety rests entirely on the query-time layers
never failing and never being bypassed by an operator holding a deployment-wide credential.

There is a neighbouring shape that is not this. Some vendors sell an organization-wide
export path that the deploying organization pays for and discloses, whose reach is what the
organization has told its people it is. The distinguishing fact is the licence the interface
carries and the ceiling it produces, not the vendor or the transport.

## Decision

An API licensed to see what no individual user sees — security, eDiscovery, legal-hold
export — raises `EnforceTwinApiSource` for every source. A sanctioned, paid, disclosed
organization-wide export path is admitted; the line is the licence and the resulting
ceiling. The refusal is unconditional across sources rather than negotiated per connector.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the twin API as a source everywhere; admit the sanctioned disclosed export path** *(chosen)* | The store's ceiling stays at or below what people in the organization can see, so query-time enforcement bounds harm rather than being the only thing preventing it. | A deployment wanting complete coverage of a workspace ingests per user under per-user authority, which costs more connections and leaves genuinely private content out. |
| Ingest through the twin API and restrict at query time | Complete coverage with one connection, and each reader still receives only what the source grants them. | Loses on the ceiling: the property at stake is what the store contains, not what a given query returns, and no query-time restriction lowers it. An operator credential, a bypassed view or a future surface reaches the whole corpus. |
| Permit it where the deploying organization signs a disclosure | The organization's own governance decides, which is where such decisions usually sit. | Loses on consent from the people whose content is ingested: a disclosure signed by the organization is not consent from the people whose private content is ingested, and the sanctioned export path already covers the case where reach and disclosure genuinely match. |
| Decide per vendor, refusing the interfaces whose terms are most restrictive | Tracks the actual licence terms, which differ. | Loses on the line being drawn in the wrong place: the criterion is the licence and the ceiling it produces, and a per-vendor list encodes that criterion badly and goes stale as terms change. |
| Ingest through the twin API and drop content above the ceiling before storage | Coverage where it is legitimate, no ceiling lift. | Loses on decidability: computing what every individual can see, per resource, ahead of storage is the mirrored-visibility problem solved completely and in advance, which is the thing the sweep does approximately and after the fact. |

## Criteria

1. **The system's ceiling relative to every human's** — whether the store holds content no
   person in the organization is permitted to read. *This criterion decides.* Every other
   option keeps the coverage and argues that a later layer bounds the consequence, and
   bounding a consequence is a different claim from not holding the material; a ceiling
   lifted at ingestion cannot be lowered downstream.
2. **Consent from the people whose content is ingested** — whether the party agreeing is the
   party affected.
3. **Durability of the rule** — whether it survives vendor terms changing.
4. **Coverage of the resulting store** — the criterion the chosen option loses on.

## Consequences

A store's contents stay inside the union of what the organization's people can see, so the
query-time layers bound harm within a corpus that is already bounded, rather than being the
sole barrier between a corpus and its readers.

The cost accepted is coverage and operational weight. A deployment ingests per user under
per-user authority, which means one connection per user, per-user consent and per-user
revocation, and a store that omits genuinely private content — direct messages, private
channels, unshared files. Answers over that store are answers over less.

The rule is also unconditional, which means it refuses deployments whose intent is
legitimate and whose governance is sound. The sanctioned disclosed export path is the route
for those, and whether that path exists is a vendor's decision rather than this system's.

Reversing is expensive in a specific way: a store built once through a twin API stays built.
Lowering a ceiling after the fact means deleting ingested content and everything derived
from it, across replicas.

## Revisit triggers

- A vendor offers an export interface whose licence is explicitly bounded by what the
  organization's people can see, which would be the sanctioned path rather than a twin API
  and would need no exception.
- Per-user ingestion cost at organization scale is measured and lands high enough that
  deployments route around the engine to an unrestricted copy, which is the outcome this
  record's cost is meant to avoid.
- A pre-storage filter becomes decidable — a complete, current, per-resource reachable set
  computable ahead of ingestion — which would make the drop-above-the-ceiling option real.
