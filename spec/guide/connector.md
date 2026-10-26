---
contract: connector
---

# Connectors and credentials

## What it is for

A **vendor** is an external service provider, such as GitHub. A **connector** reads its
data and tracks where reads resume. The **host** runs connectors, checks destinations,
reserves quota, and attaches credentials
({{connector.export.store-blindness}}, {{connector.attach.mediation-covers-every-egress}}).

## How it works

A **sandboxed component** is a **WebAssembly (Wasm) component** implementing the
host-owned connector interface; compiled-in connectors implement the same interface
({{connector.export.authoring-path}}, {{connector.export.world-authorship}}). The component
runs under memory limits and per-call deadlines ({{connector.package.linear-memory}},
{{connector.package.call-deadline}}) and reaches the network only through the host.
Its only imports are outgoing HTTP, logging and a wall clock
({{connector.import.three-imports}}); connectors receive no store handle
({{connector.export.store-blindness}}). Data batches cross the interface as **Arrow IPC**,
a binary format for columnar data ({{connector.export.batch-encoding}}).

Connectors resolve from four distribution forms ({{connector.package.distribution-form}}). A
remote artifact carries a content pin ({{connector.package.remote-unpinned}}), the host
re-hashes the bytes ({{connector.package.digest-mismatch}}), reuses compatible host output ({{connector.package.artifact-cache}}), and runs retain
admitted builds ({{run.own.admission-pin}}).
HTTPS and OCI downloads stay within the reference's host and port
({{connector.package.remote-transport}}).
An OCI reference selects one component layer ({{connector.package.oci-component-layer}}),
checks its descriptor ({{connector.package.oci-layer-integrity}}), and optionally uses a
project-bound registry bearer ({{connector.package.oci-registry-bearer}}).
Admitted bytes enter a digest-keyed project cache ({{connector.package.remote-cache}});
damaged cached bytes refuse ({{connector.package.remote-cache-corrupt}}).

A manifest declares keys, an artifact pin and one build input
({{connector.package.manifest-keys}}, {{connector.package.manifest-pin}},
{{connector.package.build-source}}). `connector pin` writes the pin from the fixed platform
({{connector.package.pin-verb}}); `--local` writes nothing ({{connector.package.host-built-pin}}),
and `--verify` reproduces ({{connector.package.path-remap}}).

The manifest declares hosts and clock access; the host grants access at load ({{connector.declare-capability.declared-grant}});
code reaching past it fails to load ({{connector.declare-capability.undeclared-access}}). An
optional scope probe checks the bound credential's grant before the first read
({{connector.declare-capability.scope-probe}}).

A reload compares complete declarations ({{connector.widen.comparison}}). Narrowing needs no
widening review ({{connector.widen.narrowing}}); an operator approval of additional access
is bound to both manifests ({{connector.widen.approval-binding}}). A host witness is a request both matchers replay
({{connector.widen.host-witness}}).

Every outbound request uses the host's mediated client
({{connector.attach.mediation-covers-every-egress}}); an exec child alone holds its credential
({{connector.attach.exec-environment}}). Each hop passes the allowlist and
one pre-send hook ({{connector.meter.pre-send-hook}}) before host resolution
({{connector.attach.resolve-half}}). The transport connects to the vetted address
({{connector.attach.resolve-once}}), refuses internal ranges
({{connector.attach.private-address}}), and attaches credentials only to the admitted
destination ({{connector.attach.attach-block}}). Redirects stay on the configured origin
({{connector.attach.redirect-pinning}}).

Credentials use `secret://<name>` ({{connector.reference.credential-plane}}). The provider
chain answers each name once ({{connector.resolve.first-hit-wins}}) and rejects shadowing
({{connector.resolve.shadowed-name}}). Values resolve per read
({{connector.resolve.hydration-is-just-in-time}}); rotation preserves names
({{connector.rotate.turnover-preserves-the-name}}) and reaches runs within
{{connector.resolve.cache-ttl}}. Inventory follows configured bindings
({{connector.record.inventory}}), optional operator descriptions
({{connector.record.operator-record}}, {{connector.record.record-table}}), and provider custody
({{connector.record.provider-custody}}). It reads metadata without secret values
({{connector.record.observe-port}}), separates assertions from observations
({{connector.record.observation-provenance}}), reports unknown expiry
({{connector.record.expiry-unknown}}), and attributes runtime resolution
({{connector.record.provider-attribution}}).

Model calls use one configured endpoint ({{connector.infer.model-endpoint}}).
Ingested values travel fenced; the fence lowers injection odds and bounds nothing
({{connector.infer.fence-is-not-a-boundary}}), and output inherits its least-trusted input's
label ({{connector.infer.output-taint}}).

## Worked example

The `vendor-metrics` component reads reports with a bearer token named `vendor-token`, which
the deployment lists as a leased scope ({{connector.lease.scope-declaration}}). Its
credential binding allows one exact host, `api.vendor.example`
({{connector.attach.bound-host}}).

The host re-hashes the artifact, checks the allowlist
({{connector.declare-capability.allowlist-shape}}) and quota binding
({{connector.meter.limiter-binding}}), and opens the session. The first read gets a permit,
resolves a public address, and hydrates the `Authorization` template from a lease
({{connector.lease.head-of-chain}}, {{connector.lease.lease}}). The vendor's quota headers
ride the next usage report ({{connector.meter.report}}).

Midway, the limiter denies a permit. The guest sees an ordinary throttle response
({{connector.meter.synthesized-throttle}}), which the run retries. Later
the lease lapses with the mint endpoint down: the run sends the vendor nothing
({{connector.lease.vendor-requests-on-failure}}), and the transient failure goes back to the
step's schedule.

A Drive pipeline selecting two folders in one Shared Drive declares its roots together
({{connector.source.drive-root-set}}). It checks both before listing
({{connector.source.drive-root-validation}}), walks their trees within that boundary
({{connector.source.drive-root-walk}}), and resolves a file found under both to one row
({{connector.source.drive-overlap}}). The selection lives in the position
({{connector.source.drive-selection-position}}). A metadata-only pull records the exact
byte digest and capture outcome ({{connector.source.drive-metadata-only}},
{{connector.source.drive-capture-record}}), checking the file version after download
({{connector.source.drive-version-consistency}}); a complete changed selection records removals
({{connector.source.drive-selection-removals}}), while a capped listing refuses
({{connector.source.drive-list-bound}}).

## Where to look

| Question | Operation |
| --- | --- |
| What may a connector reach? | `connector.declare-capability` |
| Does a manifest change need review? | `connector.widen` |
| Why did a request never leave? | `connector.attach`, `connector.meter` |
| Which adapter answered a name? | `connector.resolve`, `connector.record` |
| How does a pin fail? | `connector.package` |
| How does a built-in source behave? | `connector.source` |
