# 0138 — The mint client is reached over TLS or loopback, follows no redirect, and ignores the system proxy

**Status:** accepted 2026-09-18
**Decides:** `secret.lease.refusal.redirect`

## Context

Every mint call carries the bootstrap credential — the deployment's whole standing surface,
the one credential no lease shortens, the one whose revocation severs every leased source.
Each such call is a replay of that credential at whatever address the client ends up talking
to.

An HTTP client's defaults quietly widen that address set. A redirect-following client sends
the request again at a target named by the answer, which means the party answering the mint
endpoint chooses where the credential goes next. A proxy-honoring client routes the request
through a component named by ambient configuration, which means a machine's environment
chooses. Neither target is anything the deployment declared, and neither is a target any
guard reasoned about.

For ordinary vendor traffic those defaults earn their keep: redirects serve canonicalization
and pagination, and a corporate proxy is often the only egress a machine has. A mint endpoint
has neither need. It is one address, configured, answering one POST. It does not paginate,
it does not canonicalize, and the confidential-transport requirement means the endpoint is
either a TLS URL the deployment chose or a loopback provider on the same machine.

## Decision

The mint endpoint is reached over TLS or loopback. The mint client follows no redirect; a
`3xx` answer raises `SecretLeaseRedirect` and the mint credential is not replayed at the
named target. The mint client ignores any system proxy configuration.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **No redirect, no proxy, TLS or loopback** *(chosen)* | The set of addresses that ever see the bootstrap credential equals the one configured endpoint | A deployment whose egress runs through a corporate proxy configures the mint endpoint as directly reachable, or runs the provider on loopback |
| Follow a same-host redirect | A provider can move a path within its own host without a configuration edit | Lost on the same criterion at lower value: a mint endpoint has no pagination or canonicalization need that a redirect serves, so the exposure buys a convenience nobody needs |
| Follow a redirect to any TLS target | Provider migrations are transparent | Lost outright on the criterion: the party answering chooses where the standing credential is replayed, and TLS bounds the observer rather than the recipient |
| Honor the system proxy | Works on machines whose only egress is a proxy, with no extra configuration | Lost on exposure: mint traffic leaves the machine through a component the deployment did not choose for it, and that component terminates or observes the connection carrying the standing credential |
| Honor a proxy configured specifically for mint traffic | Reachability without ambient configuration deciding it | Lost on effort and timing: it is a further setting to specify and to pin, and the loopback and direct-reachability paths already cover the deployments blocked today |

## Criteria

1. **Where the mint credential can end up** — the complete set of addresses that receive the
   bootstrap credential across a run. *This is the criterion that decided it.* A followed hop
   or an ambient proxy replays the deployment's whole standing surface at a target no guard
   reasoned about, and the standing surface is the one thing the lease posture does not
   already bound by expiry.
2. **Value the flexibility buys** — what a redirect or a proxy actually provides for this
   traffic. Near zero for a single-address POST, which is why the criterion above wins so
   cheaply here and would not win as cheaply for vendor traffic.
3. **Reachability** — whether a deployment can talk to its provider at all. Real, and it is
   what the accepted cost is paid in.
4. **Consistency with the vendor-traffic client** — that one client's rules read like the
   other's. Not decisive; the two carry different credentials and different needs.

## Consequences

The addresses that see the bootstrap credential are enumerable from configuration alone, so
a review of where the standing surface travels reads one setting. A provider that moves its
endpoint is a declaration edit rather than a silent redirection, and a machine-level proxy
change cannot re-route credential traffic without anybody noticing.

The accepted cost: a deployment whose egress runs through a corporate proxy configures the
mint endpoint as directly reachable, or runs the provider on loopback. On a locked-down
network neither may be available without a firewall change, and the failure presents as a
connect error at mint time rather than as a configuration refusal. A deployment in that
position cannot adopt the lease posture until its network is changed.

A second cost: a provider migration that would have been transparent through a redirect
becomes a coordinated edit, and until it lands the deployment's leased sources fail closed.

## Revisit triggers

- Deployments are observed unable to reach a mint provider because proxy-only egress is
  their sole path, and loopback provisioning does not cover them.
- A mint protocol emerges whose flow legitimately spans more than one address, making
  single-address the wrong description of the endpoint.
- Explicit per-client proxy configuration exists elsewhere in the system, so specifying one
  for mint traffic costs no new mechanism.
