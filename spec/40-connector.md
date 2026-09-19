---
contract: connector
owns:
  - export
  - import
  - declare-capability
  - meter
  - infer
  - package
  - source
---

# Connectors and the built-in sources

A connector is the one way data enters the run path from outside. This file states the
interface a connector answers, the host access it declares, the quota it reserves against,
the model egress it reaches, the bytes it resolves from, and the behavior of the sources
compiled into the engine. How a credential reaches a request is in `spec/41-secrets.md`.

## export

| Clause | Statement | Why |
| --- | --- | --- |
| `connector.export.authoring-path` | Every connector implements one trait, as a component sandboxed in the host runtime or as a native sibling compiled into the binary. Dispatch carries no branch naming which of the two it calls. | D46 |
| `connector.export.type-taxonomy` | The shared types interface carries a schema (name, fields, primary key) and a field (name, data type, nullability) over `boolean`, `int32`, `int64`, `float64`, `string`, `bytes`, `timestamp-millis` and `json`. | — |
| `connector.export.batch-encoding` | A batch crosses the boundary as Arrow IPC bytes, and a position as opaque bytes beside a declared cursor kind. | — |
| `connector.export.nested-value` | A nested value rides as `json` and takes its type at the normalize stage. The interface carries no recursive type definition. | — |
| `connector.export.guaranteed-type` | A guest declares the type it guarantees after its own parsing — `int64` for a count, `float64` for money and a rate — and lands null, never zero, for a spelling it cannot read. | because a text column pushes a cast into every downstream read |
| `connector.export.read-handle` | A source exports its cursor kind, a discovery call, and an open call taking a table and an optional position. The handle's `next` yields one batch or exhaustion; its `position` reports where it stands. | — |
| `connector.export.write-handle` | A destination exports `prepare`, `write`, `commit` and `abort`. Source and destination are separate worlds sharing the types interface. | — |
| `connector.export.optional-world` | Operator configuration and failure attribution are each an interface in an additional world, probed on the instance by name. A base-world guest instantiates untouched, and neither addition moves the world version. | — |
| `connector.export.partition-attribution` | A guest exporting failure attribution answers with the byte-exact values of the outermost partition column a failed read belongs to. The host reads it on failure and success alike; an empty list keeps the unattributed refusal. | because a read holding one partition back and landing the rest returns success |
| `connector.export.attribution-budget` | Failure attribution carries at most 512 entries per session, each value at most 512 B. | — |
| `connector.export.native-read` | A native source reads a table from a position, returning batches each paired with the position after it. A source unable to bound a half-open chunk range falls back to the unbounded read. | because an overlapping read is recoverable and a mis-bounded chunk is not |
| `connector.export.native-extra` | A native source may offer a content fingerprint from a stat or a head request, never a download, plus its connector identity, audit entries, and a declared reason its vendor traffic runs unmetered. | — |
| `connector.export.store-blindness` | A source receives credentials, limiter bindings, the run id, the request ledger, the component pin and the incremental position, and no store handle. A store-driven run receives its query set as run parameters. | P5 |
| `connector.export.world-authorship` | Bindings are emitted from a host-owned template over the input specification and the connector name. An author writes method bodies alone: authentication, paging and the cursor-kind declaration. | — |

A connector failure crosses as a variant of {{run.retry.failure-taxonomy}}.

unsettled: How does a guest name the partitions a failed read belongs to when the failure is a shared credential rather than a per-tenant one? owner: connector affects: connector.export

## import

| Clause | Statement | Why |
| --- | --- | --- |
| `connector.import.three-imports` | Both worlds import outgoing HTTP, logging and a wall clock, and nothing else. There is no filesystem import and no socket import. | D46 |
| `connector.import.empty-context` | A guest standard library pulling broader interfaces links against an empty context: no preopens, no environment, no arguments. | — |
| `connector.import.wait-primitive` | Neither world carries a monotonic clock or a sleep primitive, so a guest holds no call open across a wait. | — |
| `connector.import.forwarded-config` | The pipeline's guest configuration table is the one part of the source configuration crossing inward, as one JSON object delivered once per session ahead of discovery. | D34 |
| `connector.import.config-shape` | Ahead of any I/O, a guest configuration value that is not a table, a serialization over 64 KiB, or a credential or environment reference inside it raises `ConnectorConfigRejected`. | D18 |
| `connector.import.config-unclaimed` | Where the artifact is local and probed, a guest table declared against a guest exporting no configuration interface raises `ConnectorConfigUnclaimed`. | P1 |
| `connector.import.unusable-key` | A guest raises a permanent failure for a configuration key or value it cannot act on. | P1 |
| `connector.import.config-hashing` | The forwarded table folds into the connector's content hash; with no table forwarded, the artifact digest is that hash verbatim. | — |

unsettled: How is a source's declared configuration key set enumerated, so an unknown key is answered at parse time rather than meaning the default? owner: connector affects: connector.import

## declare-capability

| Clause | Statement | Why |
| --- | --- | --- |
| `connector.declare-capability.declared-grant` | A manifest lists the host access its code reaches for — outbound hosts, environment names, the wall clock — and the host decides the grant at load. There is no run-time permission request and no escalation path. | D18 |
| `connector.declare-capability.undeclared-access` | A connector reaching for access its manifest does not list fails to load, raising `ConnectorUndeclaredAccess` naming that access. | D18 |
| `connector.declare-capability.host-allowlist` | The outbound allowlist holds exact hosts and subdomain wildcards matched as suffixes; a wildcard entry covers subdomains and not the apex. | — |
| `connector.declare-capability.allowlist-shape` | An empty allowlist, a bare wildcard, an empty entry, and an entry carrying a scheme, port or path raise `ConnectorAllowlistRejected`. | D18 |
| `connector.declare-capability.environment-name` | A declared environment name is a name, not ambient access: the operator binds it to a reference and the host resolves and injects the value at call time. No connector reads the process environment. | D20 |
| `connector.declare-capability.ambient-authority` | No credential is a property of the process: no shared client carries a default header and no import returns secret bytes. Every grant binds to one declared host and attaches at request time. | D46 |
| `connector.declare-capability.scope-probe` | A manifest may declare an identity endpoint, the response header carrying granted scopes, and the grant it expects. The host calls it with the bound credential ahead of the first read. | — |
| `connector.declare-capability.probe-transport` | The scope probe's host sits on the allowlist and its scheme is TLS or loopback. | D19 |
| `connector.declare-capability.scope-exceeded` | A granted scope outside the declared expectation raises `ConnectorScopeExceeded`, and the session does not open. | D18 |
| `connector.declare-capability.scope-unverified` | A probe response carrying no granted-scopes header raises `ConnectorScopeUnverified`. | because cannot-verify is not verified |
| `connector.declare-capability.environment-binding` | A source key binds from the environment as `<key>_from = "env:<NAME>"`. The engine enumerates the bindable keys in one place, each with its value shape: a scalar or a comma-separated list. | — |
| `connector.declare-capability.binding-unsupported` | A `_from` binding on a key the connector does not read raises `ConnectorBindingUnsupported`. | P3 |
| `connector.declare-capability.binding-unbound` | A bound key the environment did not supply, or supplied off its declared shape, raises `ConnectorBindingUnbound` at preflight. | P3 |

unsettled: Does a community-distributed connector need a signing and transparency layer above the content pin, and who runs the log? owner: connector affects: connector.declare-capability

## meter

| Clause | Statement | Why |
| --- | --- | --- |
| `connector.meter.limiter-declaration` | A connector declares a limiter capability naming the shared vendor quota, the traffic class of its requests, and the vendor quota-state response headers to forward. | — |
| `connector.meter.limiter-binding` | The operator binds that quota name to an endpoint, a bearer token held by reference, and a permit batch size. | — |
| `connector.meter.quota-unbound` | A declared quota with no binding raises `ConnectorQuotaUnbound` at load. | D18 |
| `connector.meter.binding-transport` | A limiter endpoint that is neither HTTPS nor loopback, and a limiter token written as an inline literal, raise `ConnectorLimiterBindingRejected` at load. | D18 |
| `connector.meter.reservation-point` | The host reserves at the mediation point, the guest's outgoing-HTTP import or the engine's shared client. One permit covers one outbound request. | because one call issues any number of requests for paging, retries and token refresh |
| `connector.meter.permit-batch` | An acquire may grant a batch of permits under a short TTL. Each request spends one, and the next report surrenders the unspent. | — |
| `connector.meter.acquire` | Acquire is an authenticated POST of quota, class and permit count, answered granted with permits and a TTL, or denied with a retry-after. A bare `429` with `Retry-After`, and a zero-permit grant, read as denials. | — |
| `connector.meter.unreadable-answer` | A limiter response the engine cannot read raises `ConnectorLimiterUnreadable` and grants nothing. | D18 |
| `connector.meter.report` | Report is an authenticated POST of quota, class, granted, spent, one entry per vendor response (status, retry-after, verbatim headers), an observation instant and the run id. | — |
| `connector.meter.report-delivery` | Report delivery is at-least-once under bounded backoff inside the run. A report that never lands is recorded in the run audit and fails nothing. | — |
| `connector.meter.unmetered-request` | Under a declared limiter, a vendor request with no granted reservation raises `ConnectorUnmetered`, including when the limiter is unreachable or unbound. | D18 |
| `connector.meter.allowlist-precedence` | A request the allowlist refuses never reaches the limiter and spends no permit. | — |
| `connector.meter.held-back-request` | A guest swallowing a held-back request still fails its call, and a fault inside the reservation machinery fails the request rather than passing it unmetered. | P2 |
| `connector.meter.synthesized-throttle` | A denied reservation returns to the guest as a synthesized `429` carrying the retry-after. An unreachable limiter fails the request as a transport failure. | because a connector mapping vendor throttles needs no second interface |
| `connector.meter.replay-reservation` | Replay issues no outbound request and makes no limiter call. | — |
| `connector.meter.built-in-grant-absent` | A compiled-in source reaching an outside vendor declares its grant in the pipeline's source configuration; the build raises `ConnectorGrantMissing` without it. | D18 |
| `connector.meter.grant-unhonorable` | A declared grant on a compiled-in source that bypasses the mediated client raises `ConnectorGrantUnhonorable` at load. | D18 |
| `connector.meter.metered-disclosure` | Where the project binds any quota, every compiled-in read records whether it ran metered, the run record states an unmetered read once, and validation warns per pipeline naming the connector. | — |
| `connector.meter.counting-not-pricing` | The limiter counts requests and prices none. | — |

## infer

| Clause | Statement | Why |
| --- | --- | --- |
| `connector.infer.model-endpoint` | Every model call resolves to one operator-configured OpenAI-compatible HTTP endpoint declared as a capability. The host owns the credential, rate limit and span; the component owns the prompt template and response schema. | D01 |
| `connector.infer.vendor-sdk` | A model-vendor SDK in any workspace crate is refused as {{topology.compose.vendor-sdk}}, so a provider swap is a URL edit. | D01 |
| `connector.infer.data-fence` | No ingested value reaches a model outside a data boundary: marker pairs whose opening marker carries the value's provenance label, a preamble declaring the blocks data, and a closing line restating the caller's rules. | — |
| `connector.infer.fence-is-not-a-boundary` | The fence lowers the success rate of injected instructions and bounds nothing. No clause treats the output of a fenced call as trusted. | because spotlighting reduces injection and guarantees nothing |
| `connector.infer.output-taint` | Model output carries the least-trusted provenance label among its fenced inputs, and lands under that label. | because output re-landing as fresh data launders injected text |
| `connector.infer.marker-derivation` | A marker carries a token derived from the fenced content itself. | because closing one's own fence then requires content containing its own digest |
| `connector.infer.fenced-value-hygiene` | Control characters are stripped from a fenced value, and a value past its declared character cap is truncated with a truncation mark. | — |
| `connector.infer.operator-template` | The operator's prompt template stays unfenced in the system role. The idempotency hash covers the template alone. | because hardening the fence leaves a dedup key intact |

The zone a call's declared locality maps onto belongs to the `authority` contract; a
placement-filtered read consults that mapping, not the endpoint URL.

## package

| Clause | Statement | Why |
| --- | --- | --- |
| `connector.package.distribution-form` | A connector resolves in one of four forms: in-tree by name, a project-local artifact path, an HTTPS URL, or an OCI reference. | — |
| `connector.package.remote-unpinned` | A remote artifact carrying no 64-hex content pin raises `ConnectorRemoteUnpinned` at parse. | D15 |
| `connector.package.insecure-artifact` | A plain-HTTP artifact reference raises `ConnectorInsecureArtifact`. | D19 |
| `connector.package.digest-mismatch` | The host re-hashes the resolved bytes and raises `ConnectorDigestMismatch` on a difference, before the bytes reach the engine. | D15 |
| `connector.package.pin-requirement` | Two switches require a pin on a local artifact, composed by disjunction: a store-wide policy key and a per-connector manifest flag. | — |
| `connector.package.local-unpinned` | With either switch set, an unpinned local artifact raises `ConnectorLocalUnpinned` at build, carrying the digest of the bytes found. | D15 |
| `connector.package.manifest-posture` | A manifest is adopted, carrying a digest built on the fixed builder platform, or a template, carrying the requirement plus a placeholder. A template parses cleanly and refuses at load. | — |
| `connector.package.posture-undeclared` | A manifest carrying neither a digest nor both template markers raises `ConnectorPostureUndeclared` at merge. | D15 |
| `connector.package.pin-verb` | `contextful connector pin <manifest>` builds the guest in a digest-pinned container on one fixed platform and writes the artifact and its digest. `--verify` rebuilds and compares without writing. | D15 |
| `connector.package.host-built-pin` | `--local` builds with the host toolchain and prints a host-local digest; writing a pin from a host build raises `ConnectorHostPinRefused`. | D15 |
| `connector.package.path-remap` | Every build remaps the source root and the package cache, so checkout location is not a digest input. | — |
| `connector.package.native-verification` | `--verify --native` checks host triple, compiler and package manager against pinned constants and clears byte-moving configuration before building. A mismatch exits with a status distinct from digest drift. | — |
| `connector.package.linear-memory` | A connector runs under 256 MiB of linear memory by default, raised per connector to at most 2 GiB. | — |
| `connector.package.call-deadline` | A read call carries a 30 s wall-clock deadline and a discovery call a 60 s one, armed by epoch interruption at 100 ms granularity. | — |
| `connector.package.session-budget` | A session carries a 1 MiB logging budget, dropping and counting messages past it, and holds at most 8 requests outbound at once. | — |
| `connector.package.world-compatibility` | Compatibility is semver over the world: a guest built against one minor loads on any host advertising a compatible minor. | — |
| `connector.package.world-drain` | On a major world bump no new run admits against the old world, in-flight runs complete or suspend on the world they pinned, and the old world then retires. | D15 |
| `connector.package.live-reresolution` | Re-resolving or reloading a connector while a journaled run holds it raises `ConnectorHotReload`; a new version applies to runs admitted after it. | D15 |
| `connector.package.built-in-registry` | The names resolving to compiled-in sources form one enumerated list. A listed name carries no manifest or artifact, and a feature-gated entry stays listed and answers with a rebuild hint. | — |

A connector striking a declared bound follows {{run.retry.bound-hit-is-transient}}.

```mermaid
flowchart TD
  NAME["connector name"] --> K{"on the built-in list?"}
  K -->|yes| NATIVE["compiled-in source"]
  K -->|no| FORM{"distribution form"}
  FORM -->|"https:// or oci://"| REMOTE{"64-hex pin?"}
  FORM -->|"http://"| IH["ConnectorInsecureArtifact"]
  FORM -->|local path| LOCAL{"pin switch set?"}
  REMOTE -->|no| RU["ConnectorRemoteUnpinned"]
  REMOTE -->|yes| HASH["re-hash bytes"]
  LOCAL -->|yes, no digest| LU["ConnectorLocalUnpinned"]
  LOCAL -->|no| HASH
  HASH -->|differs| DM["ConnectorDigestMismatch"]
  HASH -->|matches| INST["instantiate against the pinned world"]
```

unsettled: Which generation of the sandbox interface does the guest world target, and what does native async change about the per-call deadline and the reservation bridge? owner: connector affects: connector.package

## source

| Clause | Statement | Why |
| --- | --- | --- |
| `connector.source.http-headers` | The generic HTTP source binds credentials through a `headers` table whose values are templates in the {{connector.reference.value-template}} grammar, hydrated per read. | D20 |
| `connector.source.body-format` | `format` selects the decoder — `json` by default, `jsonl`, `csv` or a workbook — and one decoder serves the HTTP, file and object sources. Over HTTP the format is explicit; file sources infer it from the extension. | — |
| `connector.source.format-key-mismatch` | A JSON record path, a pagination shape, or a decode key declared against a format that does not read it raises `ConnectorFormatKeyRejected` at build. | P1 |
| `connector.source.delimited-cell` | A delimited source lands every cell as a string and an empty unquoted field as null. | because a dimension file's schema then depends on no data |
| `connector.source.declared-encoding` | A non-UTF-8 delimited body decodes through a declared encoding label, and bytes invalid under it raise `ConnectorEncodingInvalid`. | P1 |
| `connector.source.clock-column-spelling` | A delimited clock column other than an RFC 3339 instant or a fixed-width digit stamp raises `ConnectorClockColumnRejected`. | because the watermark compares text |
| `connector.source.pagination` | A walk declares exactly one pagination shape: a page parameter with a start page, a next-cursor path with a cursor parameter, a next-URL path, or link-header following. | — |
| `connector.source.pagination-ambiguity` | Declaring two pagination shapes raises `ConnectorPaginationAmbiguous`. | P1 |
| `connector.source.page-cap` | One walk issues at most 1000 requests. | — |
| `connector.source.page-loop` | A vendor returning a token or link it already served raises `ConnectorPageLoop`. | P4 |
| `connector.source.next-link-origin` | A vendor-supplied next link is judged as a hop under {{connector.attach.weakened-hop}}. | D19 |
| `connector.source.table-pattern` | A table pattern binds table-name segments into the request URL, percent-encoded, and each table holds its own position. | — |
| `connector.source.table-unmatched` | A table not matching the declared pattern raises `ConnectorTableUnmatched` ahead of any request. | P1 |
| `connector.source.placeholder-unbound` | A URL placeholder with no pattern to bind it raises `ConnectorPlaceholderUnbound` at build. | P1 |
| `connector.source.expansion` | An expansion block names its pointer as a URL template over the landed row's scalar columns, percent-encoded, or as a column holding a URL, and names the column the fetched document lands under. | — |
| `connector.source.pointer-ambiguity` | Declaring both pointer forms raises `ConnectorPointerAmbiguous`. | P1 |
| `connector.source.expansion-order` | Expansion runs after the watermark filter, and follow-ups are issued one at a time. | because a rejected row then costs no vendor request and no batch bursts one host |
| `connector.source.follow-up-failure` | A failed follow-up raises `ConnectorExpansionFailed` for the whole read. | because a position committed past a row missing its detail strands that row |
| `connector.source.expansion-budget` | One expanding read issues at most 200 requests of follow-up, judged before the first, buffers at most 256 MiB, and runs at most 600 s. Each budget refuses rather than truncating. | P4 |
| `connector.source.template-shape` | A pointer template carrying no placeholder, or binding one into the URL authority, raises `ConnectorTemplateRejected` at build. | because a response body then names a credential's destination |
| `connector.source.pointer-column-missing` | A placeholder naming a column the rows lack, or carry as an array, raises `ConnectorPointerColumnMissing` on the first row, ahead of its request. | P1 |
| `connector.source.target-column-occupied` | A target column any landed index row already carries raises `ConnectorTargetColumnOccupied` ahead of the walk. | P1 |
| `connector.source.walk-boundary` | A directory walk reaches the configured root and nothing outside it, skips dot-entries, and traverses in sorted order. A decode-selecting key narrows nothing. | because a sorted walk over an unchanged tree reproduces across hosts |
| `connector.source.declined-tally` | A directory walk records on the run record what it declined, tallied by extension. | P4 |
| `connector.source.document-grain` | A document source lands one row per page, or one row per heading past a length threshold. With a base URL, each row links to its own page anchor. | — |
| `connector.source.document-identity` | A document row's identity is a slug plus an ordinal, chunked or not. | because an identity then survives a document crossing the threshold |
| `connector.source.document-unreadable` | An encrypted document, and one with no extractable text on any page, is unreadable input under {{run.land.unreadable-input}} and never lands as empty pages. | P2 |
| `connector.source.frontmatter-shape` | A nested map, a block scalar, or a key carrying the reserved producer prefix in a note's frontmatter raises `ConnectorFrontmatterRejected`. | D16 |
| `connector.source.conversion-required` | A compound-binary office container, detected by magic bytes as well as extension, raises `ConnectorConversionRequired` naming the conversion command. | D16 |
| `connector.source.parse-containment` | A compiled-in source decodes behind {{run.land.parse-boundary}}. | D16 |
| `connector.source.office-part-selection` | An office container is read by exact part name. No other part is opened and no external entity is resolved. | D16 |
| `connector.source.decompression-budget` | A multi-part office read decompresses at most 64 MiB in total, judged against the archive directory's claim and against the bytes that arrive. | D16 |
| `connector.source.worksheet-landing` | One worksheet lands at most 64 MiB of resolved cell text, counted as cells land, and at most 1048576 rows. | because shared-string fan-out turns a small input into an unbounded output |
| `connector.source.cell-out-of-range` | A cell past the header's width raises `ConnectorCellOutOfRange`. | D16 |
| `connector.source.external-reference` | An external-reference declaration, an external-links part, or a relationship marked external raises `ConnectorExternalReference`. | D16 |
| `connector.source.workbook-cell-typing` | A workbook cell lands as a string and a date cell as its serial number. No formula is evaluated; a formula cell lands its cached value. | D16 |
| `connector.source.workbook-incremental` | An incremental position against a workbook raises `ConnectorIncrementalUnsupported`. | because a variable-width serial number advances a lexicographic watermark past unlanded rows |
| `connector.source.report-id-commit` | An async-report source commits the vendor's run id the instant creation returns it. An exhausted polling budget ends the read successfully, and an expired vendor run is recreated for the same window. | P4 |
| `connector.source.page-drain` | Report paging drains a fixed number of pages per read, one batch per page, each carrying the position after it. | — |
| `connector.source.page-cursor-missing` | A response announcing another page while naming its cursor in neither the cursor field nor the next link raises `ConnectorPageCursorMissing`. | P4 |
| `connector.source.report-watermark` | Report watermarks are held per partition — account, grain, breakdown and any split dimension — as a versioned map inside one position. | — |
| `connector.source.partition-isolation` | A retryable failure on one partition holds it at its committed position and keeps the pages other partitions drained. | P4 |
| `connector.source.metric-grain` | A metric read at a grain where the vendor estimates rather than computes it raises `ConnectorMetricGrainRejected`. | P4 |
| `connector.source.restated-window` | A caught-up partition re-opens its lookback window every run, the pull window is at least that wide, and each row carries a deterministic key over partition, entity, date and breakdown. | — |
| `connector.source.report-event-time` | A report row's event time is an RFC 3339 instant at midnight UTC of the window-start day, null where the vendor gave no usable date. | — |
| `connector.source.ordering-column` | Ordering a report stream by the engine-injected ingest stamp raises `ConnectorOrderingColumnRejected`. | because a late seed then outranks a live restatement |
| `connector.source.field-drift` | Report rows stay untyped JSON into the batch, and throttling is recognized from a throttle status or from an error body carrying a throttle code. | — |
| `connector.source.report-auth` | An authentication failure on a report read raises `ConnectorAuthTerminal`, terminal against the retry schedule. | P6 |
| `connector.source.image-row` | An image source lands path, content hash, header width and height, embedded capture instant, file modification time, a modality marker and a null body. No pixel decode runs on ingest. | — |
| `connector.source.image-header` | An image header that does not parse raises `ConnectorImageHeaderUnreadable` naming the path. | D16 |
| `connector.source.quotability-marker` | Whatever fills an image body records in a companion column whether the text is verbatim extraction or a model's reading. | — |
| `connector.source.marker-body-pairing` | An image body and its quotability marker that are not null together raise `ConnectorQuotabilityMismatch`. | D16 |
| `connector.source.search-config` | The search source takes a driver, a query, a freshness window and a result ceiling, landing one row per result. Its cursor kind is snapshot-id, keyed on the result URL. | — |
| `connector.source.provider-origin` | Each search provider's host is pinned at the call site; a non-TLS transport or another host raises `ConnectorProviderOriginRejected`, loopback excepted. | D19 |
| `connector.source.search-degradation` | An absent provider key and a provider `5xx` are transient failures yielding a zero-row outcome, and a replay returns the journaled result set. | — |
| `connector.source.poll-only-increments` | Incremental loading is a scheduled poll. A replication socket held open is not a source read. | because its output is unbounded and has no recorded exchange to replay |
| `connector.source.twin-api` | A source reading through an API licensed to see what no individual user sees raises `ConnectorTwinApiSource`. A sanctioned, disclosed organization-wide export path is admitted. | D37 |

## Shapes

A connector manifest in the adopted posture:

```toml
name    = "vendor-metrics"
version = "1.4.0"
world   = "source-connector@1.2.0"

wasm             = "vendor-metrics.wasm"
wasm_sha256      = "9f2c1b7ad0e4…"         # 64 hex, written by `connector pin`
require_wasm_pin = true

[capabilities]
allow_hosts = ["api.vendor.example", "*.cdn.vendor.example"]
env         = ["VENDOR_ACCOUNT_ID"]
clock       = true

[capabilities.scope_probe]
endpoint      = "https://api.vendor.example/v1/identity"
scopes_header = "X-Granted-Scopes"
expect        = ["reports.read", "accounts.read"]

[capabilities.limiter]
quota         = "vendor-app-shared"
class         = "batch-read"
usage_headers = ["X-RateLimit-Remaining", "X-RateLimit-Reset"]

[attach]
Authorization = "Bearer ${secret://vendor-token}"

[bounds]
memory_bytes = 268435456
```

The world:

```wit
package contextful:connector@1.2.0;

interface types {
  record schema       { name: string, fields: list<schema-field>, primary-key: list<string> }
  record schema-field { name: string, ty: data-type, nullable: bool }
  enum   data-type    { boolean, int32, int64, float64, string, bytes, timestamp-millis, json }
  record cursor       { kind: cursor-kind, bytes: list<u8> }
  variant error {
    transient(string), permanent(string), auth-expired(string),
    rate-limited(u32), schema-incompatible(string),
  }
}

world source-connector {
  import wasi:http/outgoing-handler;
  import wasi:logging/logging;
  import wasi:clocks/wall-clock;
  export source: interface {
    cursor-kind: func() -> cursor-kind;
    discover: func() -> result<list<schema>, error>;
    open: func(table: string, from: option<cursor>) -> result<read-handle, error>;
  }
}

world configurable-source-connector { include source-connector; export config: interface { … } }
world attributing-source-connector  { include source-connector; export attribution: interface { … } }
```

The limiter wire:

```http
POST /v1/acquire   {"quota":"vendor-app-shared","class":"batch-read","permits":32}
200                {"granted":true,"permits":32,"ttl_ms":15000}
429 Retry-After: 4 -> a denial

POST /v1/report    {"quota":"vendor-app-shared","class":"batch-read","granted":32,"spent":19,
                    "run_id":"run_5f1c","observed_at":"<instant>",
                    "responses":[{"status":200,"retry_after":null,"headers":{…}}]}
```

One outbound request, end to end:

```mermaid
flowchart LR
  G["guest source"] -->|outgoing-http| M["host mediation point"]
  N["compiled-in source"] --> M
  M --> A{"host on allowlist?"}
  A -->|no| R1["SecretUnpermittedRequest"]
  A -->|yes| D{"address public?"}
  D -->|no| R0["ConnectorPrivateAddress"]
  D -->|yes| L{"limiter declared?"}
  L -->|no| H["attach bound headers"]
  L -->|yes| Q["acquire permit"]
  Q -->|denied| R2["synthesized 429"]
  Q -->|unreachable| R3["ConnectorUnmetered"]
  Q -->|granted| H
  H --> S{"TLS or loopback?"}
  S -->|no| R4["SecretCleartextEndpoint"]
  S -->|yes| V["vendor, at the vetted address"]
  V --> P["origin check on every hop"]
```
