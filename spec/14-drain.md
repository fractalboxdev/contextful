---
contract: store
owns:
  - drain
---

# Unpushed runs

Replacing a disposable disk loses locally committed runs that have not reached the bucket.
Operators use this contract to check these runs before replacing a disk or container.
The execution database tracks each run from opening until a successful push.
Commands, `pipeline serve` shutdown and release checks read that record.

## drain

Tracking unpushed runs, retrying pushes before reads, pushing during shutdown and reporting status to release checks.

- `mark` — Under `postgres` or `cloudflare-d1`, opening a run sets its row's `unpushed` mark with the node, before landing commits. Closing clears the mark when no run manifest exists in any table included in the push.
  *A-store*
- `record-unmarked` — The store's run record stores each run row without `unpushed`. Only the execution database stores that mark.
  *because a rebuilt execution database restores rows from the pushed run record, and a mark there returns every pushed run as unpushed*
- `clear` — A committed push clears marks for manifests present in included tables at push start. For marked runs lacking a local manifest and live owner, the push sets `lost` and names the runs on stderr.
  *because a committed push carries every run manifest the store held as it walked, and no retry restores a commit the store has lost*
- `before-read` — Under these profiles, {{store.pull.before-run}} pushes after pulling for unlost marks on its node with a local manifest or expired owner lease. A failed push names those runs and stops the command before its first read.
  *A-store*
- `deadline` — On a stop signal, `pipeline serve` with `push_after_run` ends its children. The server then pushes for marks on its node meeting {{store.drain.before-read}}, for at most 15 s.
  *because a container platform kills a stopping process after its grace period, and a replaced container discards every run its last push missed*
- `unfinished` — If the shutdown push fails or exceeds {{store.drain.deadline}}, the push preserves the run marks and names the runs on stderr. `pipeline serve` exits non-zero.
  *because a release step reading a clean exit as a carried store discards the runs the push missed*
- `status` — `contextful sync status` prints JSON with `error` and `runs`. Each marked run lists pipeline, table, node, end instant and `lost`. If any run is not `lost`, the command reports `SyncRunsUnpushed` and exits 3.
  *because a release step replacing a container discards each unpushed run, and an empty list then means no committed run is missing from the bucket*
- `status-unread` — If `sync status` fails to read the execution database, the command prints `runs` as null and `error` as `SyncStatusUnread` with its cause. The command exits 1.
  *because a release step gating on the status tells a store holding unpushed runs from a check that read nothing*

unsettled: Which command clears a `lost` mark once an operator re-runs or rewinds the run it names? owner: store affects: store.drain

#### Scenarios

- `store.drain.mark`: WHEN a process terminates after a run's local commit and before its push, THEN the run row stays marked and the next push uploads the run.
- `store.drain.before-read`: WHEN the bucket refuses a run's push and then accepts a push from `run start`, THEN that start pushes the marked run before opening its own run.
- `store.drain.clear`: WHEN a replacement disk holding none of a marked run's files pushes, THEN the run turns `lost` and `sync status` lists it without refusing.
- `store.drain.unfinished`: WHEN a stopping serve's push waits on a push guard past 15 s, THEN serve exits non-zero and the run stays marked.
