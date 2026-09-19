# D06 — The schema lattice has one promotion, and a key never widens

**Status:** accepted

## Context

Sources emit one column as an integer in one batch and a float in the next. Parquet stores each file's physical type, so the store either reconciles the pair or refuses it. A float cannot represent every 64-bit integer, so promoting a key column can collapse distinct keys onto one value.

## Decision

- `store.reconcile` models one promotion: `Int64` with `Float64` becomes `Float64`, the only pairing that produces physically mixed Parquet. Widening happens in the scan's own type resolution, and the generated relation carries no per-column cast. Any other pair refuses at the write, naming the column, the stored type and the arriving one.
- A primary-key column takes no float promotion. Reconciliation refuses the widening batch before any Parquet is written, and `store.fold` refuses again on a key already reconciled to `Float64`. A key value beyond the exact-integer range of `Float64` is emitted as a string.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| One promotion; keys excluded; refuse at the write *(chosen)* | — | A source flipping between text and number fails its batch until the declaration or the source is fixed. |
| Widen every clash to text | Read-time recoverability | Every downstream query casts, and a numeric aggregate silently becomes a string comparison. |
| A variant column holding both physical types | Read-time recoverability | The projection differs per row, and no single Arrow type describes the column. |
| Fail the read instead of the write | Who receives the error | The rows have landed, and the refusal reaches a reader who cannot fix the source. |
| Document key widening as an operator obligation | Irreversibility | One out-of-range value in one batch merges distinct keys under the fold, with no signal. |

## Consequences

- Every accepted column has one logical type across its files.
- A keyed table's identity survives any source that emits integers past the float range.

## Revisit

- A second pairing that some source emits routinely and that the scan resolves losslessly.
