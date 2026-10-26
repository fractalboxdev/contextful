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

- `declaration` — `[pipeline.tables.available_time]` declares {{type:contextful_core::store::declare::AvailableTime}}, absent from canonical serialization when unset. `column` holds the source availability instant as `timestamptz`, which landing keeps without substituting an engine timestamp and availability bounds compare alone; `zone` defaults to UTC.
  *A-store*
- `lag-fill` — At landing, `available_time.lag` fills a null availability column with valid-time `to`, else `from`, plus the duration `lag.classes` maps the row's `lag.class_column` value to, and injects `_available_imputed` true; a class without an entry leaves the instant null.
  *because a source can publish different record classes after different declared delays*
- `live-since` — A date-typed availability value resolves at landing to the start of the following day in the declared `zone`. The engine injects `_available_reconstructed` true on each row available before `available_time.live_since`, the instant the source began publishing live.
  *because a date alone gives no publication time within that day, and a source's history before its live date was rebuilt after the fact*
- `available-as-of` — `available_as_of` filters the inner source with `<column> <= available_as_of` beneath access controls, under {{store.bound-time.beneath-enforcement}}, and combines with {{store.bound-time.as-of}} and {{store.bound-time.valid-as-of}} when the read supplies those bounds.
  *A-store, P5*
- `bound-echo` — A read carrying `available_as_of` adds `available_as_of` to the object {{store.bound-time.echo}} echoes, in the same instant form.
- `undeclared` — An `available_as_of` read naming a table that declares no `available_time` raises `StoreAvailableTimeUndeclared`, naming the table; landing a row whose resolved availability instant is null raises `StoreAvailableTimeNull`, naming the table and the run.
  *because a missing availability instant, on the table or the row, either hides a backfill or leaks it to every bound*
- `vintage-rows` — A keyed table declaring `available_time` is a vintage table, keeping every vintage per primary key, valid-time `from` and availability instant; a read returns the latest at or before any `available_as_of`, or under `vintage = "first"` the earliest.
  *A-store*
- `vintage-hash` — A landing row whose `content_hash_column` value equals that of its key's latest vintage lands no new vintage.
  *because a re-pulled unchanged window otherwise lands a false restatement*
- `vintage-validation` — A vintage table declaring no `content_hash_column` raises `StoreVintageHashUndeclared`, and one declaring {{store.declare.retain-rows}} raises `StoreVintageRetainRows`, at validation, naming the table.
  *because without a hash a re-pulled row reads as a restatement, and a wall-clock row-age cutoff shrinks a past answer*
- `vintage-dedup-key` — A vintage table reads through {{store.declare.dedup-view}}, or {{store.bound-time.interval-rows}} under `valid_time`, and folds under {{store.declare.dedup-view}} or {{store.fold.valid-time-line}}, each adding the availability column to the key and keeping every vintage; an unclustered `valid_time` fold sorts by `from`, then primary key.
  *A-store*
- `vintage-scope` — A vintage landing compares only its own keys' stored vintages. Landing skips row groups excluded by text or signed-integer key bloom filters or footer bounds on text, integer or temporal keys or temporal valid time.
  *A-store*
- `adjustment-factors` — `available_time.adjustment` keeps stored values raw; an `available_as_of` read multiplies each `adjust_columns` column by the product of `factor_column` vintages available by the bound, over factor rows sharing its factor-table `primary_key` values with a later valid-time `from`.
  *because applying a correction published later exposes information unavailable at an earlier read bound*

#### Scenarios

- `store.available-time.available-as-of`: WHEN a 2026 backfill lands a first-quarter measurement with availability date `2014-04-30`, THEN `available_as_of` `2014-05-01` returns the row and `available_as_of` `2014-04-29` returns none.
- `store.available-time.vintage-rows`: WHEN a first-quarter measurement lands at 17149.6 available `2014-04-30`, 17101.3 available `2014-05-29` and 17016.0 available `2014-06-25`, THEN `available_as_of` `2014-06-01` returns 17101.3 and an unbounded read returns 17016.0, before and after a fold.
- `store.available-time.vintage-rows`: WHEN those three measurement vintages have landed, THEN a read carrying `vintage = "first"` returns 17149.6.
- `store.available-time.vintage-hash`: WHEN a re-pull lands the measurement at 17016.0 with an unchanged content hash, THEN the table still holds three measurement vintages.
- `store.available-time.vintage-scope`: WHEN day 101 of 100 entities lands over 100 stored daily runs, THEN its comparison decodes no stored row; WHEN a revision of day 50 lands, THEN the comparison decodes only day 50's 100 rows.

unsettled: Does `vintage = "first"` count a same-day correction as a revision of the initial release? owner: store affects: store.available-time
