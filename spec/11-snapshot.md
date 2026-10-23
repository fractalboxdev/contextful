---
contract: store
owns:
  - snapshot
---

# Snapshot corpora

A snapshot corpus stores copies of external documents for local ranking under a reading horizon.
Implementers use this contract to record captures and bind evaluation cases to a stored snapshot.
Pipelines store the documents through the ordinary write path. Reads follow {{read.horizon.local-rank}} without querying the live web.

## snapshot

Document capture records, snapshot identifiers for evaluation cases and access to documents captured after the reading horizon.

- `capture` — `[pipeline.tables.snapshot]` names each document's `url`, `captured_at`, `digest` and `source` columns. Each capture commits as one store snapshot. An evaluation case pins the capture by `snapshot` id.
  *because a corpus re-captured between two runs of one case answers each run from different pages*
- `post-horizon-capture` — A document captured after the reading horizon carries `_available_reconstructed` true under {{store.available-time.live-since}}. A horizon read admits the document only when the block's `leak_score` column holds 0.
  *A-store*

#### Scenarios

- `store.snapshot.post-horizon-capture`: WHEN a pipeline captures a page published `2014-03-02` on `2019-06-10` and a reader requests horizon `2014-05-01`, THEN the row carries `_available_reconstructed` true and appears only while its `leak_score` is 0.
