# 0166 — An established absence and an unexplained silence are separate statuses under a column that is not the reserved name

**Status:** accepted 2026-09-18
**Decides:** `derive.land.refusal.unknown-status`, `derive.land.refusal.discriminator-name`

## Context

An engine that comes back with nothing has said one of two very different things, and the wire
does not distinguish them. It may have established that there is nothing to derive — the recording
holds no speech, the document declares no picture, a `404` is a positive answer about a document.
Or it may simply have produced nothing while being unable to say why.

The second population is large and it is not a recording property at all. A source gating on
authorization answers a caller it refuses the way it answers one asking about absent content: a
bot check, a geographic block, an age gate and a quota exhaustion all arrive as an empty result.
Folding those into an established-absence status writes a false statement into the store — the
recording is asserted silent when nobody ever heard it — and the consequence compounds, because an
established absence is attempted once and then settled. The anti-join then retires every one of
those units permanently and silently, and the operator's evidence that their deployment is being
refused by a publisher is a column saying the publisher had nothing to say.

The two populations also have different fixes. Unexplained silence is fixed by credentials, by a
different network path, by a slower cadence. Established absence is not fixed at all. They are
only separately fixable if they are separately countable.

The third question is what the discriminator column is called. `kind` is reserved: a table
declaring a column by that name is treated as a different genre, for the whole of its history, and
genre is what the read path filters on when deciding what a query returns. A derive table that
named its status column `kind` would silently change what every reader of that table sees, and the
change would apply retroactively to rows landed before the column existed.

## Decision

`unit_status` is `ok`; `empty`, where the engine established there is nothing to derive and said
so; `unavailable`, where the engine came back with nothing and did not say why; or `failed`, where
the engine returned a typed error. An engine answering with nothing and no stated reason lands
`unavailable`, which is the default. A status value outside the four raises
`DeriveUnitStatusUnknown`. The status discriminator is `unit_status`; a derive table declaring a
column named `kind` raises `DeriveReservedDiscriminator`.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Two empty statuses with unexplained as the default, under `unit_status`** *(chosen)* | The store acquires no fact nobody established; the two populations are separately countable; genre is untouched | Every engine carries an absence-reason channel and asserts emptiness explicitly |
| One empty bucket for both | One status, simplest schema and simplest consumer predicate | Loses on both content criteria: a bot check, a geographic block, an age gate and a quota all land as a false statement about the recording, and the anti-join then retires those units permanently and silently |
| Treat an empty answer as a transient error carrying no status | Nothing false is written; units stay outstanding | Loses on countability: the population is indistinguishable from transport failures, so nobody can tell a publisher refusing them from a network that flapped |
| Default to established-absence and let an engine opt into unexplained | Fewer engines to change; the common case is cheap to express | Loses on the first criterion — silence from an engine that was never taught the distinction becomes an assertion, which is the exact failure being avoided |
| Name the discriminator with the reserved word | Consistent with the vocabulary readers already know | Loses on genre: it flips the table retroactively across its whole history, and the read path filters on genre |

## Criteria

1. **Whether the store acquires a fact nobody established.** Measured by asking what a reader of
   a row would believe and whether anything observed it.
2. **Separate countability** — whether two differently-fixable populations can be counted apart.
3. **Whether a column name changes a table's genre** — and therefore what the read path returns
   for every row already landed.
4. **Engine-author burden** — how much an adapter must implement to land a correct status.

Criteria 1 and 2 decide the taxonomy together, and they are the same argument seen twice: a status
that conflates the two is both a false assertion and an uncountable one, and the settling behavior
turns the falsehood into a permanent one. Criterion 3 decides the column name alone and
independently, and it outranks familiarity because the damage is retroactive and silent — a rename
would change what readers see for history nobody re-derived. Criterion 4 is the cost.

## Consequences

Easier: an operator asks how many units their deployment was refused, as a count grouped by
status, and gets a number rather than an inference. An engine that genuinely establishes absence
gets the cheap attempt ceiling that finding deserves, and nothing else does.

Harder: every adapter implements an absence-reason channel and states emptiness rather than
returning an empty result. An adapter wrapping a vendor that does not distinguish the two cannot
manufacture the distinction, and correctly lands the unexplained status.

Accepted cost: an engine that cannot tell the difference lands the unexplained status and pays its
full attempt budget for content that genuinely has nothing. That is a real waste — up to the
configured ceiling per unit — and it falls hardest on exactly the engines whose vendors are least
informative.

Expensive to reverse: the four values are the validated domain and the refusal enforces it, so
adding a fifth is a schema and consumer change everywhere the column is read. The status also
drives the attempt ceiling, so re-mapping a value later changes how many times historical units
would have been attempted, which cannot be reconstructed from the rows.

## Revisit triggers

- Engines that cannot distinguish the two populations become the majority of bindings in practice,
  which would make the unexplained status the common case and its full attempt budget the dominant
  cost.
- A fifth outcome appears that fits none of the four, observed as `DeriveUnitStatusUnknown` raised
  by an adapter acting in good faith.
- The read path's genre filter changes such that a reserved column name no longer determines what
  a query returns, which removes criterion 3 entirely.
