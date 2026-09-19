# D16 — Untrusted input decodes off-process and fails whole

**Status:** accepted

## Context

Watched directories, bucket prefixes and fetched bodies are attacker-reachable. Native decoders have no bound on time or memory, the release build aborts on panic, and a partially read document lands text that retrieval, ranking and citation all quote as the whole.

## Decision

An input either lands whole and faithful or refuses by name, and no input ends the serving process.

- `run.land` decodes behind a process boundary that bounds wall clock and resident memory; a crash or fatal signal is the same named diagnosis as a returned parse error.
- An unreadable input is a permanent refusal naming path and internal position (page, worksheet, entry). It fails one table's pull; sibling tables keep their tick.
- A parse that covers part of an input refuses the whole input; no part lands.
- `connector.source` reads office containers by exact part name, answers a compound-binary container with a conversion command, and refuses external references.
- An image lands provenance with a null body and a null quotability marker, paired by refusal; no pixel decode runs at ingest.
- `run.fetch` reads UTF-8 only: a declared other charset, or undeclared bytes failing validation, refuse.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Process boundary, permanent whole-input refusal per table *(chosen)* | — | Every input pays a boundary crossing; one damaged page discards a readable document; non-UTF-8 publications are unreadable. |
| In-process unwind guard or watchdog thread | Effectiveness | The release build aborts on panic, and a thread stuck in native code is neither interruptible nor memory-bounded. |
| Land the parts that parsed with a truncation flag | Distinguishability | No reader consults the flag, so a fragment would be quoted as the document. |
| Skip the item and tally it | Answerability | A collection missing documents would read exactly like one that has none. |
| Lossy or statistical decoding | Honesty of landed values | Replacement characters and wrong guesses would land as publisher text. |

## Consequences

- A systematically broken directory emits one refusal per input.
- A large collection makes progress only after the offending file is removed or converted.
- Per-input time and memory budgets are named bounds of the decode process.

## Revisit

- Crossing overhead exceeds decode time on a corpus of small inputs.
- A read surface honors a completeness predicate, making partial landing distinguishable.
- A memory-safe compound-binary reader becomes available.
