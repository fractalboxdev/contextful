---
contract: connector
---

# Connectors and credentials

## What it is for

A connector knows a vendor's paging, authentication and cursor. The host checks each
request against its declaration, reserves quota, attaches the credential and vets the
destination; the connector never holds credential bytes.

## How it works

Every connector implements one trait, as a sandboxed component or a compiled-in sibling,
and dispatch never branches on which ({{connector.export.authoring-path}}). A component
imports outgoing HTTP, logging and a wall clock and nothing else
({{connector.import.three-imports}}), and no connector receives a store handle
({{connector.export.store-blindness}}). It hands the runner batches and an opaque position.

A connector resolves from one of four forms ({{connector.package.distribution-form}}). A
remote artifact carries a content pin ({{connector.package.remote-unpinned}}), the host
re-hashes the bytes ({{connector.package.digest-mismatch}}), reuses compatible host output ({{connector.package.artifact-cache}}), and a run keeps the
build it was admitted with, so a rebuild reaches only later runs ({{run.own.admission-pin}}).
HTTPS and OCI artifact requests use a client restricted to the reference authority
({{connector.package.remote-transport}}).
An OCI reference selects one component layer ({{connector.package.oci-component-layer}}),
checks its descriptor ({{connector.package.oci-layer-integrity}}), and takes an optional
registry bearer from the project's credential binding ({{connector.package.oci-registry-bearer}}).
Admitted bytes enter a digest-keyed project cache ({{connector.package.remote-cache}});
damaged cached bytes refuse instead of reaching the component host
({{connector.package.remote-cache-corrupt}}).

Host access is declared, not requested. The manifest lists hosts, environment names and the
clock, and the host decides the grant at load ({{connector.declare-capability.declared-grant}});
code reaching past it fails to load ({{connector.declare-capability.undeclared-access}}). An
optional scope probe checks the bound credential's grant before the first read
({{connector.declare-capability.scope-probe}}).

A reload compares complete declarations ({{connector.widen.comparison}}). Narrowing needs no
widening review ({{connector.widen.narrowing}}); additional access reaches operator approval
({{connector.widen.approval-required}}), bound to both manifests
({{connector.widen.approval-binding}}). A host witness is a request both matchers can replay
({{connector.widen.host-witness}}).

Every outbound request uses the host's mediated client
({{connector.attach.mediation-covers-every-egress}}). Each hop passes the allowlist and
one pre-send hook ({{connector.meter.pre-send-hook}}) before host resolution
({{connector.attach.resolve-half}}). The transport connects to the vetted address
({{connector.attach.resolve-once}}), refuses internal ranges
({{connector.attach.private-address}}), and attaches credentials only to the admitted
destination ({{connector.attach.attach-block}}). Redirects stay on the configured origin
({{connector.attach.redirect-pinning}}).

Credentials use `secret://<name>` references ({{connector.reference.credential-plane}}).
The provider chain answers a name once ({{connector.resolve.first-hit-wins}}) and refuses
shadowing ({{connector.resolve.shadowed-name}}). Material hydrates per read
({{connector.resolve.hydration-is-just-in-time}}); rotation keeps the name
({{connector.rotate.turnover-preserves-the-name}}). Optional operator descriptions follow
{{connector.record.operator-record}}; provider custody follows
{{connector.record.provider-custody}}. The inventory starts from configured bindings
({{connector.record.inventory}}), distinguishes assertions from observations
({{connector.record.observation-provenance}}), and exposes missing expiry through
{{connector.record.expiry-unknown}}. Runtime attribution follows
{{connector.record.provider-attribution}}.

Model calls leave through one configured endpoint ({{connector.infer.model-endpoint}}).
Ingested values travel fenced; the fence lowers injection odds and bounds nothing
({{connector.infer.fence-is-not-a-boundary}}), and output inherits its least-trusted input's
label ({{connector.infer.output-taint}}).

## Worked example

The `vendor-metrics` component reads reports with a bearer token named `vendor-token`, which
the deployment lists as a leased scope ({{connector.lease.scope-declaration}}). Because it
binds a credential, its allowlist holds one exact host, `api.vendor.example`
({{connector.attach.bound-host}}).

The host re-hashes its pinned artifact, checks the allowlist
({{connector.declare-capability.allowlist-shape}}) and quota binding
({{connector.meter.limiter-binding}}), and opens the session. The first read gets a permit,
resolves a public address, and hydrates the `Authorization` template from a lease
({{connector.lease.head-of-chain}}, {{connector.lease.lease}}). The vendor's quota headers
ride the next usage report ({{connector.meter.report}}).

Midway through, the limiter denies a permit. The guest sees an ordinary throttle response
({{connector.meter.synthesized-throttle}}), which the run retries under its schedule. Later
the lease lapses while the mint endpoint is down: the run sends the vendor nothing
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
