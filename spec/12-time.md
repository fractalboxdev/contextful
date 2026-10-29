---
contract: store
owns:
  - bound-time
  - available-time
---

# Time bounds

The engine stamps transaction time at commit; a table declaration names its valid-time and availability-time columns.
Each bound resolves before access controls select rows.

## bound-time

Transaction-time and valid-time columns, their read parameters and the snapshots or rows each parameter selects.

- `two-clocks` — Transaction time is `_ingested_at`; valid time is a declared pair of table columns, which the engine never infers, and it stamps no second transaction timestamp.
- `valid-time-declaration` — `[pipeline.tables.valid_time]` declares `from` and an optional `to`; a table declaring `from` alone treats each row as valid from that instant onward.
- `valid-time-type` — A declared valid-time column whose type is not a timestamp raises `StoreValidTimeNotTimestamp` at declaration.
  *P1*
- `instant-comparison` — Every bound compares instants as timestamps, not strings; a date-only literal resolves to the start of the next day, exclusive.
- `as-of` — Inside each table's FROM-source, `as_of` selects the newest reachable snapshot created at or before the bound plus the committed runs at or before it that the snapshot omits.
  *because filtering current files by ingest stamp returns different rows before and after a fold*
- `row-age-cutoff` — A read of a table with {{store.declare.retain-rows}} excludes rows older than its wall-clock cutoff, `as_of` reads of retained snapshots included, and the result cache reuses no such statement.
  *because a historical snapshot does not restore a row after its declared age passes*
- `as-of-unretained` — An `as_of` before the oldest snapshot retention kept, or selecting a snapshot {{store.fold.collected-snapshot}} counts or whose successors fold a run absent here, raises `StoreAsOfUnretained`, naming the oldest answerable instant.
  *P4*
- `valid-as-of` — `valid_as_of` filters the same inner source with `from <= valid_as_of AND (to IS NULL OR to > valid_as_of)` over the declared pair.
- `valid-time-undeclared` — A `valid_as_of` read against a table declaring no pair raises `StoreValidTimeUndeclared`, naming the table.
  *P1*
- `pin-bound` — A read carrying `as_of` and a build pin resolves each table named by both by {{read.resolve-pin.earlier-bound-wins}}.
- `unbounded-latest` — A read carrying neither bound resolves each table to its current snapshot plus the committed runs it omits.
- `beneath-enforcement` — Both bounds apply before the row predicate, column mask and tenant filter, so they only narrow the rows access controls admit.
  *P5*
- `covering-versions` — For an unkeyed table, a valid-time bound returns every version whose interval contains the requested instant.
- `interval-rows` — A keyed table declaring `valid_time` stores one row per key and valid-time interval, deduplicating on its primary key and `from`, so a fold keeps every interval.
  *A-store#validity-interval-row*
- `echo` — A bounded read returns `contextful.bounds` as `{as_of?, valid_as_of?, inclusive}`, each instant RFC 3339 UTC with nine fractional digits and a `Z`; an unbounded read omits it.

unsettled: Does an `as_of` read selecting a held snapshot older than a collected one read that snapshot, rather than refuse under {{store.bound-time.as-of-unretained}}? owner: store affects: store.bound-time

## available-time

The source availability timestamp, its value at landing and row selection under `available_as_of`.

- `declaration` — `[pipeline.tables.available_time]` declares {{type:contextful_core::store::declare::AvailableTime}}, omitted from canonical serialization when unset. `column` holds the source availability instant as `timestamptz`, which landing keeps and availability bounds compare alone; `zone` defaults to UTC.
  *A-store#availability-declared-column*
- `lag-fill` — At landing, `available_time.lag` fills a null availability column with valid-time `to`, else `from`, plus the duration `lag.classes` maps the row's `lag.class_column` value to, and injects `_available_imputed` true; a class without an entry leaves the instant null.
  *because a source can publish different record classes after different declared delays*
- `live-since` — A date-typed availability value resolves at landing to the start of the next day in the declared `zone`; rows available before `available_time.live_since`, when the source began publishing live, get injected `_available_reconstructed` true.
  *because a date gives no publication time within its day*
- `available-as-of` — `available_as_of` filters the inner source with `<column> <= available_as_of` under {{store.bound-time.beneath-enforcement}}, combining with {{store.bound-time.as-of}} and {{store.bound-time.valid-as-of}} when the read supplies them.
  *A-store#availability-declared-column, P5*
- `bound-echo` — A read carrying `available_as_of` adds it, in the same instant form, to the object {{store.bound-time.echo}} returns.
- `undeclared` — An `available_as_of` read of a table declaring no `available_time` raises `StoreAvailableTimeUndeclared`, naming the table; a landing row resolving to a null instant raises `StoreAvailableTimeNull`, naming the table and run.
  *because a missing availability instant either hides a backfill or leaks it to every bound*
- `vintage-rows` — A keyed table declaring `available_time` is a vintage table, keeping every vintage per primary key, valid-time `from` and availability instant; a read returns the latest at or before any `available_as_of`, or under `vintage = "first"` the earliest.
  *A-store#restatement-vintage-row, A-store#adjustment-factors-apply, A-read#join-matches-availability*
- `vintage-hash` — A landing row whose `content_hash_column` value equals that of its key's latest vintage lands no new vintage.
  *because a re-pulled unchanged window otherwise lands a false restatement*
- `vintage-validation` — A vintage table declaring no `content_hash_column` raises `StoreVintageHashUndeclared`, and one declaring {{store.declare.retain-rows}} raises `StoreVintageRetainRows`, at validation, naming the table.
  *because a re-pull without a hash reads as a restatement, and a row-age cutoff shrinks a past answer*
- `vintage-dedup-key` — A vintage table reads through {{store.declare.dedup-view}}, or {{store.bound-time.interval-rows}} under `valid_time`, and folds under {{store.declare.dedup-view}} or {{store.fold.valid-time-line}}, each adding the availability column to the key and keeping every vintage; an unclustered `valid_time` fold sorts by `from`, then primary key.
  *A-store#restatement-vintage-row, A-store#validity-interval-row, A-store#adjustment-factors-apply*
- `vintage-scope` — A vintage landing compares only its own keys' stored vintages, skipping row groups that text or signed-integer key bloom filters, or footer bounds on text, integer or temporal keys or temporal valid time, exclude.
  *A-store#vintage-landing-reads*
- `adjustment-factors` — `available_time.adjustment` keeps stored values raw; an `available_as_of` read multiplies each `adjust_columns` column by the product of `factor_column` vintages available by the bound, over factor rows sharing its factor-table `primary_key` values with a later valid-time `from`.
  *because applying a correction published later exposes information unavailable at an earlier read bound*

#### Scenarios

- `store.available-time.available-as-of`: WHEN a 2026 backfill lands a first-quarter measurement with availability date `2014-04-30`, THEN `available_as_of` `2014-05-01` returns the row and `available_as_of` `2014-04-29` returns none.
- `store.available-time.vintage-rows`: WHEN a first-quarter measurement lands at 17149.6 available `2014-04-30`, 17101.3 available `2014-05-29` and 17016.0 available `2014-06-25`, THEN `available_as_of` `2014-06-01` returns 17101.3, an unbounded read returns 17016.0 before and after a fold, and `vintage = "first"` returns 17149.6.
- `store.available-time.vintage-hash`: WHEN a re-pull lands the measurement at 17016.0 with an unchanged content hash, THEN the table still holds three measurement vintages.
- `store.available-time.vintage-scope`: WHEN day 101 of 100 entities lands over 100 stored daily runs, THEN its comparison decodes no stored row; WHEN a revision of day 50 lands, THEN the comparison decodes only day 50's 100 rows.

unsettled: Does `vintage = "first"` count a same-day correction as a revision of the initial release? owner: store affects: store.available-time
