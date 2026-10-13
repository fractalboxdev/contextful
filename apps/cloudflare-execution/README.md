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
