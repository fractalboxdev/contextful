# D18 — Host access and quota are declared and fail closed

**Status:** accepted

## Context

Guests, compiled-in sources and derive engines reach vendors and spend quota shared with other traffic. A grant decided at run time is a union over code paths no reader can enumerate, and a control that fires after a request is formed sits behind the point the credential moved.

## Decision

Every host access and quota draw is named in a file the operator reads and decided before anything runs; a degraded answer is a denial.

- `connector.import` forwards only the guest configuration table; a table carrying a credential or environment reference refuses with `ConnectorConfigRejected` before I/O.
- `connector.declare-capability` decides grants from the manifest at load: undeclared access, an empty or bare-wildcard allowlist, and a wildcard beside a bound credential refuse. A declared scope probe runs before the first read; excess scopes and a missing scopes header both refuse.
- `connector.meter` issues one permit per outbound request at the mediation point. An unbound quota, an unreadable limiter answer and an unreachable limiter refuse; a compiled-in vendor-reaching source declares its grant in pipeline configuration. An acquire retries nothing: a denial returns to the step, whose schedule is the one retry layer.
- `run.select` keys a derive pipeline's metering on its task; `run.fetch` reaches only an operator-authored host list with no bare wildcard.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Declared grants decided at load, fail closed on every degradation *(chosen)* | — | A second host is a file edit and reload; a vendor dropping the scopes header breaks a working pipeline; each permit batch costs a round trip. |
| A run-time permission request | Reviewability | The grant set would be a union over code paths. |
| Ambient access, denial at the network stack | Timing | The request and credential would already exist when the denial fires. |
| Fail open on an unreachable limiter or missing header | Failure direction | An unmetered burst would land during the outage nobody is watching. |
| A local token bucket inside the engine | Visibility | It would cap the engine's rate with no view of the consumer's other traffic. |

## Consequences

- A remote artifact cannot be probed for its configuration export before download.
- Permits held by a crashed run return only on TTL expiry.

## Revisit

- A connector class whose reachable hosts are genuinely data-dependent.
- Permit round trips become a measurable share of read wall-clock time.
