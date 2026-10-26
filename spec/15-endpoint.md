---
contract: store
owns:
  - endpoint
---

# Bucket endpoints

The `[sync]` endpoint decides how each request reaches the bucket: its scheme, its region and the credentials that sign it.
The adapter speaks the S3 object API to AWS S3, R2 and compatible services, and reads a filesystem bucket directly.

## endpoint

Bucket endpoint schemes, transport, request addressing and signing, credentials and backend response handling.

- `schemes` — `[sync] endpoint` maps `file://<directory>` to a filesystem bucket and `s3://<region>` to regional AWS S3. `r2://<account-id>` selects that account's R2 endpoint at region `auto`. `https://<host>` uses `[sync] region`, default `us-east-1`.
- `unsupported-scheme` — Any other scheme, or an S3 or R2 endpoint in a build without the `s3-sync` feature, raises `SyncEndpointUnsupported`, naming the endpoint.
  *because an unreachable bucket refuses when it opens, before a push uploads anything*
- `plaintext` — An `http://` endpoint opens on a loopback host alone; any other host raises `SyncEndpointInsecure`, naming it.
  *because plaintext objects and signed requests are readable and replayable by anyone on the path*
- `addressing` — S3 and R2 address `<endpoint>/<bucket>/<key>` path-style, percent-encoding each key segment, and sign every request with AWS Signature Version 4 for service `s3` at the endpoint's region.
- `credentials` — `[sync] access_key_id`, `secret_access_key` and the optional `session_token` each bind `secret://<name>`, hydrated as {{connector.reference.whole-value-reference}}, or `env://NAME`, read whole from the process environment, as the bucket opens.
- `credential-unbound` — An S3 or R2 `[sync]` omitting `access_key_id` or `secret_access_key`, supplying a non-reference credential, or naming an unset variable raises `SyncCredentialUnbound`, naming the key.
  *because a literal key in `config.toml` sits in plaintext on every disk and backup of the store root*
- `role-credentials` — `[sync] credentials = "role"` on an `s3://` or `https://` endpoint signs with workload-role credentials in place of both keys, from the ECS container credential endpoint, then STS web identity, then IMDSv2.
  *because a task role or service account supplies short-lived keys, and regulated accounts refuse long-lived stored keys*
- `role-unavailable` — Under {{store.endpoint.role-credentials}}, a bucket open that no role source answers raises `SyncRoleUnavailable`, naming each source tried.
  *because an unsigned request otherwise fails as forbidden, hiding the missing role*
- `role-conflict` — `credentials = "role"` beside a key credential {{store.endpoint.credentials}} binds, or on an R2 endpoint, raises `SyncCredentialConflict`, naming the endpoint.
  *because two credential sources for one bucket leave unstated which identity signed a write*
- `role-refresh` — Role credentials refresh 300 s before their stated expiry and never sign once expired; a failed refresh keeps still-valid credentials and retries at the next request.
- `conditional-answers` — A `412` or `409` means a failed condition; a `404` naming `NoSuchKey` does so for an `If-Match` put. `501` means an unsupported method; `403` means forbidden credentials. Both follow {{store.probe.inconclusive}}.
- `missing-bucket` — A `404` naming a code other than `NoSuchKey`, `NoSuchBucket` included, is neither an absent object nor a failed condition: the request fails as transport, naming the code.
  *because a mistyped or deleted bucket otherwise reads as empty, a free lease or a lost compare-and-set*
- `transport` — An incomplete S3 get, head, list page, delete or unconditional put retries within 3 attempts. Connections allow 5 s; responses allow 2 s, 4 s, then 10 s. Conditional puts attempt once.
  *because a resent conditional put whose lost answer applied reads its own write as a failed condition, and a stalled request holds a pull*
- `throttled` — S3 requests retry within 5 attempts after `429` or `503`, or `500` except on conditional puts. Each retry waits a uniform draw below a ceiling doubling from 100 ms to 5 s.
  *because a throttled request never applied, a conditional put answered `500` may have, and jitter spreads throttled clients*
- `unavailable` — A request unanswered within {{store.endpoint.transport}}, or answered `429` or `5xx` past {{store.endpoint.throttled}}, fails as unavailable, naming the request and claiming nothing about the bucket's content.
- `list-pages` — A list follows continuation tokens until the backend reports completion, returning every key under the prefix in sorted order.
