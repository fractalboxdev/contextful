# A-authority — Authority and enforcement decisions

**Status:** accepted

## redaction-no-copy — Redacted material leaves no derived copy

Redaction holds only when no derived artifact carries the pre-redaction value.

| Option | Lost on | Cost |
| --- | --- | --- |
| Prepare before recording or refuse the copy path *(chosen)* | — | A redacted column is reachable only by full scan. |
| Encrypt the index or journal and permit it | At-rest completeness | The guarantee rests on key custody for content redaction exists to remove. |
| Build locally, strip at the sync edge | Failure points | The local tree is itself a copy target. |
| Redact in each adapter, or at read time | Uniformity | The guarantee is as strong as the least careful adapter. |

Revisit: an index structure provably independent of the indexed values.

## body-projection — A closed body projection records canonical opaque groups

Removal protects designated row values, not arbitrary schema labels.

| Option | Lost on | Cost |
| --- | --- | --- |
| Closed projection and canonical opaque groups *(chosen)* | — | Authority changes refuse pending owners. |
| Record raw results, then rewrite outputs | At-rest completeness | The paid journal retains values removed from landed rows. |
| Accept caller-labelled prepared bytes | Authority | A caller claims preparation without canonical admission. |
| Rewrite recorded values on replay | Idempotence | A keyed digest becomes a digest of its own output. |

## prepared-record — A prepared source record binds canonical authority before execution

The recorded wire differs from the source wire, and replay re-admits it without repeating transforms, so keyed hashes and source resumption stay stable.

| Option | Lost on | Cost |
| --- | --- | --- |
| Canonical preparation and authority-bound replay *(chosen)* | — | Unrelated manifest edits also refuse a pending owner. |
| Repeat removal on replay | Idempotence | Keyed hashes change when applied to their own output. |
| Accept a source-supplied prepared flag | Authority | A connector labels raw rows as already protected. |
| Record raw rows, then rewrite parts | At-rest completeness | The journal retains the value its parts remove. |

## span-union — Span iteration rewrites the union of all matches

One forward pass over a bounded Thompson NFA keeps the leftmost start per state, so work stays linear in the value's length and no matched byte remains beside its substitute.

| Option | Lost on | Cost |
| --- | --- | --- |
| Union of all matches in one pass *(chosen)* | — | Per-byte work grows with NFA states. |
| Earliest-ending search | Completeness | `password=\S+` rewrites `password=h` and stores the rest of the secret. |
| Leftmost-first iterator search | Bounding | Repeated suffix searches have quadratic worst-case work. |
| Infer lineage from child names | Authority | A matching name establishes no source-column relationship. |

## row-classes — Per-row classes resolve through registered masks

Every branch compiles through the static policy's mask checks, so a per-row class adds no weaker path.

| Option | Lost on | Cost |
| --- | --- | --- |
| Registered strategy map with strict drop fallback *(chosen)* | — | Unknown classes lose their text. |
| One table per class | Utility | One heterogeneous value list becomes several reads. |
| Caller-chosen masks | Uniformity | One adapter omission exposes the original value. |
| Unknown classes left unmasked | Failure direction | A source-side typo grants weaker protection. |

## acting-principal — The acting principal is provider-verified, normalized once, fixed per chain

Only an identity provider makes a principal. A false identity match is a silent source-wide disclosure, so confidence relaxes no check.

| Option | Lost on | Cost |
| --- | --- | --- |
| Verify at the provider, normalize at the mint, fix per chain *(chosen)* | — | Unprovisioned accounts read nothing. |
| Normalize at each point of use | Consistency | One padded identity behaves as two principals. |
| Authorize heuristic or operator-asserted links | Failure asymmetry | A false match discloses a whole source silently. |
| Rebinding within a parent-carried set | Offline forgery | The parent's mint chooses whom every descendant impersonates. |

Revisit: a directory's identifiers exceed 256 B; provisioning latency dominates why subjects read nothing.

## delegation-profile — Delegation is a versioned profile over an attenuable-credential library

The library is Biscuit; the engine owns only the profile. No wall-clock timeout enters admission, so slow hardware refuses nothing fast hardware admits.

| Option | Lost on | Cost |
| --- | --- | --- |
| Library format, owned profile, evaluation ceilings *(chosen)* | — | The wire format tracks an upstream project. |
| A custom envelope beside the library token | Chain depth | Grandchildren are inexpressible; two formats need review. |
| A hand-rolled N-block format | Review cost | Its failure mode is silent forgery reviewed by nobody outside the project. |
| The library's full language, unprofiled | Bounding | Evaluation cost and reachable facts are holder-controlled. |

Revisit: chain verification cost becomes measurable at real delegation depths.

## offline-narrowing — Grants narrow offline, and admission re-checks the whole chain

Three pattern forms keep containment decidable offline.

| Option | Lost on | Cost |
| --- | --- | --- |
| Three-form patterns, double check, per-derivation revocation id *(chosen)* | — | Admission compares the whole chain per request. |
| Full glob or regular-expression patterns | Containment decidability | Derivation cannot decide offline whether a child lies within its parent. |
| Check at derivation alone, or the last block alone | Trust | A widened intermediate block passes unexamined. |
| One revocation id per root | Blast radius | Stopping one sub-agent stops every sibling. |

Revisit: chain comparison becomes measurable against request latency; a grant dimension carries a tenant set.

## query-templates — Query templates are deny-by-default capabilities

A template runs exactly the reviewed statement, so identifier confinement is the sole protection on that surface.

| Option | Lost on | Cost |
| --- | --- | --- |
| Deny-by-default allowlist, strict binding *(chosen)* | — | A parameter rename breaks callers with no window. |
| Restriction polarity like other dimensions | Blast radius | Long-lived credentials silently gain every later template. |
| Hidden from the listing, invocable by name | Enumeration | A guessed identifier reaches the read. |
| Templates through the statement guard | Expressiveness | The guard refuses exactly the reads templates exist for. |

Revisit: operators routinely grant star; a needed store feature requires a non-plain identifier.

## issuance-ceiling — One persisted issuance ceiling bounds every lifetime

One number, versioned with the project, sets the longest lifetime any mint path produces, and every other window derives from it. Format withdrawal is therefore never immediate.

| Option | Lost on | Cost |
| --- | --- | --- |
| One persisted ceiling; refuse explicit excess, clamp configured excess *(chosen)* | — | A ceiling change is a commit and deploy. |
| Per-mint flag or environment variable | Uniformity | The ceiling differs per host, invisible in review. |
| A policy service consulted at mint | Offline minting | Issuance inherits a network availability requirement. |
| Refuse a configured lifetime above the ceiling | Availability | A stale policy value becomes a total sign-in outage. |

Revisit: a verified need for lifetimes above 24 h; operators surprised by clamped exchange sessions.

## pinned-keys — Verification trusts only pinned or injected keys

The deployment, never the credential, chooses the verifying key, and an absent answer about keys refuses. Retiring a key is an action, not an omission.

| Option | Lost on | Cost |
| --- | --- | --- |
| Deployment-chosen keys *(chosen)* | — | Key-route availability is admission availability. |
| Dispatch on the credential's algorithm claim | Downgrade | Every weaker supported routine is reachable by asking. |
| Fall back to static pins or last-known-good | Frozen keys | A retired key verifies while the route is down. |
| Provider discovery inside the exchange | Reproducibility | Verification depends on a remote document at that instant. |

Revisit: the port cannot express a needed adapter capability such as batch signing.

## one-credential — A face admits one verified credential and carries authority as a value

Authority enters through one verification and flows as an explicit value, never ambient state. A wrong authoring posture mis-authors silently, so the posture argument is the one review point.

| Option | Lost on | Cost |
| --- | --- | --- |
| One admission path, authority as an argument *(chosen)* | — | Every embedding application needs an identity provider. |
| An opt-in perimeter secret or owner path | Attribution | A subjectless owner-equivalent path stands beside the credential. |
| Ambient environment or task-local authority | Attribution | Two callers through one daemon both take the process's principal. |
| One application credential plus application-side filtering | Enforcement boundary | A filter bug is a disclosure, not a failed check. |

Revisit: per-request mint latency dominates reads.

## sender-constrained — Credentials are sender-constrained, with a nonce replay window

Holding a key-bound credential admits nothing without proof of its key. The proof establishes key possession and claims nothing about what the client is.

| Option | Lost on | Cost |
| --- | --- | --- |
| Confirmation thumbprint, per-request signature, local nonce window *(chosen)* | — | Every client signs each request. |
| Mutual TLS instead of a carried proof | Reach across intermediaries | The engine behind the gateway receives an unbound credential. |
| A monotonic counter per client | Concurrency | Parallel honest requests arrive out of order and refuse. |
| A replay store shared across checkpoints | Locality | Admission calls a shared service per request. |

Revisit: cross-checkpoint replay observed within a credential's lifetime.

## network-bearer — A network face admits a short-lived audience-bound bearer, and a key-bound credential only under proof

MCP clients speaking Streamable HTTP send `Authorization: Bearer` and sign nothing, so a key-only network face serves none of them. Compatibility decided it over theft yield.

| Option | Lost on | Cost |
| --- | --- | --- |
| Short-lived audience-bound bearer; proof when a confirmation claim is present *(chosen)* | — | A captured bearer replays against its audience until expiry. |
| A proof on every network request | MCP-client compatibility | A client holding no signing key reaches no network face. |
| A bearer at any lifetime under the issuance ceiling | Theft yield | A captured bearer replays for up to 24 h. |

Revisit: mainstream MCP clients sign per-request proofs; bearer replay is observed within a lifetime.

## exchange-renewal — The exchange renews a credential, binding it to a proven holder key

Theft yield decided the fixed cap. Identity-provider availability therefore bounds a bearer session past one hour.

| Option | Lost on | Cost |
| --- | --- | --- |
| Bind on proof, bearer otherwise, fixed cap *(chosen)* | — | A bearer client re-exchanges hourly with a live assertion. |
| A face setting raising the bearer cap | Theft yield | A captured bearer admits for up to the 24 h ceiling. |
| A refresh token beside the credential | Surface | A second secret class to store, rotate and revoke. |
| Server-side expiry extension on use | Offline verification | The checkpoint writes state per request. |

Revisit: clients report hourly exchange as a sign-in outage.

## exhaustible-digest — A digest over an exhaustible class needs generalizing truncation

Anyone computing a digest over an enumerable value space recovers identities by enumeration, so the digest ships only where exhaustive search returns a crowd. Sufficient narrowness stays unverified.

| Option | Lost on | Cost |
| --- | --- | --- |
| Refuse a bare digest; admit it behind truncation *(chosen)* | — | A join on the column matches a crowd. |
| A bare digest relying on key secrecy | Recovery cost | Enumeration recovers every identity. |
| Refuse hashing for these classes | Utility | Equality filters and joins disappear. |
| A fixed width threshold | Honesty | One threshold fits one domain. |

Revisit: a per-person join over an exhaustible class becomes a requirement.

## placement-breadth — Placement breadth is declared data-side, and resolution fails narrow

Only the data side widens inference placement, every widening shows in the manifest diff, and ambiguity resolves narrowest.

| Option | Lost on | Cost |
| --- | --- | --- |
| Data-side breadth, intersection for synthesis *(chosen)* | — | A multi-zone batch mints per zone. |
| Subject-side wildcard as every zone | Widening side | A credential grants itself every zone. |
| Trust a synthesized row's declared set | Laundering | The constrained path enforces its own constraint. |
| A vendor client behind a feature flag | Verifiability | The placement claim depends on a build the reader cannot see. |

Revisit: fragment-level provenance through synthesis becomes observable; per-zone minting dominates issuance.

## redistribution — Redistribution ships an allowlist and never reaches backwards

Redistribution fails toward not shipping, and the engine is not the bucket's retention arbiter.

| Option | Lost on | Cost |
| --- | --- | --- |
| Enumerate what ships; name left-behind objects *(chosen)* | — | An operator cleans the bucket by hand. |
| Name the objects that stay behind | Failure direction | A new object ships by default carrying withheld rows. |
| Inspect each root object at push | Soundness | Record-blob contents are not statically decidable. |
| Delete left-behind objects automatically | Bucket authority | Deletion is irreversible and can cut a replica from its readers. |

## local-transport — Local transports bind possession to the holder key, then to the OS peer

MCP hosts spawn a server over stdio or relay to a Unix socket, and many sign nothing. Theft yield decided the order.

| Option | Lost on | Cost |
| --- | --- | --- |
| Holder proof first, peer uid otherwise *(chosen)* | — | Two local admission paths, one per-platform peer call. |
| Peer uid alone | Theft yield | A key-holding client gains nothing from its key. |
| Per-request proof on every transport | Host compatibility | Every local client needs a signing shim. |
| Peer process identity by pid | Portability | A pid is reusable. |

Revisit: stdio hosts sign per request; a portable call names the peer process.

## horizon — The horizon is a grant dimension enforced through the existing egress hook

A cutoff the agent passes per call is one it can omit or widen, so the horizon rides the credential. Tools beyond the read face reach the outside through one attachment point, so the horizon feeds the pre-send hook.

| Option | Lost on | Cost |
| --- | --- | --- |
| Grant dimension, read face and pre-send hook *(chosen)* | — | A provider-hosted tool is unavailable under a horizon. |
| A per-call `available_as_of` | Enforcement | The agent omits or widens it. |
| A per-tool horizon list | One home | A new tool slips past it. |
| A prompt instruction naming the cutoff | Enforcement | Models recall facts they are told to ignore. |

Revisit: a provider exposing hosted search through a client-side hook.
