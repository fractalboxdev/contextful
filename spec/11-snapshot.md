---
contract: store
owns:
  - snapshot
---

# Snapshot corpora

A snapshot corpus stores copies of external documents, landed through the ordinary write path, for local ranking under {{read.horizon.local-rank}}.

## snapshot

Document capture records, snapshot identifiers for evaluation cases and access to documents captured after the reading horizon.

- `capture` — `[pipeline.tables.snapshot]` declares {{type:contextful_core::store::declare::SnapshotDecl}}, the columns describing each captured document. Each capture commits as one store snapshot, and an evaluation case pins it by `snapshot` id.
  *A-store#snapshot-corpus-declared*
- `post-horizon-capture` — A document captured after the reading horizon carries `_available_reconstructed` true under {{store.available-time.live-since}}, and a horizon read admits it only while the block's `leak_score` column holds 0.
  *A-store#snapshot-corpus-declared*

#### Scenarios

- `store.snapshot.post-horizon-capture`: WHEN a pipeline captures a page published `2014-03-02` on `2019-06-10` and a reader requests horizon `2014-05-01`, THEN the row carries `_available_reconstructed` true and appears only while its `leak_score` is 0.
