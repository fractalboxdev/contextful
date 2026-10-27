# Cloudflare execution conformance

The Worker binds one disposable D1 database and an R2 blob bucket to a Container; a second R2 bucket holds corpus objects. The Container's `contextful.d1` outbound host resolves SQL and blob requests through the Worker bindings, while the corpus uses the R2 S3 endpoint. `POST /conformance` runs the packaged `contextful-conformance` suite against the packaged `contextful-full` binary; all other HTTP routes return `404`. A suite process outlives a Durable Object restart, so a run refuses with `ConformanceAlreadyRunning` while another `contextful-conformance` process lives in the Container, then ends every `contextful` process an earlier run left driving the bindings before it starts.

The image context contains the exact Linux release binaries. Generate it from the release archive, its `.release.json` metadata, and its `.cloudflare-d1.compatibility.json` report:

```sh
cd apps/cloudflare-execution
npm run prepare-image -- <release.tar.gz> <release.json> <cloudflare-d1.compatibility.json> ../../.tmp/cloudflare-conformance-image
```

`prepare-image` verifies the archive SHA-256, binary digest, clean source revision, suite revision, full profile, and Linux target before it writes the image context. The generated directory is untracked.

An operator overlay based on `wrangler.example.jsonc` supplies a disposable D1 identifier, R2 bucket, account selection, and the six required secrets. The template keeps `workers_dev` disabled; the disposable test overlay enables its protected route. `CONFORMANCE_TOKEN` authenticates `POST /conformance`. The R2 endpoint, bucket, access key, secret key, and recovery identity pass into the Container process. The endpoint returns `compatible: true` only when every scenario passes, the suite exits successfully, and its digest matches the packaged release binary.

The Worker derives the conformance Durable Object name from those five Container bindings. Changing an R2 binding or the recovery identity selects a fresh instance on the next request, so a running Container cannot keep serving the prior secret values. The D1 admission lease prevents two instances from resetting the disposable database at the same time. A deployment that changes only the image keeps the same instance name; stop the running Container before checking a replacement image.

The corpus endpoint uses `r2://<account-id>` so storage preflight identifies the R2 binding; the S3 adapter resolves it to the account's HTTPS endpoint. The [provider report](evidence/cloudflare-d1-provider.json) records the required outcomes of its suite revision for the source-pinned release binary; a release attaches only a report whose suite revision and scenario IDs match the packaged suite.

The passing provider report attaches to a new release output directory:

```sh
npm run attach-report -- <release.json> <provider-report.json> <attached-output-dir>
```

`attach-report` requires the release's exact scenario IDs, artifact digest, source revision and suite revision. It writes the provider report into the archive, updates the companion report and release metadata, and writes the archive's new SHA-256. The original release output remains intact.

## Atomicity boundaries

D1 commits metadata; R2 holds content. Each boundary below is one D1 batch, and an R2 object a row names is written before that batch commits.

| State | One D1 commit covers | R2 holds |
| --- | --- | --- |
| Typed export | One export's outbox, acknowledgement sequence, promoted keyed state and checkpoint, in one row of at most 1.9 MB | Nothing |
| Coordinated group | One namespace's sender row — pending group, delivery offset, promoted state — plus the active namespace on completion, under the publication lease and prior row version | A sender body over 1 MiB, by SHA-256 |
| Producer completion | The group's latest producer frontier and its per-run history row | A frontier over 1 MiB, by SHA-256 |

A coordinated group's sender state stays at or below 8 MiB; a larger state refuses with `PublicationStorageUnsupported` before staging. A receiver activates a group only at its `group_complete` event, so a crash between staging and delivery, or between the receiver's commit and the sender's acknowledgement, leaves the receiver's active generation whole; the replacement reoffers the identical group and event identifiers from D1. The blob sweep keeps every R2 object a publication row names, and `contextful storage check` verifies them with the other execution blobs.

## Whole-state recovery

A D1 export alone leaves journal blobs and the corpus behind. A recovery set contains a full D1 SQL export, every object in the execution blob bucket, and every object in the corpus bucket. The D1 export includes the recovery identity, schema, owner and cadence rows, journal references, awakeables, guards, export outboxes and checkpoints, and coordinated publication rows. Keep these three captures under one recovery generation.

1. Stop all writers, callback dispatchers, and export deliveries for the project. Wait for admitted callbacks to finish; preserve any guard left by an earlier crash for reconciliation after restore. Keep writers stopped until the replacement passes `contextful storage check`.
2. Run `contextful storage capture --project <project>` while writers remain stopped. It hashes every corpus object under the project's sync prefix and commits that frontier to the D1 recovery identity. Export the full remote database, including schema and data, with `wrangler d1 export <database> --remote --output=<generation>/execution.sql`. Record its SHA-256 and the executable digest in the recovery set.
3. Copy the entire execution blob bucket and corpus bucket into separate directories in that recovery set. For an R2 S3 endpoint, `aws s3 sync s3://<execution-blob-bucket> <generation>/execution-blobs/ --endpoint-url <r2-endpoint>` and `aws s3 sync s3://<corpus-bucket> <generation>/corpus/ --endpoint-url <r2-endpoint>` capture the two object inventories. Record the object keys and SHA-256 of each downloaded object. An R2 object listing or D1 Time Travel bookmark by itself is not a whole-state recovery set.
4. Restore into an empty replacement D1 database and empty R2 buckets under the same project recovery identity. Upload each object directory with `aws s3 sync <generation>/<directory>/ s3://<replacement-bucket> --endpoint-url <r2-endpoint>` and verify its keys and content hashes. Then run `wrangler d1 execute <replacement-database> --remote --file=<generation>/execution.sql`. Keep the original recovery set immutable during this operation.
5. Start the replacement Container with the packaged executable and the restored bindings. `contextful storage check --project <project>` verifies every recorded execution blob and hashes the captured corpus objects before writers resume; routine execution preflight checks their keys through an R2 listing. Compare restored guard, outbox, checkpoint, publication, and `CAST(started_ns AS TEXT)` values with the source database. Reconcile a retained guard from external receipt or idempotency evidence before its owner or lease transfers. Resume writers and export deliveries only after a recovery drill confirms the saved delivery identity and checkpoint continue without duplicate logical effects. The conformance suite uses separate disposable bindings because its setup clears execution rows.

The D1 export and import commands follow [Cloudflare's D1 import and export guide](https://developers.cloudflare.com/d1/best-practices/import-export-data/). That guide warns that large numeric values can lose precision; `contextful_run.started_ns` stores decimal text, and the recovery drill compares its exact digits. R2 copies use its [S3-compatible API](https://developers.cloudflare.com/r2/get-started/s3/). D1 Time Travel restores only the database and can overwrite its current state; it does not restore R2 objects ([Cloudflare Time Travel guide](https://developers.cloudflare.com/d1/reference/time-travel/)).
