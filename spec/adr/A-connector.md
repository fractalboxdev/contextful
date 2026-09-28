# A-connector — Connectors and credentials decisions

**Status:** accepted

## Executables are content-pinned and replay uses the recorded artifact

A reviewed program is identified by a content digest, and a live run completes against the digest its record names. `connector.package` refuses a remote artifact without a 64-hex pin at parse, refuses plain HTTP, re-hashes resolved bytes before load, and pins only from a digest-pinned container on one fixed platform; re-resolving a connector while a journaled run is live refuses. `run.own` holds connector identity, component world and plan hash until a terminal status; `run.exec` requires a digest on a path-form command.

| Option | Lost on | Cost |
| --- | --- | --- |
| Pin by digest, hold the pin while work is pending, apply changes to new runs only *(chosen)* | — | Pinning needs a container runtime; an urgent fix waits for in-flight runs; a bare-name binary changes with no configuration change. |
| Trust version tags | Review binding | Reviewed and running programs match only while the publisher behaves. |
| Reload in place, or adopt the new build on resume | Replay fidelity | Recorded output replays silently into code that never produced it. |
| Digest every command, including bare names | Runnability | Each package upgrade refuses every derive pipeline until re-pinned. |

Consequences: an unpinned local artifact runs whatever bytes sit on disk unless the store policy key or per-connector flag is set.
Revisit: production runs unpinned local artifacts, arguing for inverting the default; the toolchain stops embedding the host triple.

## Untrusted input decodes off-process and fails whole

An input lands whole and faithful or refuses by name, and no input ends the serving process. `run.land` decodes behind a process boundary bounding wall clock and resident memory; a crash is the same named diagnosis as a parse error. A partial parse refuses the whole input, naming path and page, worksheet or entry, and fails one table's pull while siblings keep their tick. `connector.source` reads office parts by exact name and refuses external references; an image lands a null body with no pixel decode; `run.fetch` reads UTF-8 only.

| Option | Lost on | Cost |
| --- | --- | --- |
| Process boundary, permanent whole-input refusal per table *(chosen)* | — | Every input pays a boundary crossing; one damaged page discards a readable document; non-UTF-8 publications are unreadable. |
| In-process unwind guard or watchdog thread | Effectiveness | The release build aborts on panic; native code is neither interruptible nor memory-bounded. |
| Land the parsed parts with a truncation flag | Distinguishability | No reader consults the flag, so a fragment is quoted as the document. |
| Skip the item and tally it | Answerability | A collection missing documents reads like one that has none. |
| Lossy or statistical decoding | Honesty of landed values | Replacement characters land as publisher text. |

Consequences: a large collection progresses only after the offending file is removed or converted.
Revisit: crossing overhead exceeds decode time on small inputs; a read surface honors a completeness predicate; a memory-safe compound-binary reader exists.

## Host access and quota are declared and fail closed

Every host access and quota draw is named in a file the operator reads and decided before anything runs; a degraded answer is a denial. `connector.import` refuses a configuration table carrying a credential or environment reference with `ConnectorConfigRejected`. `connector.declare-capability` decides grants at load and refuses an empty or bare-wildcard allowlist, and excess or missing scopes at the probe. `connector.meter` issues one permit per request, refuses on an unbound quota or unreachable limiter, and retries nothing. `run.select` meters per task; `run.fetch` reaches only an operator-authored host list.

| Option | Lost on | Cost |
| --- | --- | --- |
| Declared grants decided at load, fail closed on every degradation *(chosen)* | — | A second host is a file edit and reload; a vendor dropping the scopes header breaks a working pipeline; each permit batch costs a round trip. |
| A run-time permission request | Reviewability | The grant set is a union over code paths. |
| Ambient access, denial at the network stack | Timing | Request and credential already exist when the denial fires. |
| Fail open on an unreachable limiter or missing header | Failure direction | An unmetered burst lands during the unwatched outage. |
| A local token bucket inside the engine | Visibility | It caps the engine with no view of other traffic. |

Consequences: permits held by a crashed run return only on TTL expiry.
Revisit: a connector class whose reachable hosts are genuinely data-dependent; permit round trips become a measurable share of read wall-clock time.

## Outbound transport is origin-pinned and never downgrades

The origin answering any outbound request equals the declared origin, over the same or a stronger transport. `connector.attach` follows a hop only on the configured host and port, connects to the vetted resolved address, and raises `ConnectorPrivateAddress` on a private, link-local, loopback or metadata address unless the configured host is loopback; declared headers select the proxy-bypassing client. `connector.source` pins next links and search hosts; `connector.lease` follows no redirect; `run.fetch` guards every hop and caps a chain at 5.

| Option | Lost on | Cost |
| --- | --- | --- |
| Pin origin at declaration and every hop; refuse downgrade *(chosen)* | — | A vendor moving to a `www` or CDN host refuses until the declaration changes; proxy-only egress needs a direct path for credential traffic. |
| Pin only credential-bearing sources | Landed-content provenance | A redirected uncredentialed source lands another party's body in a trusted column. |
| Check the first link only | Credential containment | One shortener empties the pin of meaning. |
| Warn and send, or follow a downgrade and record it | Irreversibility | The record follows a disclosure it cannot undo. |
| Vet the host name, not the resolved address | Internal-range containment | A name whose DNS points inward reaches the machine's network. |

Consequences: a publisher whose canonical address crosses domains needs both names listed.
Revisit: the declaration grammar gains a per-vendor allowed-origin set; proxy-only egress is the sole path for a class of deployments.

## Credentials are references, workload identity is the default, and leases lead

Declarations carry names, stores carry opaque bytes, connectors carry provider know-how, and the engine holds the shortest-lived material the deployment mints. A `connector.reference` template accepts only `${secret://<name>}` placeholders; `connector.record` puts grants, account or tenancy, creation, expiry and rotation location above every encrypted entry. `connector.resolve` defaults to an external manager through workload identity, refuses inline material outside development, and raises `SecretNameShadowed` when two adapters answer a name. `connector.lease` leads the chain with no fall-through and retries nothing; `connector.rotate` fails the run closed on a refused write-back.

| Option | Lost on | Cost |
| --- | --- | --- |
| Reference-only declarations, workload identity default, lease at the head *(chosen)* | — | Material sits in engine memory for a request; a credential needs a five-field operator record; a read-only manager cannot run the OAuth refresh loop. |
| Inline material or environment templates | Plaintext at rest | The committed file or environment becomes the secret. |
| A broker sidecar as the default | Operability | Every deployment runs a second failure domain. |
| Lease provider last, first hit wins | Posture | A leftover environment variable wins unnoticed. |
| Provider rotation engines inside the store | Swappability | Every backend reimplements every vendor's exchange. |

Consequences: local development pastes a token only behind an explicit opt-in flag; a bootstrap backend is a second backend to operate.
Revisit: workload identity becomes unattestable on a needed runtime; a broker becomes near-free to operate; a runtime identity authenticates to the mint directly.

## Egress passes one transport port and one pre-send hook ahead of resolution

Status: accepted.

Context: the client resolves a name before any gate, a reservation sees no request, the model endpoint bypasses mediation, and `contextful-outbound` links its HTTP stack unconditionally.

Decision: the runtime defines a transport port with a resolve half and a send half; its HTTP adapter sits behind a default-on `transport-ureq` feature. The model call sends through the mediated client. Each hop passes the allowlist, then one hook carrying the request intent, then resolution; the limiter reservation is the hook's innermost implementation, and an operator hook composes in front.

Criteria: one attachment point, no lookup before a refusal, `connector.meter.allowlist-precedence` intact; allowlist precedence decided the hook's position.

| Option | Lost on | Cost |
| --- | --- | --- |
| Allowlist, one intent hook, port-owned resolution *(chosen)* | — | Each adapter re-implements proxy choice, timeout and body ceiling; a ledger learns allowlist refusals from the run record, not the hook. |
| Hook ahead of the allowlist | Allowlist precedence | An undeclared host reaches the operator hook and the limiter. |
| An egress gate beside the meter | One attachment point | Three pre-send hooks, each a call site to miss. |
| A send-only port, resolution through the system | Lookup containment | A refused hop has already sent its host name to a resolver. |
| The HTTP stack unconditional | Embeddability | An embedder with its own client links a second TLS stack. |

Consequences: `contextful-decode` then holds the record decoders with no edge to `contextful-outbound`, a split needing no section here. Each transport adapter is audited against the attach clauses.
Revisit: a transport that must resolve remotely, such as a proxy-only deployment.
