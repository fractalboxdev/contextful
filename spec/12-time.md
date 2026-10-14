---
contract: store
owns:
  - bound-time
  - available-time
---

# Time bounds

A row carries three clocks: the transaction time the engine stamps at commit, and the valid
time and availability time its table declares. Each clock has a read bound, and every bound
narrows what enforcement admits.

## bound-time

The two clocks a row carries, the parameter bounding each, and what a bounded read resolves to.

- `two-clocks` — Transaction time is `_ingested_at`; valid time is a declared pair of the table's own columns. The engine infers no pair and stamps no second transaction clock.
- `valid-time-declaration` — `[pipeline.tables.valid_time]` declares `from` and an optional `to`; a table declaring `from` alone treats each row as valid from that instant onward.
- `valid-time-type` — A declared valid-time column whose type is not a timestamp raises `StoreValidTimeNotTimestamp` at declaration.
  *P1*
- `instant-comparison` — Every bound compares instants as timestamps, never as strings; a date-only literal resolves, where it is built, to the start of the next day, exclusive.
- `as-of` — `as_of` resolves each table to the newest reachable snapshot created at or before it, plus the committed runs at or before it that snapshot omits, inside the table's FROM-source.
  *because filtering the current files by ingest stamp returns different rows before and after a fold*
- `row-age-cutoff` — A statement reading a table with {{store.declare.retain-rows}} excludes rows older than its wall-clock cutoff, including an `as_of` read of a retained older snapshot; the result cache reuses no such statement.
  *because a historical snapshot does not restore a row after its declared age passes*
- `as-of-unretained` — An `as_of` earlier than the oldest retained snapshot of a table whose history has been collected raises `StoreAsOfUnretained`, naming the oldest answerable instant.
  *P4*
- `valid-as-of` — `valid_as_of` wraps the same inner source with `from <= valid_as_of AND (to IS NULL OR to > valid_as_of)` over the declared pair.
- `valid-time-undeclared` — A `valid_as_of` read against a table declaring no pair raises `StoreValidTimeUndeclared`, naming the table.
  *P1*
- `pin-bound` — A read carrying `as_of` and a build pin resolves each table named by both by {{read.resolve-pin.earlier-bound-wins}}.
- `unbounded-latest` — A read carrying neither bound resolves each table to its current snapshot plus the committed runs it omits.
- `beneath-enforcement` — Both bounds apply beneath the row predicate, the column mask and the tenant filter, narrowing what enforcement admits and widening nothing.
  *P5*
- `covering-versions` — Over an unkeyed table a valid-time bound returns every version whose interval covers the instant.
- `interval-rows` — A key valid over several intervals holds one row per interval: a keyed table declaring `valid_time` dedupes on its primary key and `from`, so a fold keeps every interval.
  *A-store*
- `echo` — A bounded read echoes `contextful.bounds` as `{as_of?, valid_as_of?, inclusive}`, each instant in RFC 3339 UTC with nine fractional digits and a `Z` suffix; an unbounded read omits it.

## available-time

Availability time: the declared instant each row's value became public, how landing resolves it, and the `available_as_of` bound a read compares against it.

- `declaration` — `[pipeline.tables.available_time]` declares `column`, the `timestamptz` instant a row's value became public, and an optional `zone`, UTC by default; landing writes the instant resolved from the source, never an engine stamp, and an availability bound compares that column alone.
  *A-store*
- `lag-fill` — A declared `available_time.lag` fills a null column at landing with valid-time `to`, else `from`, plus the duration {{store.available-time.lag-class}} assigns the row, and the engine injects `_available_imputed` true on that row.
  *because a filing becomes public a filer-class deadline after period end, 40 to 90 days for a 10-Q or 10-K*
- `lag-class` — `available_time.lag = { class_column = "<column>", classes = { <class> = "<n>d" } }` maps each row's class to a duration; a row whose class names no entry resolves no instant and refuses as {{store.available-time.null}}.
- `date-end` — A date-typed availability value resolves at landing to the start of the following day in the declared `zone`.
  *because a date-only filing or headline can land after the close*
- `live-since` — `available_time.live_since` names the instant a source began publishing live; the engine injects `_available_reconstructed` true on each row available before it.
  *because a vendor's point-in-time history before its live date was rebuilt after the fact*
- `available-as-of` — `available_as_of` wraps the inner source with `<column> <= available_as_of` over the declared column, conjoined with {{store.bound-time.as-of}} and {{store.bound-time.valid-as-of}} where given.
  *A-store*
- `table-key` — Beyond {{store.declare.table-block}}, a table block may declare `available_time`, absent from the canonical serialization when unset.
- `beneath-enforcement` — The `available_as_of` bound applies beneath enforcement as {{store.bound-time.beneath-enforcement}} applies the other two, narrowing what enforcement admits and widening nothing.
  *P5*
- `bound-echo` — A read carrying `available_as_of` adds `available_as_of` to the object {{store.bound-time.echo}} echoes, in the same instant form.
- `undeclared` — An `available_as_of` read naming a table that declares no `available_time` raises `StoreAvailableTimeUndeclared`, naming the table.
  *because falling back to `_ingested_at` hides a backfill and falling back to nothing leaks it*
- `null` — Landing a row whose resolved availability instant is null raises `StoreAvailableTimeNull`, naming the table and the run.
  *because a row without an availability instant is visible to every bound or to none, and either misstates what was public*
- `vintage-rows` — A keyed table declaring `available_time` is a vintage table: it keeps one row per primary key, valid-time `from` where declared, and availability instant, so a restatement lands beside the value it revises.
  *A-store*
- `vintage-resolution` — A read of a vintage table returns, per primary key and valid-time `from`, the vintage with the latest availability instant at or before `available_as_of`, or the latest of all when the read carries no availability bound.
  *A-store*
- `vintage-first` — A read carrying `vintage = "first"` returns, per primary key and valid-time `from`, the vintage with the earliest availability instant, within `available_as_of` where given.
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
- `vintage-fold-key` — A fold over a vintage table partitions as {{store.fold.valid-time-line}} or {{store.declare.dedup-view}} prescribes with the availability column added, and drops no vintage.
  *A-store*
- `adjustment-factors` — `available_time.adjustment = { table = "<name>", factor_column = "<column>" }` keeps a price table's values raw; an `available_as_of` read applies only factor vintages of that table available at or before the bound.
  *because a back-adjusted 2014 price encodes splits announced after 2014*

#### Scenarios

- `store.available-time.available-as-of`: WHEN a 2026 backfill lands 2014Q1 GDP with availability date `2014-04-30`, THEN `available_as_of` `2014-05-01` returns the row and `available_as_of` `2014-04-29` returns none.
- `store.available-time.vintage-resolution`: WHEN 2014Q1 GDP lands at 17149.6 available `2014-04-30`, 17101.3 available `2014-05-29` and 17016.0 available `2014-06-25`, THEN `available_as_of` `2014-06-01` returns 17101.3 and an unbounded read returns 17016.0, before and after a fold.
- `store.available-time.vintage-first`: WHEN the same three 2014Q1 GDP vintages have landed, THEN a read carrying `vintage = "first"` returns 17149.6.
- `store.available-time.vintage-hash`: WHEN a re-pull lands 2014Q1 GDP at 17016.0 with an unchanged content hash, THEN the table still holds three 2014Q1 vintages.

unsettled: Does `vintage = "first"` count a same-day correction as a revision of the initial release? owner: store affects: store.available-time
