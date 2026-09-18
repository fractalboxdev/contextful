# 0055 — Extract validates every candidate against a declared output schema, retries with feedback, and dead-letters the terminal failure

**Status:** accepted 2026-09-18
**Decides:** `memory.synthesize.refusal.candidate-schema`, `memory.synthesize.refusal.dead-letter`

## Context

Extract is the one synthesis stage that calls a model. It reads rows landed since the
pass's last cursor and emits tuples, entity mentions and candidate edges; Resolve is
deterministic and Consolidate deduplicates and writes. Everything downstream of Extract
treats its output as structured data — the deduplication key hashes a canonical subject and
a predicate, the revision rule reads a subject, predicate and scope, the evidence gate reads
row identifiers.

A model returns text. Text that nearly conforms is the common failure: a field renamed, a
confidence returned as a word, a list where an object was asked for, an explanation wrapped
around otherwise-valid content. Every one of those is recoverable by a lenient parser, and
every recovery is a guess about what the model meant, made by the component with the least
information about the domain.

The recovery lands in a row that afterwards is indistinguishable from a row the model
produced correctly. Provenance columns record which model and which template produced a
claim; they do not record that a parser repaired it.

Extraction reads from a cursor. A batch that fails has rows behind it, and what happens to
the cursor decides whether those rows are ever read again.

Failures are not uniform. A model that returns a malformed response once often returns a
conforming one when shown what was wrong. A template whose schema is unsatisfiable returns
a malformed response every time, at full cost, forever.

## Decision

Every candidate validates against the deployment's declared output schema. A candidate that
fails is fed back with the validation error for a further attempt, bounded at 3 attempts
per batch, and raises `MemoryCandidateSchemaInvalid` on the terminal attempt. A batch
exhausting its attempts writes the response, the template hash and the drop reason to the
dead-letter table, raises `MemoryExtractExhausted`, and leaves the cursor unadvanced for
that batch. No free-text path converts an unvalidated response into a row.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Schema validation, bounded retry with feedback, dead-letter with the cursor held** *(chosen)* | No unvalidated text reaches storage; transient malformation self-corrects; a failed batch is replayable after the template is fixed. | A template change that widens the schema leaves earlier dead-lettered batches needing an explicit replay, and a persistently failing source stalls its cursor until someone acts. |
| Best-effort parsing of a non-conforming response | Recovers most near-misses at zero model cost and never stalls a pass. | Loses on the no-free-text rule: the repaired row is indistinguishable from a conforming one downstream, so a parser's guess enters the claim table as a model's assertion. |
| Unbounded retry | Every transient failure eventually converges; nothing is ever dead-lettered for bad luck. | Loses on cost: a systematically failing template spends per attempt without converging, and the spend is unbounded and unattended. |
| Advance the cursor past a failed batch | The pass never stalls; one bad batch costs one batch. | Loses on recoverability: the rows behind the cursor are never read again, so fixing the template recovers nothing and the loss is silent. |
| Fail the whole pass on the first invalid candidate | Maximum loudness; no partial synthesis. | Loses on throughput: one malformed candidate discards every valid candidate in the pass, and the model cost for those is already spent. |

## Criteria

1. **No unvalidated response reaches storage** — whether text a schema rejected can become
   a row by any path. *This criterion decided the validation rule.* A claim table whose
   rows carry different levels of trust, with nothing marking which, poisons recall, the
   evidence gate and every downstream citation at once.
2. **Recoverability of a failed batch** — whether the rows behind a failure can be
   re-synthesized after the cause is fixed. *This criterion decided the cursor rule.*
   Holding the cursor is the only thing that makes a dead-lettered batch worth writing
   down; advancing past it turns a recoverable defect into a permanent gap.
3. **Bounded cost under systematic failure** — spend per batch when the template is simply
   wrong. This set the attempt bound at a number rather than leaving retry open.
4. **Throughput under isolated failure** — whether one bad candidate costs the good ones.
   This kept the failure per-batch rather than per-pass.

## Consequences

A persistently failing source stalls its own cursor. That is the intended behavior — the
rows are still there and still synthesizable — but it means an unattended dead-letter table
turns into an unattended backlog, and the pass stops making progress on that source without
erroring anything a dashboard watches by default.

Widening a template's output schema does not retroactively rescue earlier dead-lettered
batches; they are replayed explicitly. Since a changed template yields a different
deduplication key and both generations coexist under `prompt_version`, that replay is safe
to run and its results are distinguishable from the original generation.

The bound of 3 attempts is a cost ceiling, not a convergence measurement. How often a
second or third attempt actually converges on a conforming response is unmeasured, and the
number is a judgment about acceptable spend rather than an optimum.

## Revisit triggers

- Observed conversion at the second and third attempts is near zero, which makes the retry
  pure spend and argues for dead-lettering on the first failure.
- Dead-lettered batches routinely accumulate faster than they are worked, meaning the held
  cursor is producing backlog rather than recoverability.
- Output schemas become strict enough, or model conformance reliable enough, that terminal
  failures are dominated by genuinely unanswerable inputs rather than by malformation.
