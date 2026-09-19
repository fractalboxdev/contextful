# D19 — Outbound transport is origin-pinned and never downgrades

**Status:** accepted

## Context

A credential goes wherever its request goes. Redirects, vendor next links, configurable base URLs and system proxies each let a party other than the operator choose that destination.

## Decision

The origin answering any outbound request equals the origin the operator declared, over the same transport or a stronger one.

- `connector.attach` follows a hop only when it keeps the configured host and port, cleartext to TLS at that address included, for every source, credential-bearing or not. It resolves a permitted host once and connects to the vetted address; a private, link-local, unique-local, loopback or cloud-metadata address raises `ConnectorPrivateAddress` unless the configured host is loopback.
- Declaring any header selects the hardened client, which bypasses the system proxy; a header hydrated from a reference refuses a cleartext endpoint, with loopback exempt in IPv4 and IPv6.
- `connector.source` pins a vendor's next link and a search provider's host to the configured origin.
- `connector.lease` reaches the mint endpoint over TLS or loopback, follows no redirect and ignores the system proxy.
- `run.fetch` runs scheme, address-literal, resolved-address and host-list guards before a socket opens and again per hop, stops on a downgrade and caps a chain at 5 hops.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Pin origin at declaration and at every hop; refuse downgrade *(chosen)* | — | A vendor moving to a `www` or CDN host refuses until the declaration changes; proxy-only egress needs a direct path for credential traffic. |
| Pin only credential-bearing sources | Landed-content provenance | A redirected uncredentialed source would land another party's body in a trusted column. |
| Check the first link, not the landed URL or later hops | Credential containment | One listed shortener or redirect would empty the pin of meaning. |
| Warn and send, or follow a downgrade and record it | Irreversibility | The record would follow a disclosure it could not undo. |
| Vet the host name, not the address it resolves to | Internal-range containment | An allowlisted name whose DNS points inward would reach the machine's own network. |

## Consequences

- A publisher whose canonical address crosses domains needs both names listed.
- A source declaring only non-credential headers loses the proxy, failing to connect on proxy-only networks.

## Revisit

- The declaration grammar gains an explicit allowed-origin set per vendor.
- Proxy-only egress is observed as the sole path for a class of deployments.
