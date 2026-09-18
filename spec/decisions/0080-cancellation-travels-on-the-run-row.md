# 0080 — A stop reaches a run in another process as a catalog write on its run row

**Status:** accepted 2026-09-18
**Decides:** `run.cancel.refusal.abandoned-pull`

## Context

A stop is issued by one process and observed by another. The terminal, the control plane and
the run in flight are separate, and a foreground run started at the same terminal is separate
again. What all of them already share is the catalog: it holds journal rows, execution
ownership, cursor positions and the awakeable registry as durable state one process writes at
a time, and the run row itself is written there when the run opens, ahead of the first pull.

Where the stop is observed is the second half of the question, and it is decided by the shape
of a pull. A source read materializes a whole pull before returning. An upstream that accepts
the connection and then does not answer sits inside one step, for as long as its own timeouts
allow. A check placed between recorded steps therefore reaches a run that is making progress
and does not reach the one state an operator actually reaches for a stop.

The land path is the opposite shape. Once bytes are home, landing them is bounded local work
over a batch the engine already paid a vendor to fetch.

## Decision

A stop is written onto the run row as a requested-at instant, a scope and an optional reason;
the catalog is the channel and no control socket exists for this or anything else. The pull is
raced against the stop on a 500 ms poll, with the cancellation arm evaluated ahead of the
pull's, and a request already standing when the race begins is observed on the first tick. The
land path carries no stop check. An abandoned pull surfaces as a typed `Canceled` failure
rather than a dropped future, so the ordinary failure path settles the outbound request
ledger, closes the run record and leaves the position alone.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A catalog write on the run row, raced against the pull** *(chosen)* | Both processes already share the channel, and the check reaches the state an operator stops for: a pull that is not returning. | An abandoned pull drops its future mid-await, so a stop is only as prompt as the poll interval, and the poll adds a catalog read while a pull is in flight. |
| A control socket between the processes | Delivery is immediate and carries no polling cost. | Loses on infrastructure. It invents a channel where the catalog already connects both processes, and it does not reach a foreground run on the same terms — that run has no socket to listen on. |
| A check between recorded steps | Free: no race, no poll, no dropped future. | Loses on reach. An unanswering upstream sits inside one step, so the check never fires in the case a stop is issued for. |
| A stop check on the land path as well | The stop is honored at the last possible moment. | Loses on value. It discards a recorded pull the engine already paid for, to save the seconds that landing bytes locally takes. |
| A signal to the process | Immediate, and the operating system carries it. | Loses on grain and on durability. A signal reaches a process, not one of several runs inside it, and leaves no record a later reader can see. |

## Criteria

1. **What the two processes already share.** Whether the channel exists or has to be built.
2. **Reach of the check.** Whether it fires in the states an operator issues a stop for.
3. **Value discarded.** Whether honoring the stop throws away work already paid for.
4. **Promptness.** How long after the request the run notices.

Criterion 2 decides the placement. A stop is issued when something is not returning, and a
check that only runs between steps is absent from exactly that situation — reach outranks
promptness here, because a check that fires quickly in the cases nobody stops is worth less
than one that fires at all in the case everybody does. Criterion 1 decides the channel against
the socket; criterion 3 keeps the check off the land path.

## Consequences

A stop is durable data, so it is readable after the fact: the run record carries the request,
and a run that finished before observing it carries a request it did not fulfil. The failure
travels the engine's ordinary failure path rather than a special one, which means the outbound
request ledger settles, the run record closes with a terminal status, and the position stays
where it was — a stopped run advances no position.

The accepted cost is on the abandoned pull. Racing means dropping a future mid-await, so
whatever the pull held is released without ceremony and any partial vendor interaction is
settled by the ledger rather than by the connector. Promptness is bounded by the poll interval
rather than by delivery, and the poll itself adds a catalog read for every interval a pull is in
flight — small, and paid by every run whether or not a stop ever comes.

A storage failure during the poll warns and keeps polling, so a blip does nothing that only an
operator is permitted to do.

## Revisit triggers

- The catalog read under the poll becomes a measurable cost on high-concurrency fires.
- A deploy target arrives where the executing run and the requesting process do not share a
  catalog, which removes the channel this decision rests on.
- Pulls become incrementally streamed rather than materialized whole, which would make a
  between-batch check reach the states a poll reaches.
