# A-authority — Authority and enforcement decisions

**Status:** accepted

## Redacted material leaves no derived copy

Redaction holds only when no derived artifact carries the pre-redaction value, so each copy path closes at its source. `store.encrypt` refuses an index over a column redacted at write time; `run.journal` refuses write-path redaction paired with a journaling source, at manifest validation and again at run open; the derive tier sits on the journaling opt-out list. `connector.attach` renders a URL as scheme, host, port and path and refuses configured userinfo; `run.emit` refuses a derived error string written without address redaction.

| Option | Lost on | Cost |
| --- | --- | --- |
| Refuse each copy path at declaration or at the row builder *(chosen)* | — | A redacted column is reachable only by full scan; redacting pipelines lose replay-without-refetch; a derive crash re-pays up to one run of metered inference. |
| Encrypt the index or journal and permit it | At-rest completeness | The guarantee rests on key custody for content redaction exists to remove. |
| Build locally, strip at the sync edge | Failure points | The local tree is itself a copy target. |
| Redact in each adapter, or at read time | Uniformity | The guarantee is as strong as the least careful adapter; stored material outlives every reader that forgets. |

Consequences: two failures at different pages of one walk render identically from a scrubbed URL.
Revisit: an index structure provably independent of the indexed values; redaction runs ahead of the journal.

## The acting principal is provider-verified, normalized once, fixed per chain

Only an identity provider makes a principal. `authority.identify` checks every subject at the mint: non-empty, at most 256 B, no control character, no surrounding whitespace; the command line trims a padded value and the automated exchange refuses it. A link authorizes only with method `scim_email` or `oidc_sub`; `operator_asserted` stages and confers nothing, and `disclosure.reach` consumes verified links alone. A derivation naming a different `on_behalf_of` refuses, and `authority.issue` refuses a write or execute grant naming none.

| Option | Lost on | Cost |
| --- | --- | --- |
| Verify at the provider, normalize at the mint, fix per chain *(chosen)* | — | Longer directory identifiers are inexpressible; unprovisioned accounts read nothing; acting for many people costs a round trip each. |
| Normalize at each point of use | Consistency | One padded identity behaves as two principals. |
| Authorize heuristic or operator-asserted links above a threshold | Failure asymmetry | A false match is a silent source-wide disclosure. |
| Rebinding within a parent-carried set | Offline forgery | The parent's mint chooses whom every descendant impersonates. |
| Refuse an unbound write at the write | Failure placement | The credential admits and fails at its first useful action. |

Consequences: a script calling the command line accepts a padded value the exchange refuses.
Revisit: a directory's identifiers exceed 256 B; provisioning latency dominates why subjects read nothing.

## Delegation is a versioned profile over an attenuable-credential library

The library is Biscuit, and its own format is the one wire format; the engine owns a versioned profile and no envelope. `authority.profile` names the admitted facts, checks and restriction tuples, and refuses any other element; the library owns serialization, signatures, chaining and evaluation. A credential is an authority block plus N attenuation blocks; the supported chain depth is unsettled under `authority.attenuate`. Time, audience, resolved resources and request identity are reserved engine-supplied facts. Evaluation admits no third-party block, external function, recursion or regular expression, under fact and iteration ceilings that refuse past their bound.

| Option | Lost on | Cost |
| --- | --- | --- |
| Library format, N-block chain, owned profile, evaluation ceilings *(chosen)* | — | The wire format tracks an upstream project; a pattern restriction is an enumerated allowlist. |
| A custom envelope beside the library token | Chain depth | One attenuation segment leaves grandchildren inexpressible; two formats each need review. |
| A hand-rolled N-block format | Review cost | Its failure mode is silent forgery reviewed by nobody outside the project. |
| The library's full language, unprofiled | Bounding | Evaluation cost and reachable facts are holder-controlled. |
| Bearer token plus a policy-service lookup | Read-path independence | Every admission fails when the policy service does. |

Consequences: no wall-clock timeout enters admission, so slow hardware refuses nothing fast hardware admits.
Revisit: chain verification cost becomes measurable at real delegation depths; the library gains a backtracking-free matcher identical across native and WebAssembly builds.

## Grants narrow offline, and admission re-checks the whole chain

Every derivation provably narrows its parent. `authority.grant` table patterns take three forms — bare star, trailing star, exact literal — answered by one implementation for derivation, view registration, tool visibility and replica selection. A tenant scope over a table without a bare outermost partition column refuses; a tenant-scoped child lives 15 min. `authority.attenuate` runs the identical comparison at derivation and over the whole chain at admission, naming the widened dimension; dropping or changing a tenant scope refuses. `authority.revoke` keys the denylist per derivation, withdrawing one subtree.

| Option | Lost on | Cost |
| --- | --- | --- |
| Three-form patterns, double narrowing check, per-hop revocation id *(chosen)* | — | Admission pays chain-depth comparison per request; exclusions are sets of concrete patterns; the denylist grows per withdrawn hop. |
| Full glob or regular-expression patterns | Containment decidability | Derivation cannot decide offline whether a child pattern lies within its parent's. |
| Check at derivation alone, or the last hop alone | Trust | A widened intermediate hop passes unexamined. |
| Ignore an unbindable tenant scope | Failure direction | A scoped-looking credential reads the whole table. |
| One revocation id per root | Blast radius | Stopping one sub-agent stops every sibling. |

Consequences: a table rebuild dropping the partition column surfaces as a refusal at the read.
Revisit: chain comparison becomes measurable against request latency; a grant dimension carries a tenant set.

## Query templates are deny-by-default capabilities

A template is reachable only by a grant naming it and runs exactly the reviewed statement. `authority.grant` treats the template allowlist as a capability: absent authorizes none, star authorizes every declared template, and a child names only identifiers its parent covered; an uncovered template is absent from the listing and refuses on invocation. `disclosure.template` binds strictly — missing, unknown or mistyped arguments refuse — and validates placeholders, single statements and built-in prefix collisions at manifest check and startup. The statement runs unrewritten under every enforcement layer, over plain store tables only.

| Option | Lost on | Cost |
| --- | --- | --- |
| Deny-by-default allowlist, strict binding, unrewritten statement over plain tables *(chosen)* | — | One grant carries two polarities; a new template amends every consuming grant; a parameter rename breaks callers with no window. |
| Restriction polarity like other dimensions | Blast radius | Long-lived credentials silently gain every later template. |
| Hide from the listing, allow invocation by name | Enumeration | A guessed identifier reaches the read. |
| Coerce or default arguments | Statement fidelity | A caller receives rows for values it never sent. |
| Gate templates through the statement guard | Expressiveness | The guard refuses exactly the reads templates exist for. |

Consequences: identifier confinement is the sole protection on that surface.
Revisit: operators routinely grant star; a needed store feature requires a non-plain identifier.

## One persisted issuance ceiling bounds every lifetime

One number, versioned with the project, sets the longest lifetime any mint path produces, and every other window derives from it. `authority.issue` reads a persisted policy: a ceiling of at most 24 h and a default audience. An explicit lifetime above the ceiling refuses; `authority.exchange` clamps a configured lifetime down to the ceiling, with an exchange ceiling of 3600 s. `authority.revoke` refuses a rotation grace shorter than the effective ceiling and validates against the recorded previous ceiling. Format withdrawal runs through revocation, expiry or key rotation at a declared cutover; no flag restores it.

| Option | Lost on | Cost |
| --- | --- | --- |
| One persisted ceiling; refuse explicit excess, clamp configured excess *(chosen)* | — | A ceiling change is a commit and deploy; one condition has two behaviors; a clamped deployment learns only from expiry. |
| Per-mint flag or environment variable | Uniformity | The ceiling differs per host, invisible in review. |
| A policy service consulted at mint | Offline minting | Issuance inherits a network availability requirement. |
| Clamp explicit requests silently | Caller expectation | A caller plans around a lifetime it does not hold. |
| Refuse a configured lifetime above the ceiling | Availability | A stale policy value becomes a total sign-in outage. |

Consequences: the longest issued lifetime floors rotation cadence, and format withdrawal is never immediate.
Revisit: a verified need for lifetimes above 24 h; operators surprised by clamped exchange sessions.

## Verification trusts only pinned or injected keys

The deployment, never the credential, chooses the verifying key, and an absent answer about keys refuses. `authority.issue` signs through one provider-agnostic port with seed file, secret reference and remote signing oracle adapters. The pinned key's scheme decides verification; a conflicting algorithm claim refuses. `authority.verify` accepts a key set from static pins or an opted-into key route; an opted-in checkpoint lacking the set refuses everything — the engine declines to start, a gateway answers 503. `authority.exchange` verifies against operator-injected secret, PEM key or `kid`-selected key set, with no network call.

| Option | Lost on | Cost |
| --- | --- | --- |
| Deployment-chosen keys: signing port, pinned scheme, key set, injected material *(chosen)* | — | Port operations are the adapters' intersection; key-route availability is admission availability; provider keys rotate out of band. |
| Dispatch on the credential's algorithm claim | Downgrade | Every weaker supported routine is reachable by asking. |
| A single pinned key | Rotation | No overlap; every rotation is a flag day. |
| Fall back to static pins or last-known-good | Frozen keys | A retired key verifies while the route is down. |
| Provider discovery inside the exchange | Reproducibility | Verification depends on a remote document at that instant. |

Consequences: a missed provider rotation fails the mint closed; retiring a key is an action, not an omission.
Revisit: the port cannot express a needed adapter capability such as batch signing; key-route outages lead admission refusals.

## A face admits one verified credential and carries authority as a value

Authority enters through one verification and flows as an explicit value, never ambient state. `authority.issue` takes an authoring posture at every project-load site with no default: `session` authors through one ambient credential, `per_request` withholds it. A served face admits a verified capability credential and nothing else — no perimeter bearer, gateway shared secret or owner path, flagged or not. `authority.verify` yields an admitted-authority value, with no fabricating constructor, that every read surface and row-landing effect takes. `authority.exchange` mints per signed-in reader per request.

| Option | Lost on | Cost |
| --- | --- | --- |
| One admission path, authority as an argument, per-reader mint *(chosen)* | — | Every load site and effect carries a parameter; every embedding application needs an identity provider; one mint per request sits on the read path. |
| An opt-in perimeter secret or loopback owner path | Attribution | A subjectless, expiryless owner-equivalent path stands beside the credential. |
| Infer posture from deployment shape | Silence | A wrong guess mis-attributes rows with no error. |
| Read the environment or a task-local at each effect | Attribution | Two callers through one daemon both take the process's principal. |
| One application credential plus application-side filtering | Enforcement boundary | A filter bug is a disclosure, not a failed check. |

Consequences: a wrong posture mis-authors silently, so the posture argument is the one review point.
Revisit: per-request mint latency dominates reads; a face needs a caller class no grant expresses.

## Credentials are sender-constrained, with a nonce replay window

Over a network, holding a credential admits nothing without proof of its key, and a captured request is not re-sendable. A network checkpoint requires a confirmation thumbprint of a client public key in every credential; under a credential carrying one, every transport requires a per-request signature over method, target, body digest and nonce from the matching private key; a mismatch refuses. A credential with none admits only locally, as the local-transports section records. A nonce repeating inside the checkpoint's local replay window refuses; one outside it is not remembered. The proof establishes key possession and claims nothing about what the client is.

| Option | Lost on | Cost |
| --- | --- | --- |
| Confirmation thumbprint, per-request signature, local nonce window *(chosen)* | — | Every client holds a key pair and signs each request; a capture replayed at another checkpoint meets only the credential's other bounds. |
| Bearer bytes defended by short lifetimes and one audience, admitting MCP clients that sign nothing | Theft yield | A copied credential acts as the original against its audience until expiry. |
| Mutual TLS instead of a credential-carried proof | Reach across hops | The engine behind the gateway receives an unbound credential. |
| A monotonic counter per client | Concurrency | Parallel honest requests arrive out of order and refuse. |
| A replay store shared across checkpoints | Locality | Admission calls a shared service per request. |

Consequences: a credential is minted against one client's key; each checkpoint holds nonce state sized by window and request rate.
Revisit: cross-checkpoint replay observed within a credential's lifetime; a client class cannot hold a private key.

## A network face admits a short-lived audience-bound bearer, and a key-bound credential only under proof

**Status:** accepted; narrows the network requirement of the sender-constrained decision above.

Context: MCP clients speaking Streamable HTTP send `Authorization: Bearer` and sign nothing per request, so a network face admitting only key-bound credentials serves none of them.

Decision: `authority.verify` admits a credential with no confirmation claim over a network as a bearer when it names the checkpoint's declared audience and lives at most an hour from issue to expiry; a longer-lived bearer refuses, and its holder refreshes through token exchange. A credential carrying a confirmation thumbprint, minted with `--holder`, admits only under a proof on every request. Local transports keep holder proof first, then the peer uid.

Criteria: MCP-client compatibility, then theft yield; compatibility decided it.

| Option | Lost on | Cost |
| --- | --- | --- |
| Short-lived audience-bound bearer; proof exactly when a confirmation claim is present *(chosen)* | — | A captured bearer replays against its audience until expiry; inside that window only the denylist withdraws it. |
| A proof on every network request | MCP-client compatibility | A client holding no signing key reaches no network face. |
| A bearer at any lifetime under the issuance ceiling | Theft yield | A captured bearer replays for up to the 24 h ceiling. |

Consequences: an operator wanting replay resistance mints with `--holder`; a bearer client re-exchanges at least hourly.
Revisit: mainstream MCP clients sign per-request proofs; bearer replay is observed within a lifetime.

## The exchange renews a credential, binding it to a proven holder key

**Status:** accepted; applies the network-face decision above to the exchange.

Context: a network face admits a bearer living at most an hour and a key-bound credential only under a per-request proof, so a client renews hourly or holds a key.

Decision: `authority.exchange` is the one renewal path; nothing extends an expiry. An exchange request carrying a possession proof mints a credential whose confirmation thumbprint is the proof key's; one carrying none mints a bearer under the hour cap, and no setting raises that cap. The command line and every served face parse one request through one library function.

Criteria: theft yield, then MCP-client compatibility; theft yield decided the fixed cap.

| Option | Lost on | Cost |
| --- | --- | --- |
| Bind on proof, bearer otherwise, fixed hour cap *(chosen)* | — | A bearer client re-exchanges hourly and needs a live assertion to do it. |
| A face setting raising the bearer cap | Theft yield | A captured bearer admits for up to the 24 h ceiling. |
| Always mint a bearer, ignoring a proof | Theft yield | A holder able to sign still receives replayable bytes. |
| A refresh token beside the credential | Surface | A second secret class to store, rotate and revoke. |
| Server-side expiry extension on use | Offline verification | The checkpoint writes state per request, and expiry stops being a claim. |

Consequences: identity-provider availability bounds a bearer session past one hour.
Revisit: clients report hourly exchange as a sign-in outage; a face needs holders with no identity provider.

## A digest over an exhaustible class needs generalizing truncation

A digest over an enumerable value space — national identifier, phone, email, medical record number — ships only behind truncation, so exhaustive search returns a crowd. `authority.mask` refuses a class outside the registry with `EnforceUnknownClass` at manifest check. A bare keyed hash over an exhaustible class refuses; `combine = "truncate:<n>"` satisfies it, and random identifiers keep a bare hash. Truncation is the one secondary admitted behind a hash or tokenization. A width at or past the primary's output refuses; narrower widths are operator judgment. Primary and secondary compile to one inseparable value.

| Option | Lost on | Cost |
| --- | --- | --- |
| Refuse a bare digest; admit it behind truncation; refuse no-op widths *(chosen)* | — | A join on the column lands on a crowd; a too-wide truncation passes as a near-unique mapping. |
| A bare digest relying on key secrecy | Recovery cost | Any party computing the digest recovers identities by enumeration. |
| Refuse hashing for these classes | Utility | Equality filters and joins disappear. |
| A fixed width threshold or declared domain size | Honesty | A threshold fits one domain; a declared size is a guess treated as fact. |
| Treat an unknown class as unclassified | Failure direction | A typo silently downgrades protection. |

Consequences: a band and a joinable key need two columns; sufficient narrowness is unverified.
Revisit: manifests carry a measured domain size per class; a per-person join over an exhaustible class becomes a requirement.

## Placement breadth is declared data-side, and resolution fails narrow

Only the data side widens inference placement, every widening shows in the manifest diff, and ambiguity resolves narrowest. `authority.issue` refuses a wildcard inference zone on a credential or subject. `authority.place` refuses a permissive default for unlabeled tables when more than one principal owns the store. Widening a protected-class surface past its floor requires a class-named override flag, else it narrows at serve time. A synthesized row's zones are at most the intersection of its evidence tables'. Incognito pins the session, the local owner included. No model-vendor client library links into any workspace package.

| Option | Lost on | Cost |
| --- | --- | --- |
| Data-side breadth, intersection for synthesis, refusal of every widening assertion *(chosen)* | — | A multi-zone batch mints per zone; a second writing principal breaks a permissive default; one narrow evidence table narrows a whole conclusion. |
| Subject-side wildcard as every zone | Widening side | A credential grants itself every zone. |
| Trust a synthesized row's declared set, or the union | Laundering | The constrained path enforces its own constraint. |
| Honor a wider zone under incognito and audit it | Toggle guarantee | The audit reports a crossing the toggle exists to stop. |
| A vendor client behind a feature flag | Verifiability | The placement claim depends on a build the reader cannot see. |

Consequences: the override flag records an assertion and verifies no vendor agreement; a session needing a cloud model restarts without the pin.
Revisit: fragment-level provenance through synthesis becomes observable; per-zone minting dominates issuance.

## Redistribution ships an allowlist and never reaches backwards

`authority.bound-redistribution` fails toward not shipping and names what an earlier push uploaded. While any table is withheld, a root object ships only if allowlisted as free of row data — the mirrored permission tables and the stale-memory index; any other raises `EnforceRootObjectNotAllowlisted` at push. Clearing the redistribution flag over a table an earlier push uploaded raises `EnforceStaleRedistributedObjects`, naming each implicated object and its remedy, up to 100 files plus the total, and removes nothing; the check runs before and after the upload loop.

| Option | Lost on | Cost |
| --- | --- | --- |
| Enumerate what ships; refuse and name left-behind objects *(chosen)* | — | A new root artifact replicates only once listed; a retroactive bound needs an operator to clean the bucket by hand. |
| Name the objects that stay behind | Failure direction | A new object ships by default carrying withheld rows, unreported. |
| Inspect each root object for row values at push | Soundness | Record-blob contents are not statically decidable, and every push pays for the scan. |
| Delete left-behind objects automatically | Bucket authority | The engine is not the retention arbiter; deletion is irreversible and can cut a replica from its readers. |

Consequences: a push refuses until the bucket is clean; a replica keeps resolving permissions and staleness while tables are withheld.

## Local transports bind possession to the holder key, then to the OS peer

Context: MCP hosts spawn a server over stdio or relay to a Unix socket, and many sign nothing per request.

Decision: a local transport checks the holder key first: a credential carrying a confirmation thumbprint admits only with a proof from that key. A credential with no confirmation claim falls back to the kernel's peer authentication: an inherited stdio pipe, or a socket peer whose uid — `SO_PEERCRED` on Linux, `getpeereid` on macOS — equals the checkpoint's. A network checkpoint refuses a credential with no confirmation claim. An admission binds to its connection.

Criteria: theft yield, then host compatibility; theft yield decided the order.

| Option | Lost on | Cost |
| --- | --- | --- |
| Holder proof first, peer uid when no confirmation claim *(chosen)* | — | Two local admission paths; the fallback rests on a per-platform peer call, and the uid is its boundary. |
| Peer uid alone, `transport:stdio` confirmation on pipes | Theft yield | A key-holding client gains nothing from its key; any same-uid process acts with its credential. |
| Per-request proof on every transport | Host compatibility | Every local client needs a signing shim. |
| Peer process identity, by pid or audit token | Portability | No call common to Linux and macOS names the process; a pid is reusable. |

Consequences: a key-holding local client keeps network-grade theft resistance; a credential with no confirmation claim is local-only. Accepted cost: on the fallback, a same-uid process reading the host's configuration acts with the credential locally.
Revisit: stdio hosts sign per request, retiring the fallback; a portable call names the peer process.
