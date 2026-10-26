---
contract: store
owns:
  - drain
---

# Unpushed runs

Replacing a disposable disk loses committed runs that have not reached the bucket.
The execution database marks each run from opening until a push carries it, and commands, `pipeline serve` shutdown and release checks read the mark.

## drain

Tracking unpushed runs, retrying pushes before reads, pushing during shutdown and reporting status to release checks.

- `mark` — Under `postgres` or `cloudflare-d1`, opening a run marks its row `unpushed` with the node before landing commits; closing clears the mark when no table the push includes holds a manifest of the run.
  *A-store#unpushed-run-marked*
- `record-unmarked` — The store's run record keeps each run row without `unpushed`; only the execution database holds the mark.
  *because a rebuilt execution database restores rows from the run record, resurrecting any mark it held*
- `clear` — A committed push clears the marks of manifests its included tables held at push start, and sets `lost` on marked runs with no local manifest and no live owner, naming them on stderr.
  *because no retry restores a commit the store has lost*
- `before-read` — Under these profiles, {{store.pull.before-run}} pushes after pulling for unlost marks on its node with a local manifest or expired owner lease. A failed push names those runs and stops the command before its first read.
  *A-store#unpushed-run-marked*
- `deadline` — On a stop signal, `pipeline serve` with `push_after_run` ends its children, then pushes for its node's marks meeting {{store.drain.before-read}}, for at most 15 s.
  *because a platform kills a stopping process after its grace period*
- `unfinished` — A shutdown push failing or exceeding {{store.drain.deadline}} keeps the run marks and names the runs on stderr, and `pipeline serve` exits non-zero.
  *because a release step reading a clean exit as a carried store discards the missed runs*
- `status` — `contextful sync status` prints JSON `error` and `runs`, each marked run with pipeline, table, node, end instant and `lost`; any run not `lost` reports `SyncRunsUnpushed`, exiting 3.
  *because a release step replacing a container discards each unpushed run*
- `status-unread` — A `sync status` failing to read the execution database prints `runs` as null and `error` as `SyncStatusUnread` with its cause, exiting 1.
  *because a release gate tells a store holding unpushed runs from a check that read nothing*

unsettled: Which command clears a `lost` mark once an operator re-runs or rewinds the run it names? owner: store affects: store.drain

#### Scenarios

- `store.drain.mark`: WHEN a process terminates after a run's local commit and before its push, THEN the run row stays marked and the next push uploads the run.
- `store.drain.before-read`: WHEN the bucket refuses a run's push and then accepts a push from `run start`, THEN that start pushes the marked run before opening its own run.
- `store.drain.clear`: WHEN a replacement disk holding none of a marked run's files pushes, THEN the run turns `lost` and `sync status` lists it without refusing.
- `store.drain.unfinished`: WHEN a stopping serve's push waits on a push guard past 15 s, THEN serve exits non-zero and the run stays marked.
