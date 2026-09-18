# 0158 — One unit is bounded by wall clock, by captured output, and by the file its step promised to write

**Status:** accepted 2026-09-18
**Decides:** `derive.exec.refusal.non-zero-exit`, `derive.exec.refusal.deadline-elapsed`, `derive.exec.refusal.overflowing-step`, `derive.exec.refusal.silent-step`

## Context

An `exec` engine is a chain: zero or more media-to-media preprocess steps followed by one
media-to-cues engine step, each a third-party binary running inside a scratch directory created
for that unit. Speech tools and transcoders are long-running by nature — a chain's default
deadline is 1800 s — and they are also chatty, emitting progress lines to standard error for the
whole of that time. A derive run holds the pipeline lease while it works, so a unit that never
ends is a pipeline that never runs again.

The failure modes a chain exhibits are not symmetric. A tool that exits non-zero is
self-reporting. A tool that loops without exiting never reaches an exit code at all. A tool that
logs a progress line per frame produces output at a rate unrelated to the media's size. And a
tool that exits zero having written nothing is the worst shape, because it looks like success:
the chain substitutes `{input}` and `{output}` as whole argument elements and passes the next
step whatever path it was handed, so a preprocess step that transcoded nothing leaves the
previous file in place and the engine step derives cues from the wrong media, landing passages
that are internally consistent and about the wrong recording.

Killing a chain is also not the same as killing a process. A transcode tool spawns helpers; a
fetch script spawns a downloader. A surviving descendant holds the resolved environment — the
credentials 0157 hydrated for that unit — and holds the scratch directory open past the point
where the chain removed it, while the tier has already recorded the unit failed and moved on.

## Decision

A step exiting non-zero raises `DeriveStepExit`, naming the step and carrying its bounded error
text into the run audit. A chain outrunning its deadline raises `DeriveStepTimeout` naming the
step that was running. A step whose captured output crosses its bound raises `DeriveOutputCap`
naming the step and the byte count observed; the bound is tested before each wait rather than
after an exit, bytes past it are counted and discarded rather than buffered, and draining
continues so the child cannot block on a full pipe. A preprocess step exiting zero without
writing the file its template names raises `DeriveStepProducedNothing` and fails that unit. Each
child is spawned leading its own process group and the deadline signal reaches the group.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Three bounds tested during the run, plus a promised-file check, plus group kill** *(chosen)* | Every failure mode reaches a bound that its own shape cannot evade; a wrong-media derivation is impossible | A long but legitimate chain dies at the deadline; a verbose tool's useful output is truncated |
| Test the output bound after the child exits | One comparison, no per-read accounting | Lost on reachability: a tool logging in a loop never exits, so the bound never runs |
| Buffer all output and truncate at the end | Complete error text when the step is short | Lost on containment: the bound stops bounding the machine — memory grows with the tool's chattiness, and the daemon is the thing that dies |
| Continue past a step that wrote no output | Tolerates a tool that writes in place under another name | Loses on correctness: the next step receives the previous file and derives from the wrong media, landing plausible passages about the wrong recording |
| Kill the child alone, not its process group | Simpler signal handling; no group setup at spawn | Lost on containment: a surviving descendant keeps the resolved environment, holds the scratch directory open past removal, and spends the machine on a unit already recorded failed |

## Criteria

1. **Reachability** — whether the bound can be reached at all by the failure mode it targets.
2. **Containment** — whether the machine is still bounded once the bound fires, in memory, in
   processes and in disk.
3. **Correctness of what lands** — whether a partial or absent intermediate product can produce a
   row that reads as a valid finding.
4. **Diagnosability** — whether an operator can tell which step failed and why.

Criterion 1 decided it against the post-exit output check, and criterion 3 decided the
promised-file check. Reachability outranks the others because an unreachable bound is
indistinguishable from no bound while reading as protection; the other criteria describe how well
a bound that does fire behaves. Criterion 3 outranked convenience for the silent step because a
wrong-media passage carries a citation key that points into a real recording, so nothing
downstream can detect it.

## Consequences

Easier: a run audit names one step, one reason and a byte count, so an operator fixes the binding
rather than bisecting a chain. A unit's failure is contained to that unit's row; the run
continues.

Harder: a chain whose legitimate work exceeds the deadline has no partial credit — it is killed
and the unit is re-attempted from the start against the same bound, so raising the deadline is
the only fix. Draining-past-the-cap means the engine reads bytes it will discard, so a
pathologically verbose tool costs read syscalls for as long as it lives.

Accepted cost: a long but legitimate chain is killed at the deadline, and a tool whose useful
output is verbose is truncated at the byte bound, so the tail of a diagnostic — often the part
naming the actual error — is the part lost.

Expensive to reverse: the promised-file check makes `output_path` a contract rather than a hint.
A tool that writes to a name it chooses itself cannot be bound without a wrapper script, and
existing bindings encode paths on that assumption.

## Revisit triggers

- Legitimate chains reach the deadline routinely, observed as `DeriveStepTimeout` dominating
  marker rows for a task whose media length is within its engine's stated capability.
- A step's most diagnostic output consistently falls past the byte bound, observed as truncated
  error text that ends before the error.
- A platform where process-group signalling does not reach descendants becomes a target, making
  containment unachievable by this mechanism.
