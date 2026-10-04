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

The host implements outbound HTTP itself ({{connector.attach.host-mediation}}), and a guest,
a built-in source, a model call, a limiter call and an exec step all pass the same point
({{connector.attach.mediation-covers-every-egress}}). For each hop it checks the
allowlist, passes one pre-send hook carrying the hop's intent
({{connector.meter.pre-send-hook}}), an operator hook in front of the quota reservation
({{connector.meter.hook-composition}}), and only then resolves the host name through the
transport port ({{connector.attach.resolve-half}}), once, connecting to the address it vetted
({{connector.attach.resolve-once}}). It refuses internal ranges
({{connector.attach.private-address}}), writes the credential header
({{connector.attach.attach-block}}), and refuses to send it in cleartext
({{connector.attach.cleartext-endpoint}}). Redirects keep the configured host and port and
never weaken transport ({{connector.attach.redirect-pinning}}).

Credentials live on their own plane ({{connector.reference.credential-plane}}). A
declaration names `secret://<name>`, often inside a header template
({{connector.reference.value-template}}). A provider chain answers the name
({{connector.resolve.provider-chain}}), the first answering adapter wins
({{connector.resolve.first-hit-wins}}), and a name answered twice refuses rather than letting
a stray variable shadow the manager ({{connector.resolve.shadowed-name}}). Material enters
the process per read, never reaching a journal or log
({{connector.resolve.hydration-is-just-in-time}}). Rotation changes material and
keeps the name ({{connector.rotate.turnover-preserves-the-name}}), and each entry carries a
plaintext record of its grants and expiry ({{connector.record.operator-record}}).

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
