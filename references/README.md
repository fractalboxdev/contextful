# References

The literature and practice the **Contextful** design answers to. The specification
under `spec/` states behavior and never cites; this directory holds the sources
behind that behavior, and points into the spec by contract and operation name
(`store.lease`, `authority.attenuate`, `disclosure.release`). No file under `spec/`
links here, so a source can be added, re-ranked or retired without touching a clause.

## Entry shape

Each entry carries:

- **Citation** — authors, title, venue and year, with a DOI, arXiv, proceedings or
  publisher URL.
- **Priority** — `must-read` (the design takes a position the source settles or
  contests), `should-read` (the source sharpens a choice or supplies a baseline),
  `optional` (origin or vocabulary).
- **Informs** — the `<contract>.<operation>` names whose clauses the source bears on.
- **Question** — the one question the source answers for this design. Where the
  question is open, the entry says what the source would settle.

A source appears once, in the area where its main question lives; another area
names it by title.

## Contracts

| Contract | Covers |
| --- | --- |
| `corpus` | The grammar the specification obeys |
| `topology` | Parties, crossings, build profiles, coordination |
| `store` | Parts, manifests, catalog, fold; the bucket wire format, leases, replicas |
| `read` | Registration, guard, retrieval, ranking; synthesized memory |
| `run` | Journal, cursors, retry; pipelines; the derive tier |
| `connector` | Component world, packaging, built-in sources; outbound credentials |
| `authority` | Subject, delegation profile, admission, revocation; enforcement layers |
| `disclosure` | Mirrored visibility; aggregate release; audit chain, erasure, receipt |
| `surface` | Cadence, dispatch, control document; the analyst console |
| `assurance` | Formal model, proof gate, differential harness; build gates, evaluation |

## Areas

| File | Area | Main operations |
| --- | --- | --- |
| [storage-and-table-formats.md](./storage-and-table-formats.md) | ACID tables on object storage, bitemporal time, local-first replication | `store.fold`, `store.push`, `store.merge`, `store.probe`, `store.bound-time`, `surface.edit` |
| [leases-and-coordination.md](./leases-and-coordination.md) | Leases, fencing tokens, reconciliation loops | `store.lease`, `topology.coordinate`, `surface.dispatch`, `run.backfill`, `surface.reconcile` |
| [durable-execution.md](./durable-execution.md) | Journaled replay, exactly-once effects, watermarks, retry amplification | `run.journal`, `run.advance`, `run.retry`, `run.declare`, `connector.meter` |
| [access-control-and-capabilities.md](./access-control-and-capabilities.md) | Query-rewrite access control, ReBAC, attenuable tokens, sandboxed connectors | `authority.compose`, `authority.refuse`, `authority.attenuate`, `authority.verify`, `disclosure.bound-staleness`, `connector.import` |
| [privacy-and-disclosure.md](./privacy-and-disclosure.md) | Differential privacy, reconstruction, declassification, keyed pseudonyms | `disclosure.release`, `disclosure.suppress`, `authority.mask` |
| [audit-and-erasure.md](./audit-and-erasure.md) | Tamper-evident logs, erasure in append-only stores, provenance | `disclosure.record`, `disclosure.attest`, `disclosure.erase`, `disclosure.erase` |
| [retrieval-and-memory.md](./retrieval-and-memory.md) | Hybrid rank fusion, filtered ANN, agent memory, memory poisoning | `read.rank`, `read.retrieve`, `read.revise`, `read.recall`, `assurance.evaluate` |
| [prompt-injection.md](./prompt-injection.md) | Indirect prompt injection and defenses by construction | `connector.infer`, `authority.resist`, `surface.plan-turn`, `surface.ground` |
| [formal-methods.md](./formal-methods.md) | Model checking, verification-guided development, Rust verification | `assurance.prove`, `assurance.differential-test`, `assurance.scope-claim`, `store.lease`, `run.journal` |
| [specification-practice.md](./specification-practice.md) | Decision records, requirement syntax and quality, spec-to-test traceability | `corpus.rationale`, `corpus.address`, `corpus.anatomy`, `corpus.state`, `corpus.reference` |
| [specification-readability.md](./specification-readability.md) | Readable specifications with addressed, test-linked sentences; rendered views, guide layers, one-page cards | `corpus.render`, `corpus.anatomy`, `corpus.reference` |
