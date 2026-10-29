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
  *because a rebuilt execution database restores rows from the run record*
- `clear` — A committed push clears each mark whose run manifest its included tables held at push start or its committed bucket manifest lists. It sets `lost` on other marks with no local manifest and no live owner, naming them on stderr.
  *because no retry restores a commit the store has lost*
- `before-read` — Under these profiles, {{store.pull.before-run}} pushes after pulling for unlost marks on its node with a local manifest or expired owner lease. A failed push names those runs and stops the command before its first read.
  *A-store#unpushed-run-marked*
- `deadline` — On a stop signal, `pipeline serve` with `push_after_run` ends its children, then pushes for its node's marks meeting {{store.drain.before-read}}, for at most 15 s.
  *because a platform kills a stopping process after its grace period*
- `unfinished` — A shutdown push failing or exceeding {{store.drain.deadline}} keeps the run marks and names the runs on stderr, and `pipeline serve` exits non-zero.
  *because a release step reading a clean exit as a carried store discards the missed runs*
- `status` — `contextful sync status` prints JSON `error`, the counts `open` and `lost` of marked runs, and `runs`, each with pipeline, table, node, end instant and `lost`; any run not `lost` reports `SyncRunsUnpushed`, exiting 3.
  *because a release step replacing a container discards each unpushed run*
- `status-summary` — Without `--all`, `runs` lists at most 20 runs of each kind, `lost` and not, in run id order; the counts still number every marked run.
  *because a caller keeping a bounded tail of stdout parses the whole document, and the counts carry what the list leaves out*
- `status-all` — `sync status --all` lists every marked run in `runs`.
  *because an operator auditing every mark reads the full list on request, apart from the release gate's bounded read*
- `status-unread` — A `sync status` failing to read the execution database prints `runs` as null and `error` as `SyncStatusUnread` with its cause, exiting 1.
  *because a release gate tells a store holding unpushed runs from a check that read nothing*
- `prune` — A committed push clears each `lost` mark, any node's, more than 7 d after {{store.drain.clear}} set it, keeping the run row; a mark lost without its instant takes the push's.
  *because only a push sets `lost`, so pruning there bounds the marks to one window of losses while status stays a read*

#### Scenarios

- `store.drain.mark`: WHEN a process terminates after a run's local commit and before its push, THEN the run row stays marked and the next push uploads the run.
- `store.drain.before-read`: WHEN the bucket refuses a run's push and then accepts a push from `run start`, THEN that start pushes the marked run before opening its own run.
- `store.drain.clear`: WHEN a replacement disk holding none of a marked run's files pushes, THEN the run turns `lost` and `sync status` lists it without refusing.
- `store.drain.clear`: WHEN a disk whose `pull_tables` excludes a marked run's table pushes while the bucket lists that run's manifest, THEN the mark clears and the run never turns `lost`.
- `store.drain.status-summary`: WHEN the execution database holds 300 lost marks and one open mark, THEN `sync status` lists the open run and 20 lost runs, counts 1 open and 300 lost, and exits 3.
- `store.drain.prune`: WHEN a push settles while one mark turned lost 8 d before and another 6 d before, THEN the push clears the first mark and the second stays listed.
- `store.drain.unfinished`: WHEN a stopping serve's push waits on a push guard past 15 s, THEN serve exits non-zero and the run stays marked.
