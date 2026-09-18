# 0084 — A history window bound is a bare date or a UTC instant ending in Z, and every other spelling is refused

**Status:** accepted 2026-09-18
**Decides:** `run.record.refusal.bound-spelling`

## Context

Run history is read through a window: an inclusive lower bound on the start instant plus a row
ceiling, both applied in the storage adapter's `WHERE` clause, newest first. The bound reaches
that clause as a string and is compared against timestamps the store wrote canonically. The
comparison is byte-wise.

That is the whole difficulty. Byte-wise comparison over timestamp text is correct only while
every value on both sides is written the same way, and it fails silently otherwise. A zone
offset changes both the length and the trailing characters, so `2026-04-01T00:00:00+08:00`
sorts against stored Zulu values by character rather than by instant. A space separator sorts
below `T`. A lower-case `t` or `z` sorts above the digits and above the upper-case letters. In
every one of those cases the query runs, returns rows, and returns the wrong ones — the window
has moved rather than failed, and nothing in the response says so.

No adapter can detect the difference. The comparison is a string predicate pushed into storage;
it has no type to check and no error to raise. A caller reading a clipped or shifted window sees
a plausible page of history and has no way to tell it apart from a quiet week.

The bound also arrives from several surfaces — the describe window, the process listing, the
NDJSON export — and each of them would otherwise own its own parsing.

## Decision

A window's lower bound is spelled `YYYY-MM-DD` or as a UTC RFC3339 instant ending in `Z`. A
zone offset, a space separator, or a lower-case `t` or `z` raises `HistoryBoundSpelling` by
name at every caller-facing surface. The two accepted spellings are exactly the ones that
compare correctly byte-wise against what the store itself writes: a bare date is a prefix of
every instant in that day, and a Zulu instant is character-identical in form to a stored value.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Accept two spellings, refuse the rest** *(chosen)* | Acceptance means the bound compares correctly. One rule, stated once, enforced at every caller-facing surface. | A caller holding a local-offset timestamp converts before asking, and the refusal reads as pedantry until the alternative is explained. |
| Accept the full instant grammar | Every well-formed RFC3339 value a caller already holds just works, with no conversion step. | Loses on silence. An offset, a space separator or a lower-case letter all sort wrong against a stored value, no adapter can detect the difference, and the window moves rather than failing. |
| Parse the bound and re-render it canonically | Callers get the full grammar and the comparison stays correct. | Loses on cost. It pushes time parsing into every storage adapter and every surface, and a parser that is lenient in a way the store is not re-creates the same silent mismatch one layer up. |
| Take a typed instant rather than a string | The grammar question disappears at the type boundary. | Loses on reach. The bound crosses a terminal argument, an HTTP query parameter and an export flag, each of which hands over text; the type exists only after something has already decided how to read that text. |

## Criteria

1. **Whether an accepted bound compares correctly.** Whether acceptance carries a guarantee
   rather than a hope.
2. **Detectability of a wrong answer.** Whether a mis-spelled bound produces an error or a
   plausible page.
3. **Cost of the rule across surfaces.** How much machinery each caller-facing surface owes.
4. **Convenience for the caller.** How much conversion work a caller with an ordinary
   timestamp does.

Criterion 1 decides it, and criterion 2 is why. Accepting only what the store itself writes is
what makes acceptance meaningful: every other option accepts values that may be right, with a
failure mode that returns data rather than an error. A refusal an operator disputes is
recoverable in seconds; a shifted window that looks like history is not recoverable at all,
because nobody goes looking.

## Consequences

The bound rule is one sentence and holds identically at the terminal, over HTTP and in the
export, so a caller learns it once. The storage adapter keeps a plain string predicate with no
time logic in it, which is what lets the same window work across adapters.

The cost accepted is on the caller. A timestamp held in a local offset — the common shape from
a browser, a log line or a shell date command — is refused and has to be converted first. That
conversion is trivial and is nonetheless a step, and the refusal reads as pedantry to anyone who
has not been told that the alternative is a window that silently moves.

Two accepted spellings also means two behaviors at the day boundary: a bare date bounds at the
start of that day, which is a fact a caller has to know when they reach for the shorter form.

## Revisit triggers

- Stored timestamps stop being written in one canonical form, which removes the byte-wise
  correctness the two accepted spellings rest on.
- A caller class arrives whose bounds are machine-generated in a third canonical form, making
  the conversion step a recurring cost rather than a one-time one.
- The window predicate moves out of a string comparison and into a typed storage boundary,
  which would make the full grammar safe to accept.
