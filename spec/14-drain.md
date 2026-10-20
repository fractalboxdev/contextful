---
contract: store
owns:
  - drain
---

# Unpushed runs

A disposable disk loses every run its last push did not carry. The execution database
records each such run until a push carries it, so a later command, a stopping serve and a
release step each see it.

## drain

Carrying runs committed on a disposable disk to the bucket: the durable unpushed mark, the push retried before a read, the push at stop, and the status a release step reads.

- `mark` — Under a `postgres` or `cloudflare-d1` storage profile, `run start` and `pipeline run` set `unpushed`, naming the node, on the run row of each run whose run manifest their store holds, before {{store.push.after-run}} pushes.
  *A-store*
- `clear` — A committed push removes the mark from each run it read marked for its node whose run manifest the store holds, and sets `lost` on each whose manifest it lacks, naming those on stderr.
  *because a committed push carries every run manifest the store holds, and no retry restores a manifest the store has lost*
- `before-read` — Under such a profile, {{store.pull.before-run}} pushes after its pull while its node holds a run marked `unpushed` and not `lost`; a failed push stops the command before its first read, naming each such run.
  *A-store*
- `deadline` — On a stop signal, `pipeline serve` declaring `push_after_run` ends its children, then pushes when it ended a running child or its node holds an unpushed run not `lost`, for at most 15 s.
  *because a container platform kills a stopping process after its grace period, and a replaced container discards every run its last push missed*
- `unfinished` — A stop push failing or outrunning {{store.drain.deadline}} leaves its runs marked and names them on stderr, and `pipeline serve` exits non-zero.
  *because a release step reading a clean exit as a carried store discards the runs the push missed*
- `status` — `contextful sync status` prints as JSON each run marked `unpushed`, with its pipeline, table, node, end instant and `lost` flag, and after printing raises `SyncRunsUnpushed` when any is not `lost`.
  *because a release step replacing a container discards each unpushed run, and the execution database answers from outside the disk the release discards*

unsettled: Which command clears a `lost` mark once an operator re-runs or rewinds the run it names? owner: store affects: store.drain

unsettled: Does a run killed between its local commit and {{store.drain.mark}} need its mark written before that commit? owner: store affects: store.drain

#### Scenarios

- `store.drain.before-read`: WHEN a run's push meets a held push guard and the next `run start` finds the guard free, THEN that start pushes the marked run before its own run opens.
- `store.drain.clear`: WHEN a replacement disk holding none of a marked run's files pushes, THEN the run turns `lost` and `sync status` lists it without refusing.
- `store.drain.unfinished`: WHEN a stopping serve's push waits on a push guard past 15 s, THEN serve exits non-zero and the run stays marked.
