---
contract: store
owns:
  - endpoint
---

# Bucket endpoints

A store mirrors its directory tree into one bucket, and the endpoint decides how each request reaches it.
Operators configure the endpoint under `[sync]`: its scheme, its region and the credentials that sign each request.
The adapter speaks the S3 object API to AWS S3, R2 and any compatible service, and reads a filesystem bucket directly.

## endpoint

Bucket endpoint schemes, transport, request addressing and signing, credentials and backend response handling.

- `schemes` — `[sync] endpoint` maps `file://<directory>` to a filesystem bucket and `s3://<region>` to regional AWS S3. `r2://<account-id>` selects that account's R2 endpoint at region `auto`. `https://<host>` uses `[sync] region`, default `us-east-1`.
- `unsupported-scheme` — Any other scheme, or an S3 or R2 endpoint in a build without the `s3-sync` feature, raises `SyncEndpointUnsupported`, naming the endpoint.
  *because a bucket the binary cannot reach refuses when it opens, before a push uploads anything*
- `plaintext` — An `http://` endpoint opens on a loopback host alone; any other host raises `SyncEndpointInsecure`, naming it.
  *because objects and signed requests crossing a network in plaintext are readable and replayable by anyone on the path*
- `addressing` — S3 and R2 use path-style `<endpoint>/<bucket>/<key>`, percent-encoding every key segment. Every request uses AWS Signature Version 4 for service `s3` at the endpoint's region.
- `credentials` — `[sync] access_key_id`, `secret_access_key` and the optional `session_token` each bind `secret://<name>`, hydrated as {{connector.reference.whole-value-reference}}, or `env://NAME`, read whole from the process environment, as the bucket opens.
- `credential-unbound` — S3 or R2 raises `SyncCredentialUnbound` when `[sync]` omits `access_key_id` or `secret_access_key`, supplies a non-reference credential, or names an unset variable. The error names the key.
  *because a literal key in `config.toml` sits in plaintext on every disk and backup holding the store root*
- `role-credentials` — `[sync] credentials = "role"` on an `s3://` or `https://` endpoint signs with workload-role credentials in place of both keys: the ECS container credential endpoint, then a web identity token exchanged through STS, then IMDSv2.
  *because a task role or service account supplies short-lived keys, and a long-lived key in a secret store is the credential posture a regulated account refuses*
- `role-unavailable` — Under {{store.endpoint.role-credentials}}, a bucket open that no role source answers raises `SyncRoleUnavailable`, naming each source tried.
  *because an unsigned request otherwise reaches the bucket and fails as forbidden, hiding the missing role*
- `role-conflict` — `credentials = "role"` beside a key credential {{store.endpoint.credentials}} binds, or on an R2 endpoint, raises `SyncCredentialConflict`, naming the endpoint.
  *because two credential sources for one bucket leave unstated which identity signed a write*
- `role-refresh` — Role credentials refresh 300 s before their stated expiry, and no request signs with expired credentials. A failed refresh keeps still-valid credentials and retries at the next request.
- `conditional-answers` — A `412` or `409` means a failed condition; a `404` naming `NoSuchKey` does so for an `If-Match` put. `501` means an unsupported method; `403` means forbidden credentials. Both follow {{store.probe.inconclusive}}.
- `missing-bucket` — A `404` naming a code other than `NoSuchKey`, including `NoSuchBucket`, indicates neither an absent object nor a failed condition. The get, put, delete or list fails as transport and names the code.
  *because a mistyped or deleted bucket otherwise reads as an empty bucket, a free lease or a lost compare-and-set*
- `transport` — An incomplete S3 get, head, list page, delete or unconditional put retries within 3 attempts. Connections allow 5 s; responses allow 2 s, 4 s, then 10 s. Conditional puts attempt once.
  *because a resent conditional put whose lost answer applied reads its own write back as a failed condition, and a stalled small request otherwise holds a pull for 10 s*
- `throttled` — S3 requests retry within 5 attempts after `429` or `503`, or `500` except on conditional puts. Each retry waits a uniform draw below a ceiling doubling from 100 ms to 5 s.
  *because a throttled request never applied, a conditional put answered `500` may have, and jitter spreads clients throttled together*
- `unavailable` — A request unanswered within {{store.endpoint.transport}}, or returning `429` or `5xx` beyond {{store.endpoint.throttled}}, fails as unavailable. The failure names the request without claiming what the bucket holds.
- `list-pages` — A list follows continuation tokens until the backend reports completion. The list returns every key under the prefix in sorted order.
