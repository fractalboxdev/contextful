# A-connector — Connectors and credentials decisions

**Status:** accepted

## executables-content-pinned — Executables are content-pinned and replay uses the recorded artifact

A live run completes against the digest its record names; a changed connector reaches new runs only.

| Option | Lost on | Cost |
| --- | --- | --- |
| Pin by digest, hold the pin while work is pending *(chosen)* | — | Pinning needs a container runtime; an urgent fix waits for in-flight runs. |
| Trust version tags | Review binding | Reviewed and running programs match only while the publisher behaves. |
| Reload in place, or adopt the new build on resume | Replay fidelity | Recorded output replays into code that never produced it. |
| Digest every command, including bare names | Runnability | Each package upgrade refuses every derive pipeline until re-pinned. |

Revisit: production runs unpinned local artifacts, arguing for inverting the default; the toolchain stops embedding the host triple.

## remote-artifacts-use — Remote artifacts use one verified component blob

A damaged cache entry refuses rather than replacing the bytes of a recorded run.

| Option | Lost on | Cost |
| --- | --- | --- |
| One pinned blob over mediated HTTP or one-layer OCI, cached by digest *(chosen)* | — | Multi-layer packages and registry token exchanges need another contract. |
| Resolve every run from the remote reference | Replay availability | An outage stops replay of bytes once admitted locally. |
| Reuse the guest's grant and headers for retrieval | Credential containment | Registry material reaches guest endpoints, and vendor material reaches the registry. |
| Trust an OCI tag and descriptor alone | Review binding | A moved tag changes executable bytes under an unchanged pin. |

Revisit: a required registry exposes no directly bound bearer; a package needs several blobs or an OCI index.

## content-pin-trust — The content pin is the only trust root

A reviewer binds review to bytes by rebuilding with `connector pin --verify` and comparing digests.

| Option | Lost on | Cost |
| --- | --- | --- |
| Pin plus reproducible rebuild *(chosen)* | — | An operator who neither builds nor reviews trusts whoever handed over the pin. |
| Publisher signature plus transparency log | Replay availability | Admission depends on a key custodian, a log operator and a network round trip. |
| Signature without a log, or trust on first use | Review binding | A stolen or rotated key signs bytes no reviewer saw. |
| Optional signature checked when present | Failure direction | An absent signature reads like a valid one. |

Revisit: a public registry distributes connectors to operators who neither build nor review them.

## pin-builder-runs — The pin builder runs one fixed platform

`PLATFORM` in `contextful-core::connector::package` defines the platform once.

| Option | Lost on | Cost |
| --- | --- | --- |
| One fixed platform, one manifest build input *(chosen)* | — | A toolchain bump re-pins every connector at once. |
| Per-manifest image and argv | Reproducibility | Each digest reproduces only on its own image. |
| Per-manifest toolchain version fields | One home per fact | The manifest and the image both state the toolchain. |
| Per-connector Dockerfile | Reviewability | Build inputs hide in a second file a reviewer reads as code. |

Revisit: a guest language other than Rust needs first-class pinning.

## precompiled-components-reuse — Precompiled components reuse only compatible host output

Precompiled output never travels with a pin.

| Option | Lost on | Cost |
| --- | --- | --- |
| Private cache with compatibility key and integrity check *(chosen)* | — | A hardened host still links a compiler for cache misses. |
| Recompile on every process start | Startup latency | Every guest pays compilation again. |
| Deserialize any file named by the artifact digest | Input integrity | A stale engine build or altered file reaches unsafe deserialization. |

Revisit: a pinned precompile artifact and a compiler-free host build exist for every deployment target.

## guest-world-targets — The guest world targets synchronous WASI 0.2

The call deadline, the session budget and the absent wait primitive each hold because a guest call runs to completion on one stack. Adopting 0.3 changes all three and is a major world bump.

| Option | Lost on | Cost |
| --- | --- | --- |
| WASI 0.2 with synchronous exports *(chosen)* | — | A guest overlaps requests only through outgoing-handler futures. |
| WASI 0.3 native async | Runtime maturity | The host runtime gates the 0.3 interfaces as experimental. |
| Both worlds side by side | One implementation per capability | Dispatch branches on the world. |

Revisit: the host runtime ships the 0.3 interfaces without an experimental gate.

## untrusted-input-decodes — Untrusted input decodes off-process and fails whole

An input lands whole and faithful or refuses by name. A native decoder is neither interruptible nor memory-bounded in process.

| Option | Lost on | Cost |
| --- | --- | --- |
| Process boundary, whole-input refusal per table *(chosen)* | — | Every input pays a boundary crossing; one damaged page discards a readable document. |
| In-process unwind guard or watchdog thread | Effectiveness | A native decoder crash or runaway allocation ends the process. |
| Land the parsed parts with a truncation flag | Distinguishability | No reader consults the flag, so a fragment is quoted as the document. |
| Lossy or statistical decoding | Honesty of landed values | Replacement characters land as publisher text. |

Revisit: crossing overhead exceeds decode time on small inputs; a memory-safe compound-binary reader exists.

## unreadable-objects-files — Unreadable objects and files dead-letter while the read lands the rest

One damaged input refusing a prefix or folder read would hold back every other input. The dead-letter table names each input a collection lacks; a replacing table still refuses, since a partial inventory cannot replace it.

| Option | Lost on | Cost |
| --- | --- | --- |
| Dead-letter table, position past the input *(chosen)* | — | A sibling table per table; a fixed decoder waits on a replay. |
| Refuse the whole read | Progress | One damaged object stalls thousands. |
| Skip and tally under a ceiling | Answerability | A count names no input. |
| Audit entries on the run record | Queryability | Capped per run; joins no table. |

## host-access-quota — Host access and quota are declared and fail closed

The operator reads every host access and quota draw in a file before anything runs; a degraded answer is a denial.

| Option | Lost on | Cost |
| --- | --- | --- |
| Grants decided at load, fail closed *(chosen)* | — | A second host needs an edit, review and reload; each permit batch costs a round trip. |
| A run-time permission request | Reviewability | The grant set is a union over code paths. |
| Fail open on an unreachable limiter | Failure direction | An unmetered burst lands during the outage. |
| A local token bucket in the engine | Visibility | It caps the engine with no view of other traffic. |

Revisit: a connector class whose reachable hosts are data-dependent; permit round trips become a measurable share of read time.

## outbound-transport-origin — Outbound transport is origin-pinned and never downgrades

The origin answering any outbound request equals the declared origin, over the same or a stronger transport.

| Option | Lost on | Cost |
| --- | --- | --- |
| Pin origin at declaration and every hop *(chosen)* | — | A vendor moving to a `www` or CDN host refuses until the declaration changes. |
| Pin only credential-bearing sources | Landed-content provenance | A redirected source lands another party's body in a trusted column. |
| Warn and send, or follow and record | Irreversibility | The record follows a disclosure it cannot undo. |
| Vet the host name, not the resolved address | Internal-range containment | A name whose DNS points inward reaches the machine's network. |

Revisit: the declaration grammar gains a per-vendor allowed-origin set; proxy-only egress is the sole path for a class of deployments.

## credentials-references-workload — Credentials are references, workload identity is the default, and leases lead

The engine holds the shortest-lived material the deployment mints.

| Option | Lost on | Cost |
| --- | --- | --- |
| References only, workload identity, lease first *(chosen)* | — | Material sits in engine memory for a request. |
| Inline material or environment templates | Plaintext at rest | The committed file or environment becomes the secret. |
| A broker sidecar as the default | Operability | Every deployment runs a second failure domain. |
| Lease provider last, first hit wins | Precedence | A leftover environment variable wins unnoticed. |

Revisit: workload identity becomes unattestable on a needed runtime; a broker becomes near-free to operate.

## credential-custody-stays — Credential custody stays with the provider and inventory labels its evidence

Configured adapters and observed runtime attribution answer different questions, so inventory labels each. A missing expiry stays unknown.

| Option | Lost on | Cost |
| --- | --- | --- |
| Provider custody, optional descriptive records *(chosen)* | — | Metadata completeness varies by adapter. |
| Mandatory encrypted entries in Git | Backend independence | External managers need duplicate ciphertext; rotation needs commits. |
| Provider enumeration as the whole inventory | Binding coverage | Unresolvable configured names disappear. |

Revisit: a shared provider metadata protocol supplies complete observations without value access.

## rotation-writes-forward — Rotation writes forward to the answering adapter

The environment, keychain and lease adapters have none, so operator-entered and leased credentials never write back.

| Option | Lost on | Cost |
| --- | --- | --- |
| Conditional put to the attributed adapter *(chosen)* | — | A read-only manager fails the run on every refresh. |
| First writable adapter in the chain | Name join | A shadowing adapter in front hides the stored version. |
| Serve the token from memory on a refused put | Provider custody | A rotated refresh token leaves the manager's copy dead, unrecorded. |
| Unconditional put | Atomic turnover | A slower replica overwrites a newer token. |

Revisit: a manager offers no conditional write; two resolvers share one cache across runs.

## egress-passes-transport — Egress passes one transport port and one pre-send hook ahead of resolution

Allowlist precedence decided the hook's position: no host lookup precedes a refusal, and one attachment point serves every hop.

| Option | Lost on | Cost |
| --- | --- | --- |
| Allowlist, one intent hook, port-owned resolution *(chosen)* | — | Each adapter re-implements proxy choice, timeout and body ceiling. |
| Hook ahead of the allowlist | Allowlist precedence | An undeclared host reaches the operator hook and the limiter. |
| An egress gate beside the meter | One attachment point | Three pre-send hooks, each a call site to miss. |
| A send-only port, system resolution | Lookup containment | A refused hop has already sent its host name to a resolver. |

Revisit: a transport that resolves remotely, such as a proxy-only deployment.

## manifest-widening-binds — Manifest widening binds approval to both declarations

The comparison makes no claim about vendor scope implications, DNS or credential bytes.

| Option | Lost on | Cost |
| --- | --- | --- |
| Compare grants, bind approval to both digests *(chosen)* | — | Opaque scope strings over-report semantic widening. |
| Treat every reload as approval | Review binding | A program update silently gains access. |
| Compare textual entries alone | Semantic inclusion | A covering wildcard hides the newly reachable hosts. |
| Solve the host grammar with SMT | Dependency budget | A solver adds a second trusted interpretation. |

Revisit: host grants gain ports, methods, paths or address ranges.

## drive-capture-binds — Drive capture binds bounded roots and mode to the position

Native exports carry no checksum for the exported representation, so capture hashes the bytes.

| Option | Lost on | Cost |
| --- | --- | --- |
| Bounded roots and explicit capture mode *(chosen)* | — | A changed selection rewalks its roots; metadata-only still downloads each body. |
| One pipeline per root | Overlap identity | One file lands in several tables. |
| Arbitrary roots and shortcuts | Scope review | One declaration reaches unrelated drives. |
| Drive's checksum alone | Byte identity | Native exports lack a checksum. |

Revisit: Drive offers a verifiable export digest without reading bytes, or a tenant needs more roots than the ceiling.

## picker-files-bind — Picker files bind selection and principal to one position

Under `drive.file`, a selected folder may not grant its children's bytes, so a selection admits individual files.

| Option | Lost on | Cost |
| --- | --- | --- |
| Explicit files, versioned credential names *(chosen)* | — | Selection changes reread retained files. |
| A picked folder grants its descendants | Proven access | A child's bytes may sit outside the grant. |
| One global refresh lock | Distributed correctness | Another process still rotates the same credential. |
