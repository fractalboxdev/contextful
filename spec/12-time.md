---
contract: store
owns:
  - bound-time
  - available-time
---

# Time bounds

Rows record transaction time; tables optionally declare valid-time and availability-time columns.
The engine stamps transaction time at commit. Table declarations name the valid-time and availability-time columns.
Implementers use this contract to resolve timestamp bounds before access controls select rows.

## bound-time

Transaction-time and valid-time columns, their read parameters and the snapshots or rows each parameter selects.

- `two-clocks` — Transaction time is `_ingested_at`. Valid time uses a declared pair of table columns. The engine infers no valid-time pair and stamps no second transaction timestamp.
- `valid-time-declaration` — `[pipeline.tables.valid_time]` declares `from` and an optional `to`; a table declaring `from` alone treats each row as valid from that instant onward.
- `valid-time-type` — A declared valid-time column whose type is not a timestamp raises `StoreValidTimeNotTimestamp` at declaration.
  *P1*
- `instant-comparison` — Every bound compares instants as timestamps rather than strings. The code constructing a date-only literal resolves the literal to the start of the next day, exclusive.
- `as-of` — Inside each table's FROM-source, `as_of` selects the newest reachable snapshot created at or before the bound. The source adds committed runs at or before the bound that the snapshot omits.
  *because filtering the current files by ingest stamp returns different rows before and after a fold*
- `row-age-cutoff` — A statement reading a table with {{store.declare.retain-rows}} excludes rows older than its wall-clock cutoff, including reads of retained older snapshots with `as_of`. The result cache reuses no such statement.
  *because a historical snapshot does not restore a row after its declared age passes*
- `as-of-unretained` — If retention removed table history and `as_of` precedes the oldest retained snapshot, the read raises `StoreAsOfUnretained`, naming the oldest answerable instant.
  *P4*
- `valid-as-of` — `valid_as_of` filters the same inner source with `from <= valid_as_of AND (to IS NULL OR to > valid_as_of)` over the declared pair.
- `valid-time-undeclared` — A `valid_as_of` read against a table declaring no pair raises `StoreValidTimeUndeclared`, naming the table.
  *P1*
- `pin-bound` — A read carrying `as_of` and a build pin resolves each table named by both by {{read.resolve-pin.earlier-bound-wins}}.
- `unbounded-latest` — A read carrying neither bound resolves each table to its current snapshot plus the committed runs it omits.
- `beneath-enforcement` — Both bounds apply before the row predicate, column mask and tenant filter. The bounds only narrow the rows access controls admit.
  *P5*
- `covering-versions` — For an unkeyed table, a valid-time bound returns every version whose interval contains the requested instant.
- `interval-rows` — A keyed table declaring `valid_time` stores one row per key and valid-time interval. The table deduplicates on its primary key and `from`, so a fold keeps every interval.
  *A-store*
- `echo` — A bounded read returns `contextful.bounds` as `{as_of?, valid_as_of?, inclusive}`. Each instant uses RFC 3339 UTC with nine fractional digits and a `Z` suffix. An unbounded read omits `contextful.bounds`.

## available-time

The source availability timestamp, its value at landing and row selection under `available_as_of`.

- `declaration` — `[pipeline.tables.available_time]` declares `column`, the source availability instant as `timestamptz`, and optional `zone`, default UTC. Landing preserves the source instant without substituting an engine timestamp. Availability bounds compare only that column.
  *A-store*
- `lag-fill` — At landing, `available_time.lag` fills a null availability column with valid-time `to`, else `from`, plus the duration from {{store.available-time.lag-class}}. The engine injects `_available_imputed` true on that row.
  *because a source can publish different record classes after different declared delays*
- `lag-class` — `available_time.lag = { class_column = "<column>", classes = { <class> = "<n>d" } }` maps each row's class to a duration. A class without an entry resolves no instant; landing refuses the row under {{store.available-time.null}}.
- `date-end` — A date-typed availability value resolves at landing to the start of the following day in the declared `zone`.
  *because a date alone gives no publication time within that day*
- `live-since` — `available_time.live_since` names the instant a source began publishing live. The engine injects `_available_reconstructed` true on each row available before that instant.
  *because a vendor's point-in-time history before its live date was rebuilt after the fact*
- `available-as-of` — `available_as_of` filters the inner source with `<column> <= available_as_of` over the declared column. The filter combines with {{store.bound-time.as-of}} and {{store.bound-time.valid-as-of}} when the read supplies those bounds.
  *A-store*
- `table-key` — Beyond {{store.declare.table-block}}, a table block may declare `available_time`, absent from the canonical serialization when unset.
- `beneath-enforcement` — `available_as_of` applies before access controls under {{store.bound-time.beneath-enforcement}}, like the other two bounds. The bound only narrows the rows access controls admit.
  *P5*
- `bound-echo` — A read carrying `available_as_of` adds `available_as_of` to the object {{store.bound-time.echo}} echoes, in the same instant form.
- `undeclared` — An `available_as_of` read naming a table that declares no `available_time` raises `StoreAvailableTimeUndeclared`, naming the table.
  *because falling back to `_ingested_at` hides a backfill and falling back to nothing leaks it*
- `null` — Landing a row whose resolved availability instant is null raises `StoreAvailableTimeNull`, naming the table and the run.
  *because a row without an availability instant is visible to every bound or to none, and either misstates what was available from the source*
- `vintage-rows` — A keyed table declaring `available_time` is a vintage table. The table keeps one row per primary key, declared valid-time `from` and availability instant. A revised value lands beside the earlier value.
  *A-store*
- `vintage-resolution` — For each primary key and valid-time `from`, a vintage-table read returns the vintage with the latest availability instant at or before `available_as_of`. Without an availability bound, the read returns the latest vintage.
  *A-store*
- `vintage-first` — For each primary key and valid-time `from`, `vintage = "first"` returns the vintage with the earliest availability instant. The read respects `available_as_of` when supplied.
  *because a forecast scored against the initial release judges what was knowable then, not a revision published later*
- `vintage-hash` — A landing row whose `content_hash_column` value equals that of its key's latest vintage lands no new vintage.
  *because a re-pulled unchanged window otherwise lands a false restatement*
- `vintage-hash-undeclared` — A vintage table declaring no `content_hash_column` raises `StoreVintageHashUndeclared` at validation, naming the table.
  *because without a declared hash the engine cannot tell a restatement from a re-pulled unchanged row*
- `vintage-retain-rows` — A vintage table declaring {{store.declare.retain-rows}} raises `StoreVintageRetainRows` at validation, naming the table.
  *because a wall-clock row-age cutoff shrinks a past answer as real time passes*
- `vintage-dedup-key` — A vintage table reads through {{store.declare.dedup-view}} with the availability column appended to its partition key, then resolves one row per {{store.available-time.vintage-resolution}}.
  *A-store*
- `vintage-interval-key` — A vintage table declaring `valid_time` dedupes as {{store.bound-time.interval-rows}} with the availability column added to the key, so every interval keeps every vintage.
  *A-store*
- `vintage-fold-key` — A fold over a vintage table partitions under {{store.fold.valid-time-line}} or {{store.declare.dedup-view}}, adding the availability column. The fold preserves every vintage.
  *A-store*
- `vintage-scope` — A vintage landing compares only its own keys' stored vintages. Landing skips row groups excluded by text or signed-integer key bloom filters or footer bounds on text, integer or temporal keys or temporal valid time.
  *A-store*
- `vintage-fold-order` — If a vintage table declares `valid_time` and no `cluster_by`, a fold sorts the snapshot by valid-time `from`, then primary key. Footer bounds then support {{store.available-time.vintage-scope}} after the fold.
  *A-store*
- `adjustment-factors` — `available_time.adjustment = { table = "<name>", factor_column = "<column>", adjust_columns = [...] }` keeps stored values raw. An `available_as_of` read multiplies each listed numeric column by the product of factor vintages available by the bound.
  *because applying a correction published later exposes information unavailable at an earlier read bound*
- `adjustment-join` — A factor row applies to each row agreeing on every factor-table `primary_key` column whose valid-time `from` precedes the factor row's valid-time `from`.
  *A-store*

#### Scenarios

- `store.available-time.available-as-of`: WHEN a 2026 backfill lands a first-quarter measurement with availability date `2014-04-30`, THEN `available_as_of` `2014-05-01` returns the row and `available_as_of` `2014-04-29` returns none.
- `store.available-time.vintage-resolution`: WHEN a first-quarter measurement lands at 17149.6 available `2014-04-30`, 17101.3 available `2014-05-29` and 17016.0 available `2014-06-25`, THEN `available_as_of` `2014-06-01` returns 17101.3 and an unbounded read returns 17016.0, before and after a fold.
- `store.available-time.vintage-first`: WHEN those three measurement vintages have landed, THEN a read carrying `vintage = "first"` returns 17149.6.
- `store.available-time.vintage-hash`: WHEN a re-pull lands the measurement at 17016.0 with an unchanged content hash, THEN the table still holds three measurement vintages.
- `store.available-time.vintage-scope`: WHEN day 101 of 100 entities lands over 100 stored daily runs, THEN its comparison decodes no stored row; WHEN a revision of day 50 lands, THEN the comparison decodes only day 50's 100 rows.

unsettled: Does `vintage = "first"` count a same-day correction as a revision of the initial release? owner: store affects: store.available-time
