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
re-hashes the bytes before load ({{connector.package.digest-mismatch}}), and a run keeps the
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
({{connector.rotate.turnover-preserves-the-name}}), and each entry records its grants
and expiry ({{connector.record.operator-record}}).

Model calls leave through one configured endpoint ({{connector.infer.model-endpoint}}).
Ingested values travel fenced; the fence lowers injection odds and bounds nothing
({{connector.infer.fence-is-not-a-boundary}}), and output inherits its least-trusted input's
label ({{connector.infer.output-taint}}).

## Worked example

The `vendor-metrics` component reads reports with a bearer token named `vendor-token`, which
the deployment lists as a leased scope ({{connector.lease.scope-declaration}}). Because it
binds a credential, its allowlist holds one exact host, `api.vendor.example`
({{connector.attach.bound-host}}).

The name is not on the built-in list ({{connector.package.built-in-registry}}), so the host
fetches the pinned HTTPS artifact, re-hashes it and instantiates it. At load the allowlist
shape passes ({{connector.declare-capability.allowlist-shape}}) and the declared quota finds
its operator binding ({{connector.meter.limiter-binding}}). At session open the scope probe
reports grants within the expectation, and discovery runs.

The first read asks for that host. The host finds it allowlisted, acquires a permit, then resolves it to a public address. It hydrates the `Authorization`
template: the lease provider heads the chain and answers for the declared name
({{connector.lease.head-of-chain}}), holding material plus one expiry in memory
({{connector.lease.lease}}). The request goes out over TLS, and the vendor's quota
headers ride the next usage report ({{connector.meter.report}}).

A pipeline names it by artifact path ({{connector.package.component-source}}), and each
run records its hash ({{connector.package.component-load}}).

Midway through, the limiter denies a permit. The guest sees an ordinary throttle response
({{connector.meter.synthesized-throttle}}), which the run retries under its schedule. Later
the lease lapses while the mint endpoint is down: the run sends the vendor nothing
({{connector.lease.vendor-requests-on-failure}}), and the transient failure goes back to the
step's schedule.

## Where to look

| Question | Operation |
| --- | --- |
| What may a connector reach? | `connector.declare-capability` |
| Does a manifest change need review? | `connector.widen` |
| Why did a request never leave? | `connector.attach`, `connector.meter` |
| Which adapter answered a name? | `connector.resolve`, `connector.record` |
| How does a pin fail? | `connector.package` |
| How does a built-in source behave? | `connector.source` |
