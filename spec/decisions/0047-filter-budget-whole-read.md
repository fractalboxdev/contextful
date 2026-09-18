# 0047 — An over-budget filter refuses the whole read rather than dropping one arm

**Status:** accepted 2026-09-18
**Decides:** `read.retrieve.refusal.filter-budget`

## Context

A ranked read builds one arm per table and unions them beneath a single ranking. A caller's
`filter` binds caller-named columns across those arms, and it is bounded: at most 256 entries
in a membership list, with the condition count and total byte size bounded alongside. The
bound exists because a filter is caller-supplied input that becomes predicate structure in
every arm, so an unbounded one is an unbounded amount of work per table.

A filter that exceeds the bound has to be handled somewhere, and the arms make a per-arm
response tempting. The union already drops arms for a different reason: a table lacking a
column the filter names makes its arm unsatisfiable, and that arm is dropped rather than
emitted unfiltered, precisely so an equality predicate cannot come back with every row of a
table that does not have the column. Dropping is an established move here.

The two cases are not the same, though. An unsatisfiable arm is dropped because the table
provably has no matching rows — dropping it changes the result by exactly zero rows. An
over-budget condition is a condition the caller wrote and meant, and dropping the arm it
appeared under removes rows the caller wanted, while dropping only the condition returns rows
the caller excluded. Either way the response is a result set over a different question.

And the response cannot say so. The retrieval block reports the window, the candidate counts,
the matched count and the floor, all computed over whatever ran. None of those numbers changes
shape when an arm was silently omitted, so a caller comparing them against an expectation sees
a plausible read.

## Decision

The budget is checked once over the whole filter, ahead of building any arm. An oversized or
malformed condition raises `FilterBudgetExceeded` for the whole read, including the tables the
condition did not name, rather than dropping the table the condition appeared under.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Check once over the whole filter; refuse the whole read** *(chosen)* | A caller either gets an answer to the question it asked or gets a named refusal. One check site rather than one per arm. | One malformed condition costs the whole read, including the tables it did not name. |
| Drop the arm whose condition is oversized | The read still answers over the remaining tables. | Loses on detectability — the response carries rows from the other tables and nothing says one table went unfiltered or unqueried. |
| Truncate an oversized membership list | The condition survives in a narrower form and the read answers. | Loses on the same criterion, and more sharply: it silently changes the question rather than silently dropping a table. |

## Criteria

1. **Detectability** — whether a caller can tell that a condition it wrote was not applied.
   Both rejected options fail this, and neither leaves a signal in the retrieval block.
2. **Cost of the check** — whether the budget is evaluated once or once per arm. Checking ahead
   of arm construction is strictly cheaper and has one failure point.
3. **Partial usefulness** — whether a caller gets something back from an over-budget request.
   This is the criterion the chosen option loses on.

Detectability decides it. A ranked read's whole value is that its counts and its ordering are
statements about the corpus; an answer assembled from a question the caller did not ask
corrupts exactly that, and the caller who is misled has no way to notice. A refusal names
itself and the caller retries with a smaller filter.

## Consequences

The retrieval block's numbers stay interpretable: every count in a successful response was
computed over the filter the caller supplied, in full. Budget handling lives in one place
rather than being restated per arm, which keeps a later arm safe to add.

The accepted cost is bluntness. A caller filtering five tables, one of whose conditions is over
budget, loses the other four tables' results as well, and there is no partial mode. For a
caller assembling a filter programmatically from a large membership set, that means one
oversized set fails the whole call rather than degrading — which is the intended behavior and
also the most likely source of complaints.

Reversing this is cheap mechanically and re-opens the detectability problem in full, since any
partial mode needs a signal the response format does not carry.

## Revisit triggers

- The response projection gains a field naming conditions that were not applied, which would
  make a partial mode detectable and therefore arguable.
- Callers are observed routinely hitting the budget with legitimate membership sets, which
  would mean the bound is wrong rather than the handling.
- Per-arm filter construction becomes expensive enough that checking once ahead of it stops
  being the cheaper site as well as the safer one.
