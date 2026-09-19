# D34 — Configuration reaches only engine-named code and secrets

**Status:** accepted

## Context

The control document and the store registry are runtime data, writable by anyone holding the manifest or able to repoint a snapshot. Whatever that data can name, an attacker who reaches it can name.

## Decision

Configuration selects among things the engine names; it never names code, a host command, a secret or a remote control source.

- `surface.fire` runs a closed union of in-process engine operations — `sweep`, `build`, `fold`, `rebuild-catalog`, `sync-push`, `validate` — beside the pipeline-run kind. A block naming another kind, an argument vector or a host command raises `JobKindUnknown` at validation.
- `surface.register-store` derives a store's secret name (`<ID>_QUERY_TOKEN`) and binding name from its lower kebab-case id; an entry authoring either raises `StoreNameAuthored`. A reserved-name list keeps an id off names the surface uses. The authentication mode stays authored.
- `surface.arm` polls a control URL only on loopback, bound to the connection: no redirect, no proxy, `localhost` pinned. Any other host raises `ControlSourceNotLoopback`.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Closed job union, derived names, loopback control source *(chosen)* | — | Maintenance outside six kinds needs a release; renaming a store rotates its secret name; multi-host control waits on signed snapshots. |
| A job carrying an argument vector, or a plugin registry of kinds | Trust boundary | Anyone writing the manifest would gain arbitrary execution with the store's credentials and egress. |
| Authored credential and binding names per entry | Reachable secrets | A runtime edit could name any secret the worker holds and send it to a host the entry chose. |
| A remote control URL over TLS with a bearer | Content authenticity | The server, or anyone who compels or replays it, could pin or roll back the whole schedule set without a key. |

## Consequences

- Adding a job kind is an exhaustive-match obligation at every dispatch site.
- Reviewing the product states exactly what a job can do; no per-deployment allowlist changes that.
- A store id and its secret name cannot drift apart.

## Revisit

- A remote control source becomes admissible once it carries a bearer-authenticated read, TLS, and producer-side signing over `(version, content-hash)` verified before arming.
