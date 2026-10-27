---
contract: connector
---

# Connectors and credentials

## The problem a connector solves

A **vendor** is an outside service, such as a reporting API. Reading its data needs code
that knows the vendor's pages and cursors, and a credential the vendor accepts. That code
runs inside the deployment, so it gets no store handle and sees no credential value.
A **connector** holds the vendor knowledge; the **host** process runs the connector,
checks every destination, reserves quota and attaches credentials.

The example is `vendor-metrics`, a connector reading reports from `api.vendor.example`
with a bearer token named `vendor-token`.

## Loading the connector

A connector is either a sandboxed WebAssembly component or a native source compiled into
the binary, and dispatch treats both alike ({{connector.export.authoring-path}}).
`vendor-metrics` ships as a remote artifact, one of four distribution forms
({{connector.package.distribution-form}}).

The host re-hashes the downloaded bytes and refuses a mismatch before the engine sees
them ({{connector.package.digest-mismatch}}). The component then runs under a memory
limit and a per-call deadline ({{connector.package.call-deadline}}).

Its manifest lists the hosts it reaches, and the host decides that grant at load
({{connector.declare-capability.declared-grant}}). Code reaching past the list fails to
load ({{connector.declare-capability.undeclared-access}}). When a new version lists
another host, the reload compares the two manifests: a candidate host succeeds only
when an admitted host already covers it ({{connector.widen.host-inclusion}}).

## What crosses the interface

The component imports outgoing HTTP, logging and a wall clock, and its standard library
sees no files, environment or arguments ({{connector.import.empty-context}}). Batches cross
the interface as Arrow IPC, a binary columnar format ({{connector.export.batch-encoding}}).

## One request, end to end

`vendor-metrics` asks for a report page. Every outbound request goes through the host's
one mediated client ({{connector.attach.mediation-covers-every-egress}}). The host
resolves the host name once and connects to the address it vetted
({{connector.attach.resolve-once}}). A name resolving to a private or metadata address
fails ({{connector.attach.private-address}}).

The credential binding allows one exact host, `api.vendor.example`
({{connector.attach.bound-host}}). An `[attach]` block maps the `Authorization` header to
the template `Bearer ${secret://vendor-token}` ({{connector.reference.value-template}}).
The host fills that template afresh for each admitted request ({{connector.attach.per-request-hydration}}).

A redirect keeps the credential only at the configured host and port
({{connector.attach.redirect-pinning}}).

## Where the credential comes from

A `secret://` name is a reference, and a provider chain of adapters answers it,
starting with the lease provider ({{connector.resolve.provider-chain}}). The first
adapter answering a name serves it for the run ({{connector.resolve.first-hit-wins}}). A
second adapter that also holds the name fails the lookup, so no adapter shadows another
silently ({{connector.resolve.shadowed-name}}).

Hydration places the resolved value into an outbound request. The host hydrates per
request, and no record or log line holds the value
({{connector.resolve.hydration-is-just-in-time}}). A cached value lives at most 300 s
({{connector.resolve.cache-ttl}}).

The deployment declares `vendor-token` as a lease, so the lease adapter answers it first
({{connector.lease.head-of-chain}}). The resolver keeps a leased credential in memory
only, and evicts it at expiry ({{connector.lease.lease}}).

Rotation replaces the value and keeps the logical name, so no declaration or pinned
artifact changes ({{connector.rotate.turnover-preserves-the-name}}). An operator audits
names with `contextful secrets list`, which shows configured bindings and reads no value
({{connector.record.inventory}}).

## When quota or the lease fails

The operator binds the vendor's quota name to a limiter
({{connector.meter.limiter-binding}}). Midway through the pull the limiter denies a
permit. The component sees an ordinary `429` with a retry delay, and the run's retry
schedule handles it ({{connector.meter.synthesized-throttle}}).

Later the lease expires while its mint endpoint is down. The run then sends 0 requests
to the vendor ({{connector.lease.vendor-requests-on-failure}}), and the transient failure
returns to the step's schedule.

## Model calls

Every model call resolves to one operator-configured endpoint
({{connector.infer.model-endpoint}}). Data boundary markers enclose each ingested value
({{connector.infer.data-fence}}). The markers lower the success rate of injected instructions but
bound nothing
({{connector.infer.fence-is-not-a-boundary}}).

Model output carries the least-trusted
label among its inputs ({{connector.infer.output-taint}}).

## Built-in sources and Drive

The engine compiles in sources such as a paginated HTTP walk; each walk declares exactly
one pagination shape ({{connector.source.pagination}}).

The Drive sources read Google Drive under an end user's grant. `gdrive-picks` serves
files a person selects through Picker ({{connector.drive-picks.picker-source}}), and
`gdrive-sheet` captures a Sheet as rows ({{connector.drive-sheet.source-name}}).

A selection keeps a change token, committed with the first complete listing
({{connector.drive-changes.initial-token}}). If Drive rejects the token, the source lists
the selection again ({{connector.drive-changes.invalid-token}}).

Under domain-wide delegation, each request names one admitted Workspace user as its
subject ({{connector.drive-domain-auth.impersonated-principal}}). A subject outside the
declared domain fails before any token mint ({{connector.drive-domain-auth.domain-boundary}}).

A connection binds one secret name to its tenant and principal, and a run hydrates only
that name ({{connector.principal-secrets.name-binding}}).
