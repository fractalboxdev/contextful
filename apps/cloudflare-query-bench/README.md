# Cloudflare query-container bench

This bench measures how long a `worker-container-d1` query container takes to start, to hydrate its snapshot set from R2, and to answer reads. The Worker routes requests to one Container running `contextful serve --http`, the way the routing Worker in [`spec/targets.md`](../../spec/targets.md) does. Every run starts the Container on an empty disk, so each hydration pulls from the corpus bucket.

The Worker answers three `POST` routes. Each requires `Authorization: Bearer <BENCH_TOKEN>`, and any other request returns `404`:

| Route | Body | Answer |
| --- | --- | --- |
| `/bench/start` | `{"prefix": "<root>/<fixture>" \| null, "lazy_parts": bool}` | Destroys any running Container, then starts a new one; the clock starts here |
| `/bench/probe` | `{"until": "ready" \| "hydrated", "wait_ms": n}` | Waits up to 25 s for `/health` or `/health?ready=hydrated` to return `200`; returns `elapsed_ms` since start and, once hydrated, the pull and mapping figures from the serve log |
| `/bench/query` | One MCP JSON-RPC request; `Contextful-Authorization: Bearer <token>` | Forwards to `/mcp`, starting a stopped Container first; returns `503` with `Retry-After` while the Container warms |

A `null` prefix starts the Container with no `[sync]` block, which measures readiness on an empty disk. The Container sleeps once `QUERY_SLEEP_AFTER` passes without a request.

## Fixtures

`npm run fixtures` lands four stores and pushes each to its own prefix under `--prefix-root`:

| Fixture | Parquet bytes | Bucket objects |
| --- | --- | --- |
| `100mb-1k` | 100 MB | 1,000 |
| `100mb-8k` | 100 MB | 8,000 |
| `1gb-1k` | 1 GB | 1,000 |
| `1gb-8k` | 1 GB | 8,000 |

Each store holds `events` (`id`, `k`, `day`, `v`, `pad`) and the 1,000-row dimension table `dims` (`k`, `name`). `id` ascends across runs, `k` is `id % 1000`, and the runs spread in order over 100 `day` values, so all of a run's rows share one day. Each landed run adds two objects, a Parquet part and its run manifest. One push adds seven more: run state, the bucket and generation manifests, the `events` schema, and the `dims` run and schema. Folds add a few objects on top of these, so `fixtures.json` records the object count the push reported (1,017 for a 1k fixture). Rows carry 768 seeded random bytes as base64, so Parquet compresses them little. The builder lands a 2,000-row calibration run, measures its Parquet bytes per row, and sizes every run from that figure. All rows derive from seed `0x5eed0733`, so a rebuild reproduces them.

The 8k fixtures land about 4,000 runs each. Landing slows as the table's run count grows, so one 8k fixture takes tens of minutes to build. `--fixtures 100mb-1k,1gb-1k` builds a subset.

The builder writes `fixtures.json` and an issuer key pair to `--out`, which defaults to `../../.tmp/cloudflare-query-bench`. The Container pins `issuer/issuer.pub` as `ISSUER_PUBLIC_KEY`, and `measure` mints read tokens from the matching seed.

## Run against Cloudflare

1. Copy the `x86_64-unknown-linux-musl` `contextful-full` release binary to `image/contextful`.
2. Create an R2 bucket for the corpus and an R2 API token that can read and write it. The token's S3 credentials are its id (the access key id) and the SHA-256 of its value (the secret access key).
3. Build the fixtures from a machine holding that token:

   ```sh
   CORPUS_R2_ACCESS_KEY_ID=… CORPUS_R2_SECRET_ACCESS_KEY=… \
   CORPUS_R2_ENDPOINT=r2://<account-id> CORPUS_R2_BUCKET=<corpus-bucket> \
     npm run fixtures -- --prefix-root bench-<date>
   ```

4. Copy `wrangler.example.jsonc` into an operator overlay that names the account and a route. Set the six required secrets with `wrangler secret put`, and set `CORPUS_R2_ENDPOINT` to the same `r2://<account-id>` used in step 3. Then deploy with `wrangler deploy --config <overlay>`. The account needs Workers Paid with Containers enabled.
5. Measure:

   ```sh
   BENCH_TOKEN=… npm run measure -- --target cloudflare --url https://<bench-route> --sleep-after-secs 300
   ```

   Set `--sleep-after-secs` to the deployed `QUERY_SLEEP_AFTER`.

`measure` writes `results.json`, `results.md` (the issue's measurement table) and one record each for `container-cold-start-wall`, `container-hydrate-wall` and `container-first-read-after-idle` (see [`evals/ledger.toml`](../../evals/ledger.toml)). Records go under `records/`, or under `$CONTEXTFUL_MEASURE_DIR` when it is set. `--sections ready,hydrate,lazy,warm,idle` selects which measurements run. `--starts`, `--hydrate-runs` and `--warm` set the sample counts.

| Section | What `measure` does |
| --- | --- |
| `ready` | Times `--starts` empty-disk starts to the first `200` from `/health` |
| `hydrate` | Times each fixture to `/health?ready=hydrated` and reads the objects pulled and bytes mapped from the serve log |
| `lazy` | Runs the same starts under `[sync] lazy_parts`, then one aggregate. Per-part fetch time is (first aggregate − warm aggregate) ÷ the files a full hydration of that fixture mapped |
| `warm` | On `1gb-1k`, takes `--warm` samples of four query shapes, each timed end to end through the Worker: a point lookup on `id`, one `day`, a GROUP BY `k` over every row, and a join to `dims` grouped by `name` |
| `idle` | On `1gb-8k`, hydrates and reads once, sends nothing for `sleepAfter` + 30 s, then times a read through the Worker until it returns `200`. A `503` is retried after `Retry-After`, capped at 1 s |

The GROUP BY and the join check that their counts sum to the fixture's row count. A read that misses rows fails the run with `BenchReadIncomplete`.

## Loopback check

`--target local` runs the same sections against `contextful serve` on loopback, reading fixtures from a `file://` bucket. Going idle there stops the process.

```sh
npm run fixtures -- --endpoint file://$PWD/../../.tmp/bucket --bucket corpus --scale 0.01
npm run measure -- --target local --starts 5 --hydrate-runs 1 --warm 10
```

`--scale` shrinks a fixture's bytes and keeps its object count.
