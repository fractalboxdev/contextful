---
contract: store
owns:
  - snapshot
---

# Snapshot corpora

A snapshot corpus is a stored copy of outside documents, captured before a reading horizon
and ranked locally, so a point-in-time read never asks the live web what it held. The corpus
lands through the ordinary write path and is read under {{read.horizon.local-rank}}.

## snapshot

A snapshot corpus: the per-document capture record, the snapshot an evaluation case pins, and the standing of a document captured after the horizon.

- `capture` — A snapshot corpus table records per document its URL, capture instant, content digest and capture source; each capture commits as one store snapshot, and an evaluation case pins one snapshot id.
  *because a corpus re-captured between two runs of one case answers each run from different pages*
- `post-horizon-capture` — A document captured after the reading horizon carries `_available_reconstructed` true, as {{store.available-time.live-since}} injects it, and a horizon read admits it only below the case's declared leak threshold.
  *because a later capture shows the page as updated, not as published*

#### Scenarios

- `store.snapshot.post-horizon-capture`: WHEN a page published `2014-03-02` is captured `2019-06-10` and read under horizon `2014-05-01`, THEN the row carries `_available_reconstructed` true and answers only below the case's leak threshold.

unsettled: Does a document's leak score land as a platform-computed column or as rows the consumer lands beside the corpus? owner: store affects: store.snapshot
