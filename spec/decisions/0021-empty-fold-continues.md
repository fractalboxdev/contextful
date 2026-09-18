# 0021 — Folding a table that has landed nothing reports that table and the pass continues

**Status:** accepted 2026-09-18
**Decides:** `store.fold.refusal.nothing-landed`

## Context

Compaction runs over a set of tables: a scheduled pass, or an operator command covering
every keyed table in a store. A run that pulls no rows still commits, so a table with a
schema, a committed run and no Parquet is an ordinary state — common on a new table, on a
seasonal source, and on any stream that is simply quiet this interval.

A pass that treats the first such table as a fatal error therefore fails routinely, on a
schedule, for a reason that is not a problem. Operators respond to a command that fails
for benign reasons the same way everywhere: they stop reading its exit status, or they
suppress it. That response is the real cost, because the same exit status is the only
signal for the failure that matters — a fold that does not run on a keyed table leaves it
reading as a plain union, counting a re-landed row once per run that landed it, and
inflating every sum over it without changing a single row count.

So the question is not whether an empty table is worth reporting. It is what an operator
does with the refusal, and whether the refusal's presence degrades the channel that carries
genuine failures.

A neighboring rule constrains the shape of the answer: a table name that resolves to no
schema halts the pass, because a typo and a quiet stream have to stay distinguishable.
Whatever an empty table does, it cannot be folded into that outcome.

## Decision

Folding a table that has landed nothing raises `StoreNothingLanded` for that table and the
pass continues to the next one. A pass over several tables returns a per-table result
carrying that outcome alongside the tables that folded, so a quiet stream does not turn a
scheduled pass into a failed command, and the pass's overall status stays reserved for
failures an operator acts on.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Report the empty table and continue the pass** *(chosen)* | The pass's exit status stays meaningful, so the genuine failure it also carries is still read | A pass over every keyed table returns a mixed result an operator reads rather than a single status, and a misconfigured table that never lands rows reports the same benign outcome as a quiet one |
| Refuse the whole pass on the first empty table | One status to check; every anomaly is loud | Lost on operator response: refusing an ordinary state trains operators to swallow the command's exit status, which also swallows the fold-did-not-run failure that silently multiplies restated rows |
| Treat an empty table as nothing-to-fold and report nothing | Cleanest output; no benign noise | Lost on operator response in the other direction: a table that never lands rows because it is misconfigured becomes invisible, and nothing in the pass output ever mentions it |
| Treat an unknown table name as nothing-to-fold too | One uniform "no rows here" outcome | Lost on distinguishability: it merges with the rule that keeps a missing relation an error, so a misspelled target reads as a quiet stream |

## Criteria

1. **Operator response to the refusal** — what a human does after seeing it a few times.
   *(the one that decided it)* A signal that fires on an ordinary state stops being read,
   and this signal shares a channel with the failure that silently multiplies restated
   rows. Simplicity of a single pass status was the competing criterion and lost, because
   it is an ergonomic cost paid once per reading, while a suppressed exit status is a
   failure never seen at all.
2. **Channel integrity** — whether a benign outcome degrades the signal a genuine failure
   uses.
3. **Distinguishability** — whether this outcome stays separable from a misspelled table
   name.
4. **Output simplicity** — how much an operator reads to know the pass went well.

## Consequences

A scheduled pass over a store with quiet tables succeeds, and its failure status means
something an operator acts on. Each empty table is still named in the result, so a table
that never lands rows is visible to anyone who reads the output.

The cost accepted is that the result is a per-table list rather than a single status: an
operator or a monitor reads structure instead of an exit code, and tooling around the pass
has to parse it. A misconfigured table that never lands rows reports exactly the same
benign outcome as a genuinely quiet one, so the pass output does not by itself separate
those two — that separation comes from elsewhere.

## Revisit triggers

- The pass acquires a monitored, structured result channel of its own, at which point the
  exit status stops being the shared signal and the argument for continuing weakens.
- Tables that have landed nothing for many consecutive passes become a tracked condition
  elsewhere, giving the misconfigured-versus-quiet distinction a home.
- Operators are observed suppressing the pass output wholesale, which would mean the mixed
  result has the same failure mode the refusing pass had.
