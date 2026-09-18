# 0099 — A decode that can die runs behind a process boundary

**Status:** accepted 2026-09-18
**Decides:** `pipeline.land.refusal.parse-boundary`

## Context

Document decoding is the one stage of the landing sequence that runs untrusted bytes
through code the engine did not write. Format readers — document, spreadsheet, archive,
image — are large native libraries with long histories of crashing on hostile or merely
unusual input. Their failure modes are not confined to returned errors: an out-of-bounds
access aborts, a malformed length field drives an allocation the address space cannot
satisfy, a cyclic structure drives a decode that never terminates.

The process those decoders would run in is the serving process. It holds the read face
answering queries, the scheduler firing sibling pipelines, and the run catalog. A decode
that takes that process down takes the store's availability with it, and does so on
input a stranger dropped in a watched directory.

The release profile aborts on panic. That is a deliberate property of the build and it
means an in-process recovery guard compiles to nothing that runs: the guard protects a
debug build and is inert in the profile an operator ships. Verifying containment in
development and shipping without it is the failure the boundary exists to prevent.

Unwinding is also not the whole threat. Two of the three failure modes above — unbounded
memory and unbounded time — are not panics at all. No in-process construct bounds either
for a synchronous native call; the call simply does not return. Containment has to be
able to kill, not merely catch.

## Decision

A decode that can die runs off the serving process. That boundary bounds a native
decode's wall clock and its resident memory, and makes a parse that never finishes
killable from outside. A non-zero exit or a fatal signal from the decode process raises
`PipelineParseCrashed`, naming the same input a clean parse failure would name, so a
crash and a returned error are one diagnosis for the operator. No input reachable from a
watched directory, a bucket prefix or a fetched response body ends the serving process:
sibling pipelines keep firing and the read face keeps answering.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A process boundary around the decode** *(chosen)* | Holds in the shipped profile; bounds time and memory as well as unwinding; the killer is outside the thing being killed | Every input pays a boundary crossing, and the process shape stays undecided, so per-input budgets are undefined |
| An in-process unwind guard | No crossing cost, no extra process to supervise | Lost on effectiveness in the shipped profile: the release build aborts on panic, so the guard protects debug builds alone and bounds neither time nor memory in any build |
| Trusting each parser to return errors | Simplest; no supervision, no marshalling | Lost on threat coverage: a native decode has no bound on time or memory, and a parse that never finishes takes the address space with it regardless of what its error type promises |
| A thread with a watchdog | Cheaper than a process; some time bound | Lost on effectiveness: a thread stuck in a native call is not interruptible, and its allocations are the shared heap's, so neither bound is enforceable |

## Criteria

1. **Effectiveness in the shipped profile** — whether the containment runs in the build
   an operator deploys, not only in development. *(decided it)*
2. **Threat coverage** — whether unbounded time and unbounded memory are contained,
   not unwinding alone.
3. **Cost per input** — the crossing and marshalling the choice adds to every decode.
4. **Diagnosis parity** — whether a contained death reads like an ordinary parse
   failure.

Effectiveness in the shipped profile decided it because it disqualifies the cheap option
outright rather than scoring it lower. A guard that is inert in release is not a weaker
containment; it is the appearance of one, verified green in development and absent in
production. Threat coverage then eliminates the remaining in-process forms, since
neither a thread nor a guard can kill a native call that will not return.

## Consequences

The serving process's availability stops depending on the quality of third-party
decoders and on the goodwill of whoever writes to a watched directory. A hostile
document is an entry in a run record rather than an outage.

Diagnosis parity means the operator reads one failure class: crashed and malformed land
in the same place naming the same input, and no one has to know which libraries abort
and which return.

The cost accepted is a boundary crossing per input, paid in serialization and process
overhead on every decode including the overwhelming majority that would never have
crashed. For a directory of many small documents that overhead is a meaningful fraction
of the landing time.

Uncertainty carries its size here: the process shape is open. A child per input and one
long-lived extractor differ by roughly an order of magnitude in per-input overhead and
differ in blast radius between inputs, and neither has been measured. Until that
settles, the per-input wall-clock and memory budgets are undefined rather than
conservatively set, so the boundary today bounds a decode's reach without bounding its
appetite by a stated number.

## Revisit triggers

- Measured crossing overhead exceeds the decode time itself on a representative corpus
  of small inputs.
- The release profile stops aborting on panic, which restores unwinding as a containable
  mode but leaves time and memory uncontained.
- The process shape settles, at which point the wall-clock and memory budgets become
  statable numbers and this record's undefined-budget cost is retired rather than
  carried.
