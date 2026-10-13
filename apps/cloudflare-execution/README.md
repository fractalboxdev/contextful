# Cloudflare execution conformance

The Worker binds one disposable D1 database and R2 bucket to a Container. The Container's `contextful.d1` outbound host resolves SQL and blob requests through those bindings. `POST /conformance` runs the packaged `contextful-conformance` suite against the packaged `contextful-full` binary; all other HTTP routes return `404`.

The image context contains the exact Linux release binaries. Generate it from the release archive, its `.release.json` metadata, and its `.cloudflare-d1.compatibility.json` report:

```sh
cd apps/cloudflare-execution
npm run prepare-image -- <release.tar.gz> <release.json> <cloudflare-d1.compatibility.json> ../../.tmp/cloudflare-conformance-image
```

`prepare-image` verifies the archive SHA-256, binary digest, clean source revision, suite revision, full profile, and Linux target before it writes the image context. The generated directory is untracked.

An operator overlay based on `wrangler.example.jsonc` supplies a disposable D1 identifier, R2 bucket, account selection, and the six required secrets. The template keeps `workers_dev` disabled; the disposable test overlay enables its protected route. `CONFORMANCE_TOKEN` authenticates `POST /conformance`. The R2 endpoint, bucket, access key, secret key, and recovery identity pass into the Container process. The endpoint returns `compatible: true` only when every scenario passes, the suite exits successfully, and its digest matches the packaged release binary.

The passing provider report attaches to a new release output directory:

```sh
npm run attach-report -- <release.json> <provider-report.json> <attached-output-dir>
```

`attach-report` requires the release's exact scenario IDs, artifact digest, source revision and suite revision. It writes the provider report into the archive, updates the companion report and release metadata, and writes the archive's new SHA-256. The original release output remains intact.

## Whole-state recovery

A D1 export alone leaves journal blobs and the corpus behind. A recovery set contains a full D1 SQL export, every object in the execution blob bucket, and every object in the corpus bucket. The D1 export includes the recovery identity, schema, owner and cadence rows, journal references, awakeables, guards, export outboxes and checkpoints. Keep these three captures under one recovery generation.

1. Stop all writers, callback dispatchers, and export deliveries for the project. Wait for admitted callbacks to finish; preserve any guard left by an earlier crash for reconciliation after restore. Keep writers stopped until the replacement passes `contextful storage check`.
2. Export the full remote database, including schema and data, with `wrangler d1 export <database> --remote --output=<generation>/execution.sql`. Record its SHA-256 and the executable digest in the recovery set.
3. Copy the entire execution blob bucket and corpus bucket into separate directories in that recovery set. For an R2 S3 endpoint, `aws s3 sync s3://<execution-blob-bucket> <generation>/execution-blobs/ --endpoint-url <r2-endpoint>` and `aws s3 sync s3://<corpus-bucket> <generation>/corpus/ --endpoint-url <r2-endpoint>` capture the two object inventories. Record the object keys and SHA-256 of each downloaded object. An R2 object listing or D1 Time Travel bookmark by itself is not a whole-state recovery set.
4. Restore into an empty replacement D1 database and empty R2 buckets under the same project recovery identity. Upload each object directory with `aws s3 sync <generation>/<directory>/ s3://<replacement-bucket> --endpoint-url <r2-endpoint>` and verify its keys and content hashes. Then run `wrangler d1 execute <replacement-database> --remote --file=<generation>/execution.sql`. Keep the original recovery set immutable during this operation.
5. Start the replacement Container with the packaged executable and the restored bindings. `contextful storage check --project <project>` must pass before writers resume. Compare restored guard, outbox, checkpoint, and `CAST(started_ns AS TEXT)` values with the source database. Reconcile a retained guard from external receipt or idempotency evidence before its owner or lease transfers. Resume writers and export deliveries only after a recovery drill confirms the saved delivery identity and checkpoint continue without duplicate logical effects. The conformance suite uses separate disposable bindings because its setup clears execution rows.

The D1 export and import commands follow [Cloudflare's D1 import and export guide](https://developers.cloudflare.com/d1/best-practices/import-export-data/). That guide warns that large numeric values can lose precision; `contextful_run.started_ns` stores decimal text, and the recovery drill compares its exact digits. R2 copies use its [S3-compatible API](https://developers.cloudflare.com/r2/get-started/s3/). D1 Time Travel restores only the database and can overwrite its current state; it does not restore R2 objects ([Cloudflare Time Travel guide](https://developers.cloudflare.com/d1/reference/time-travel/)).
