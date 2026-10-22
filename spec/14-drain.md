---
contract: store
owns:
  - drain
---

# Unpushed runs

A disposable disk loses every run its last push did not carry. The execution database
records each run from its open until a push carries it, so a later command, a stopping
serve and a release step each see it.

## drain

Carrying runs committed on a disposable disk to the bucket: the durable unpushed mark, the push retried before a read, the push at stop, and the status a release step reads.

- `mark` — Under a `postgres` or `cloudflare-d1` storage profile, a run row carries `unpushed`, naming the node, from the run's open, before its landing can commit; a run closing with no run manifest in a table its push carries drops it.
  *A-store*
- `record-unmarked` — The store's run record holds each run row without its `unpushed` mark, which only the execution database keeps.
  *because a pushed run record carrying marks would restore every pushed run as unpushed into a rebuilt execution database*
- `clear` — A committed push removes each mark whose run manifest a carried table held as it began, and sets `lost` on each marked run whose manifest the store lacks and no live owner runs, naming those on stderr.
  *because a committed push carries every run manifest the store held as it walked, and no retry restores a commit the store has lost*
- `before-read` — Under such a profile, {{store.pull.before-run}} pushes after its pull while its node holds an unlost mark whose run manifest the store holds or whose owner lease lapsed; a failed push stops the command before its first read, naming those runs.
  *A-store*
- `deadline` — On a stop signal, `pipeline serve` declaring `push_after_run` ends its children, then pushes while its node holds a mark {{store.drain.before-read}} pushes for, for at most 15 s.
  *because a container platform kills a stopping process after its grace period, and a replaced container discards every run its last push missed*
- `unfinished` — A stop push failing or outrunning {{store.drain.deadline}} leaves its runs marked and names them on stderr, and `pipeline serve` exits non-zero.
  *because a release step reading a clean exit as a carried store discards the runs the push missed*
- `status` — `contextful sync status` prints JSON holding `error` and `runs`, each marked run with its pipeline, table, node, end instant and `lost` flag; while one is not `lost`, `error` is `SyncRunsUnpushed` and it exits 3.
  *because a release step replacing a container discards each unpushed run, and an empty list then means no committed run is missing from the bucket*
- `status-unread` — A `sync status` whose execution database answers no read prints `runs` as null and `error` as `SyncStatusUnread` with its cause, and exits 1.
  *because a release step gating on the status tells a store holding unpushed runs from a check that read nothing*

unsettled: Which command clears a `lost` mark once an operator re-runs or rewinds the run it names? owner: store affects: store.drain

#### Scenarios

- `store.drain.mark`: WHEN a run is killed after its local commit and before its own push, THEN its row stays marked and the next push carries it.
- `store.drain.before-read`: WHEN a run's push the bucket refused is followed by a `run start` the bucket accepts, THEN that start pushes the marked run before its own run opens.
- `store.drain.clear`: WHEN a replacement disk holding none of a marked run's files pushes, THEN the run turns `lost` and `sync status` lists it without refusing.
- `store.drain.unfinished`: WHEN a stopping serve's push waits on a push guard past 15 s, THEN serve exits non-zero and the run stays marked.
