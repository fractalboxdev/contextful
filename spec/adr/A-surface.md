# A-surface — Operator surfaces decisions

**Status:** accepted

## The control document is CAS-versioned, validated per entry, and fails static

One hand-edited control document arms every scheduled entry unattended. `surface.apply` claims a version by compare-and-swap on an engine-assigned version; a loser raises `ManifestVersionConflict`, reloads and reapplies. The engine owns the control-state model and raises `ConfigOwnerUnconfigured`, `StoreNotInitialized` or `ConditionalWriteUnsupported` rather than substitute a local writer. `surface.arm` holds back an invalid entry alone, by name. `surface.reconcile` keeps the armed set unchanged on a failed poll. `surface.dispatch` starts one instance per due unit; a dependent run's step refuses as a unit.

| Option | Lost on | Cost |
| --- | --- | --- |
| CAS on an engine version, per-entry validation, fail-static poll *(chosen)* | — | A loser's edits land on a document they never read; an entry can sit unarmed with only a diagnostic; a silent control plane leaves an old cadence running. |
| Last-write-wins on the pointer | Immutability | An applied version is superseded unobserved by one that never saw it. |
| A lock or lease in front of the document | Writer count under failure | An expiring lease admits an unordered second writer. |
| Abort the reconcile on one bad entry | Blast radius | One typo stops every unrelated schedule. |
| Arm empty, or fall back to local schedules, on a failed poll | Visibility of the failure | A transient error stops ingestion or runs an unapplied cadence while reporting healthy. |

Consequences: entries authored as a chain run as one unit under the head entry's cadence; control-plane staleness is visible only in diagnostics.

## Configuration reaches only engine-named code and secrets

Configuration selects among things the engine names, never code, a host command, a secret or a remote control source. `surface.fire` runs a closed union — `sweep`, `build`, `fold`, `rebuild-catalog`, `sync-push`, `validate` — beside the pipeline-run kind; anything else raises `JobKindUnknown`. `surface.register-store` derives the secret name `<ID>_QUERY_TOKEN` and binding name from the kebab-case id; authoring either raises `StoreNameAuthored`. `surface.reconcile` polls a control URL only on loopback, following no redirect and no proxy; any other host raises `ControlSourceNotLoopback`.

| Option | Lost on | Cost |
| --- | --- | --- |
| Closed job union, derived names, loopback control source *(chosen)* | — | Maintenance outside six kinds needs a release; renaming a store rotates its secret name; multi-host control waits on signed snapshots. |
| A job carrying an argument vector, or a plugin registry of kinds | Trust boundary | A manifest writer gains arbitrary execution with the store's credentials and egress. |
| Authored credential and binding names per entry | Reachable secrets | A runtime edit names any worker secret and sends it to a chosen host. |
| A remote control URL over TLS with a bearer | Content authenticity | The server, or anyone who compels or replays it, pins or rolls back the schedule set. |

Consequences: adding a job kind is an exhaustive-match obligation at every dispatch site.
Revisit: a remote control source carrying a bearer-authenticated read, TLS, and producer-side signing over `(version, content-hash)` verified before arming.

## Answer surfaces read through server-chosen, grounded calls

The server decides every capability a turn exercises. `surface.ground` dispatches only tools the turn's packs admit, else `ConsoleToolNotAdmitted`; an organization-wide face registers no write tool, and a turn's one write is server-authored. `surface.ground` reaches conclusions only through recall. `surface.render` infers views server-side over a closed component union. `surface.speak` offers only answerable suggestions. `surface.brief` renders a greeting only within budget. `surface.publish-answer` delivers to the asker alone; posting is the asker's separate act.

| Option | Lost on | Cost |
| --- | --- | --- |
| Server-chosen calls, recall as the one door, server-built views *(chosen)* | — | A caller-initiated write needs another surface; no third-party component ships; a broken greeting shows only in spans. |
| Client-named tools against an allowlist | The adversarial body | Arguments arrive unvalidated, so the weakest tool sets the boundary. |
| Writes behind a reader confirmation | Who confirms | The steered reader confirms inside the steered answer. |
| One generic query tool the model fills in | Enumerability | No audit can say what a turn can do. |
| Memory exposed to the planner with a documented predicate | The checks | A predicate the model writes is one it can omit. |

Consequences: an injected instruction finds no write tool to reach for.

## Query and Admin use separate verified page grants

The hosted operator console gives Query and Admin separate page grants and API namespaces. Cloudflare Access is the default gate, with a distinct application audience per page. Without it, Amazon Cognito manages named operator accounts and password login; the console holds a first-party session and maps Cognito groups to page grants. The console verifies each page and API request. Query admits the read subset; Admin sends edit or apply through a server-held control capability. The deployment probe requests each path anonymously; a hostname-level probe misses an exposed path.

| Option | Lost on | Cost |
| --- | --- | --- |
| Cloudflare Access or Cognito managed login *(chosen)* | — | Cognito needs a user pool, group mapping and first-party session handling. |
| One shared password for each page | Individual accountability | Two people become one subject in query grants and audit records. |
| Self-managed operator passwords | Credential lifecycle | The console must own hashing, resets, lockout and recovery. |
| Cloudflare Access only | Deployment portability | An organization without Cloudflare cannot host the console. |

Consequences: page admission and engine capability checks both remain necessary; local shells use their own perimeter.

## The console lives beside the engine and uses its public read face

`apps/console` contains the TypeScript server and Query and Admin pages; `packages/client` contains the four client shapes. The console calls stores through the public HTTP face and the operator's model endpoint. It owns presentation and turn orchestration; engine enforcement stays in the engine. The TypeScript surfaces gate builds both packages, and milestone acceptance drives the built engine beside the console server.

| Option | Lost on | Cost |
| --- | --- | --- |
| Separate TypeScript console and client library *(chosen)* | — | Two runtimes need a shared protocol and coordinated release. |
| Embed the console in the Rust CLI | Browser delivery | The engine binary takes on page assets and model-facing session code. |
| Let the browser call the engine directly | Credential custody | A page holds store credentials and bypasses the server's turn choices. |

Consequences: Query and Admin use the same store registry while retaining separate page grants; no console path implements a second read policy.

## Turn compatibility preserves the stored transcript and reader scope

An explicit process owner flag needs a signed owner claim bound to the selected local store; it does not turn an ordinary or empty credential into ownership. A store overlay reaches synthesis text alone, so planner tool selection does not inherit a store-authored persona. Distillation records the observed subject and recall resolves it through entity matching. A client with an older component union renders an unknown view as a table. One operator's credential defines an answer; a room-intersection principal is absent.

| Option | Lost on | Cost |
| --- | --- | --- |
| Preserve subjects, scope and older views at their respective readers *(chosen)* | — | Recall does entity work, and a newer widget loses fidelity on an older client. |
| Normalize subjects and components when writing | Reversibility | Old transcripts and learnings cannot recover their original labels or view shapes. |
| Give the planner the overlay and room audience | Capability scope | Store-authored text and a composite principal can widen the turn without one accountable reader. |

Consequences: a transcript remains readable across client releases; shared posting remains an operator act.

## A store-driven job runs host-registered code per row over a pinned input

**Status:** accepted; amends the configuration decision above: the fire union gains one fixed store-driven kind, and a job block still names no command.

Context: every run opens against a connector plan, and no job kind takes store rows as its input, so per-row paid work over a snapshot has no resumable home. Criteria: configuration reaches only engine-named code; a resume never repeats a recorded call; a resume against changed input refuses.

Decision: one store-driven kind joins the fire union. Its per-row body resolves only to compiled code the host registers, named by the manifest, never an argument vector. Its input is a statement read through `Face` under the job's grant at an `as_of` resolved once at open; statement and `as_of` join the plan hash, so `run.journal.plan-pin` holds a resume to them. `max_in_flight` is required, with no default.

| Option | Lost on | Cost |
| --- | --- | --- |
| Fixed kind, registered body, pinned input *(chosen)* | — | A new body is a host release, and every dispatch site matches one more kind. |
| A body as an argument vector | Trust boundary | A manifest writer gains execution with the store's credentials and egress. |
| Input re-read on each resume | Resume determinism | A fold or a later landing changes the row set mid-run. |
| A default `max_in_flight` | Spend visibility | Paid concurrency rises with no line in the manifest. |

Consequences: a job over store rows resumes without paying twice. The accepted cost: the operator states concurrency for every such job, and the closed union grows to seven kinds.

## Residency divergence is detected at push, from the bucket manifest

**Status:** accepted

Context: each site resolves its own `[residency]`, so two sites sharing a bucket can declare different allow-sets and each start cleanly under `surface.reside.region-mismatch`. Criteria: detection before a divergent write lands; no coordination service; no new artifact to keep consistent.

Decision: every push writes its site's sorted allow-set into the bucket manifest it already commits by compare-and-set, and a push finding a differing set recorded by another site, or none of its own against a recorded one, raises `ResidencySitesDiverge` before its manifest commit.

| Option | Lost on | Cost |
| --- | --- | --- |
| The set rides the bucket manifest, checked at push *(chosen)* | — | A site that never pushes is never compared; a policy change starts at the site holding the record, which the refusal names. |
| A separate policy object in the bucket | Consistency | A second object commits apart from the manifest and can disagree with it. |
| Compare at startup by reading the bucket | Air-gapped operation | Startup then needs the bucket reachable, which `topology.coordinate.air-gap` forbids. |
| A central registry of site policies | Coordination footprint | A service the tree does not ship holds the truth. |

Consequences: a divergence surfaces on the first push after it arises, with both sites and both sets named. The accepted cost: a read-only site holding a different set goes undetected, and a policy change starts at the recording site, and every other site is refused until it adopts the new set.

## Workers reach the orchestrator only through the awakeable route

**Status:** accepted

Context: a landing step runs on a remote worker, which reports liveness and outcome back to the operator plane. Criteria: no crossing beyond the three the topology names; a late result never overwrites its successor's; one pool bound per deployment.

Decision: `surface.dispatch` submits each step to a worker's `POST /submit`, keyed by run, step and attempt. The worker heartbeats with a signed `GET /awake/:token` and posts its outcome with a signed `POST` there, the HMAC covering run, step, attempt and timestamp under one deployment key. The orchestrator signs each submission under the same key, and a worker calls back only to its configured relay. The relay fences attempt and skew, closed steps included; a lapse moves the step under the next attempt. The pool bound stays per deployment, one unit in flight per exclusion key.

| Option | Lost on | Cost |
| --- | --- | --- |
| The awakeable route, attempt-fenced and signed *(chosen)* | — | A worker needs a route back to the relay; one key signs every worker's messages. |
| A worker-pull queue on the orchestrator | Crossing count | A fourth inbound surface with its own authentication. |
| Unfenced callbacks, last writer wins | Successor safety | A partitioned worker's late result replaces its successor's. |
| A pool bound per exclusion key | Concurrency cap | Adding pipelines raises concurrent pulls unseen. |

Consequences: a silent worker costs one lapse before its step moves. The accepted cost: a worker that cannot reach the relay runs steps it cannot report.

## A synced control head carries an issuer-signed apply receipt

**Status:** accepted

Context: node-owned run states and local version numbers prove no admin apply. Criteria: authenticated content, air-gapped local use and ordered claims.

Decision: a synced import or apply admits an admin capability and signs a receipt binding project, version, predecessor and snapshot digests before advancing its pointer. Push conditionally commits the receipt chain and one project-scoped head in the bucket manifest, refusing divergent heads. Pull leaves the local pointer untouched. The reconciler verifies the issuer signature against local key pins and validates the snapshot against local declarations before adoption. An installation without sync keeps unsigned applies local.

| Option | Lost on | Cost |
| --- | --- | --- |
| Signed receipt and project-scoped manifest head *(chosen)* | — | A synced apply needs an admin credential and signing port; replicas verify a chain before arming. |
| Version in node run state | Authorship | Any node writer can claim a version, and two nodes can use the same number for different documents. |
| Trust the bucket manifest's control entry | Content authenticity | A data writer can publish schedules without an admin apply. |
| Replicate the local snapshot directory directly | Claim ordering | Independent local version counters collide, and a copied pointer carries no authorization proof. |

Consequences: a cold node authenticates the bucket head. A bucket writer can replay an earlier signed head to a never-synced node; freshness needs an independent monotonic witness.
