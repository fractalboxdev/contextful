# Deployment runtimes

## Docker Desktop networking

- **Citation** — Docker, [Networking on Docker Desktop](https://docs.docker.com/desktop/features/networking/).
- **Priority** — must-read.
- **Informs** — `topology.deploy`.
- **Question** — Where do Linux containers execute on macOS, and how does a published port reach the host?

Docker Desktop places the Docker Engine in a Linux VM and forwards published ports to the host. The topology's macOS Docker example binds the Rust listener to the container's interfaces and publishes its port on macOS loopback. The shared project directory crosses Docker's file-sharing boundary.

Docker's [networking how-tos](https://docs.docker.com/desktop/features/networking/networking-how-tos/) define `host.docker.internal` for a container reaching a service on its host. The topology uses that name for Mac-hosted source and model endpoints.

## Multi-platform images

- **Citation** — Docker, [Multi-platform builds](https://docs.docker.com/build/building/multi-platform/).
- **Priority** — must-read.
- **Informs** — `topology.deploy`, `topology.package`.
- **Question** — Which image architecture matches the deployment CPU, and when does execution require emulation?

Linux containers share a Linux kernel and require a matching CPU architecture or emulation. The repository Dockerfile selects its builder and runtime platform through `PLATFORM` and verifies `RUST_TARGET` against the builder's Rust host. The topology's build examples pass both arguments alongside Docker's `--platform` selection.

## Basin Catalog and Basin SQL on R2

- **Citation** — Cloudflare, [Basin Catalog](https://developers.cloudflare.com/basin-catalog/), [Basin Catalog pricing](https://developers.cloudflare.com/basin-catalog/platform/pricing/), [Basin SQL pricing](https://developers.cloudflare.com/basin-sql/platform/pricing/) and [Containers pricing](https://developers.cloudflare.com/containers/pricing/).
- **Priority** — must-read.
- **Informs** — `run.export-iceberg`, `topology.deploy`.
- **Question** — What does a reader pay, in latency and money, to query an Iceberg export through Basin SQL instead of a Container query face?

Basin Catalog is an Iceberg REST catalog built into an R2 bucket. The bucket stores the
table's data and metadata files. The catalog tracks each table's current metadata
pointer, compacts small files, and expires snapshots on a schedule. Basin SQL is a
serverless query engine over that catalog, reached over REST with no Worker binding. A
catalog token covers a whole bucket, not one namespace.
That scope is why `run.export-iceberg.caller-sql` asks for one bucket per audience
when callers write their own SQL.

### Latency

The measurements below come from a routing Worker that timed 200 sequential Basin SQL
requests per cell, with every request returning 200. Tables used format v2 with
compaction disabled. The container figures are the `worker-container-d1` deployment
running on a `standard-1` instance. Both read paths ran the same four query shapes:

- **point:** a lookup on a sorted key.
- **range:** a one-day range.
- **groupby:** a full-table `GROUP BY`.
- **join:** a join to a 1,000-row dimension table.

Warm reads, p50 / p99 in ms:

| Query | Container, 1.27M rows | Basin, 10M rows in 40 files | Basin, 10M rows in 4,000 files |
| --- | ---: | ---: | ---: |
| point | 202 / 272 | 1,047 / 3,696 | 1,282 / 3,063 |
| range | 199 / 271 | 668 / 1,292 | 871 / 2,164 |
| groupby | 361 / 483 | 752 / 2,500 | 1,410 / 3,481 |
| join | 377 / 404 | 761 / 1,366 | 1,598 / 3,730 |

First read after idle or after an export:

| Path | First read |
| --- | --- |
| Container, 1 GB in 8,157 objects, past `sleepAfter` | 145.3 s; the Worker answers `503` with `Retry-After` until hydration ends |
| Container with lazy parts, 1 GB | 29 to 62 s |
| Basin, point lookup, 40 files | 3.6 s |
| Basin, groupby over 4,000 files | 24.4 s |

- **Floor:** Basin SQL's warm floor is about 0.7 s, even for a scan of 46 KB.
- **Throughput:** close to linear up to 32 concurrent clients, reaching about 21 queries a second for point lookups. The one-day range query levels off at 9.6 a second.
- **Export lag:** from source batch to a snapshot Basin SQL reads, an export takes 11.5 s for 51 MB and 39 to 54 s for 5.1 GB. The overwrite commit takes about 3 s at the catalog, whatever the file count.

### Cost

The rates are Workers Paid list prices.

| Item | Rate | Consequence |
| --- | --- | --- |
| Container memory, `standard-1` (4 GiB) | $0.0000025 per GiB-second | $26.06 a month always on |
| Container disk (8 GB) | $0.00000007 per GB-second | $1.42 a month always on |
| Container CPU | $0.000020 per vCPU-second, active time only | At most about $0.0000036 per full-table aggregate |
| Basin SQL scan | $2.50 per TB compressed, 10 MB minimum per query, 10 GB a month included | $25 per million queries at the minimum |
| R2 writes (Class A) | $4.50 per million | An hourly export of 4,000 files costs $13.1 a month, and one of 40 files $0.14 |
| R2 storage | $0.015 per GB-month | One full copy per retained snapshot |
| Basin Catalog compaction | $0.005 per GB processed | Wasted on a table each export replaces |

An always-on container costs about $27.5 a month whatever its query count. At the minimum
scan, Basin SQL matches that cost near 1.1 million queries a month. Below that, Basin
costs less and never refuses a read while it warms. Above it, the container costs less,
and its warm reads are 2 to 5 times faster.

Two items stay unknown:

- Whether Basin SQL's cached R2 reads bill as Class B operations.
- When Basin Catalog billing starts on every plan.

## DuckDB Iceberg REST catalogs

- **Citation** — DuckDB, [Iceberg REST catalogs](https://duckdb.org/docs/current/core_extensions/iceberg/iceberg_rest_catalogs); Cloudflare, [DuckDB with Basin Catalog, formerly R2 Data Catalog](https://developers.cloudflare.com/r2-data-catalog/config-examples/duckdb/).
- **Priority** — should-read.
- **Informs** — `run.export-iceberg`.
- **Question** — Which format version keeps an export readable by DuckDB?

DuckDB's `iceberg` extension attaches a REST catalog from a warehouse name, an endpoint
and a secret of type `ICEBERG` that holds the catalog token. Attached tables read like
any other DuckDB catalog. It reads format v2 tables. For a v3 table holding a nanosecond
timestamp, it fails with `Unrecognized primitive type: timestamptz_ns`. An export meant
for DuckDB readers therefore declares `format_version = 2`, and
`run.export-iceberg.type-refused` then refuses its nanosecond columns.
