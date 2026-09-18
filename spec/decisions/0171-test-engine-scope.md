# 0171 — The single-file verb builds an engine exactly as a pipeline does and covers the transcribe half

**Status:** accepted 2026-09-18
**Decides:** `derive.test-engine.refusal.unsupported-driver`

## Context

A derive engine is a script somebody writes. It resolves through a binding, a search
path and a set of content pins, it runs under an environment allowlist, and it hands back
a transcript the parser then reads. Every one of those is a place a freshly written
script fails, and none of them fails in a way the script's author can see from the
script.

The only other place an engine runs is a scheduled pipeline. Debugging there means
waiting for a tick, watching a batch of units, and reading a failure through the marker
rows it landed — for a script that is wrong in its first line. A tool whose only
debugging path is a scheduled run is a tool nobody debugs; the author edits blind and
ships what stops producing errors.

A test verb that builds the engine a cheaper way removes the wait and gives up the thing
the wait bought. If the verb skips the content pin check, or assembles its own
environment, then the script that passes proves nothing about what the pipeline will do
with it, and the author has traded a slow honest signal for a fast false one.

The tier's engines are not one family. The transcribe drivers turn media into cues; a
`fetch` driver retrieves material by link and belongs to a different half of the tier,
with a different input, no media file and no transcript to print.

## Decision

`contextful derive test <engine> --media <file>` resolves the same binding, the same
search path, the same content pins and the same environment allowlist a scheduled
pipeline resolves. It writes nothing and reads no store. It prints the parsed transcript
with every step's captured error stream, or emits the same object under `--json`. A
`fetch` engine named to the verb raises `DeriveTestEngineUnsupported`, carrying the
message a pipeline gives for the same pairing.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Identical build, transcribe drivers only, refuse others by name** *(chosen)* | A passing test is evidence about the pipeline, and the verb reaches the failures a fresh script actually has | The verb covers one driver family, and a link engine gets no single-file path at all |
| Debug through a scheduled pipeline, no verb | Nothing to build; one execution path exists and is therefore the one that is correct | Lost on turnaround: the loop is a tick long and the signal arrives as marker rows, so the tool is one nobody uses |
| A lighter build path for the verb | Fast, simple to write, no pin resolution or allowlist assembly to reproduce | Lost on fidelity: a test that skips the pin check or the environment allowlist passes for scripts the pipeline refuses, which is worse than no test |
| Cover every driver by widening the verb's input | One verb for the whole tier | Lost on effort and on shape: a link driver takes no media file and prints no transcript, so it needs its own input, its own output and its own assertions — the work is a second verb wearing the first one's name |
| Silently do nothing for an uncovered driver | No error identifier to register | Lost on legibility: a verb that exits clean having run nothing reads as a passing test |

## Criteria

1. **Turnaround** — how long the edit-run-read loop takes on a failing script.
2. **Fidelity** — whether a passing run is evidence about what a scheduled pipeline does
   with the same engine.
3. **Coverage** — how much of the tier the verb reaches.
4. **Legibility of the uncovered case** — what an author learns when the verb does not
   apply.

Criterion 2 decided it where 1 and 2 pulled apart. Turnaround is the reason the verb
exists, and every shortcut that would make it faster is available. Fidelity outranks it
because a fast loop around a build that differs from the pipeline's produces confident
wrong conclusions — the author stops iterating at the point the cheap path goes green,
which is exactly where the real path may still fail. A slow truthful signal is worth
more than a fast one that has to be re-verified anyway.

## Consequences

A script author iterates in seconds against the resolution the pipeline performs, and a
green single-file run is quotable evidence. Any change to binding resolution, pin
checking or the environment allowlist reaches the verb for free, because the verb does
not hold a copy of any of them.

The cost accepted is that the verb exercises one driver family. A `fetch` engine has no
single-file path, and its author is back to the scheduled loop. The refusal carries the
pipeline's own message for that pairing so the author learns which of the two the
mismatch is — an unsupported driver rather than a broken script.

Reversing toward a cheaper build is possible but pointless; reversing the coverage
boundary means building the second verb, which is new work rather than an undo.

## Revisit triggers

- Link-driver scripts become common enough that their authors are debugging through
  scheduled runs.
- The pipeline's resolution grows a step the verb cannot perform without a store, which
  would break the fidelity the decision rests on.
- A driver family arrives whose input is a file and whose output parses as cues, which
  the existing verb covers with no new surface.
