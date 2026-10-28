# A-surface — Operator surfaces decisions

**Status:** accepted

## control-document-cas — The control document is CAS-versioned, validated per entry, and fails static

One hand-edited control document arms every scheduled entry unattended, so one bad entry or failed poll stops no unrelated schedule.

| Option | Lost on | Cost |
| --- | --- | --- |
| CAS on an engine version, per-entry validation, fail-static poll *(chosen)* | — | A loser's edits land on a document they never read; a silent control plane leaves an old cadence running. |
| Last-write-wins on the pointer | Immutability | An applied version is superseded unobserved. |
| A lock or lease in front of the document | Writer count under failure | An expiring lease admits an unordered second writer. |
| Arm empty, or fall back to local schedules, on a failed poll | Visibility of the failure | A transient error stops ingestion or runs an unapplied cadence while reporting healthy. |

## configuration-reaches-engine — Configuration reaches only engine-named code and secrets

Configuration selects among things the engine names, never code, a host command, a secret or a remote control source.

| Option | Lost on | Cost |
| --- | --- | --- |
| Closed job union, derived names, loopback control source *(chosen)* | — | A new job kind needs a release; renaming a store rotates its secret name. |
| A job carrying an argument vector, or a plugin registry of kinds | Trust boundary | A manifest writer gains arbitrary execution with the store's credentials and egress. |
| Authored credential and binding names per entry | Reachable secrets | A runtime edit sends any worker secret to a chosen host. |
| A remote control URL over TLS with a bearer | Content authenticity | The server, or anyone who compels or replays it, pins or rolls back the schedule set. |

Revisit: a remote control source with a bearer-authenticated read, TLS, and producer-side signing over `(version, content-hash)` verified before arming.

## answer-surfaces-read — Answer surfaces read through server-chosen, grounded calls

The server decides every capability a turn exercises, so an injected instruction finds no write tool to reach for.

| Option | Lost on | Cost |
| --- | --- | --- |
| Server-chosen calls, recall as the one door, server-built views *(chosen)* | — | A caller-initiated write needs another surface; no third-party component ships. |
| Client-named tools against an allowlist | The adversarial body | Arguments arrive unvalidated, so the weakest tool sets the boundary. |
| Writes behind a reader confirmation | Who confirms | The steered reader confirms inside the steered answer. |
| One generic query tool the model fills in | Enumerability | No audit can say what a turn can do. |
| Memory exposed to the planner with a documented predicate | The checks | A predicate the model writes is one it can omit. |

## query-admin-use — Query and Admin use separate verified page grants

Page admission and engine capability checks both stay necessary.

| Option | Lost on | Cost |
| --- | --- | --- |
| Cloudflare Access or Cognito managed login *(chosen)* | — | Cognito needs a user pool, group mapping and first-party sessions. |
| One shared password for each page | Individual accountability | Two people become one subject in grants and audit records. |
| Self-managed operator passwords | Credential lifecycle | The console owns hashing, resets, lockout and recovery. |
| Cloudflare Access only | Deployment portability | An organization without Cloudflare cannot host the console. |

## console-lives-beside — The console lives beside the engine and uses its public read face

The console owns presentation and turn orchestration; engine enforcement stays in the engine, so no console path implements a second read policy.

| Option | Lost on | Cost |
| --- | --- | --- |
| Separate TypeScript console and client library *(chosen)* | — | Two runtimes need a shared protocol and coordinated release. |
| The console embedded in the Rust CLI | Browser delivery | The engine binary carries page assets and model-facing session code. |
| The browser calling the engine directly | Credential custody | A page holds store credentials and bypasses the server's turn choices. |

## console-page-ships — Each console page ships as one prerendered, self-contained document

Each page is one HTML document served under `default-src 'none'` plus `font-src data:`, so the console exposes no asset route to gate.

| Option | Lost on | Cost |
| --- | --- | --- |
| Prerendered self-contained documents *(chosen)* | — | Each page response carries about 750 KiB under `no-store`. |
| Hashed assets under gated asset routes | Route surface | Every asset route needs the page grant check and its own cache policy. |
| Hand-written HTML strings in the server | Component reuse | Every widget is rebuilt by string concatenation. |
| A full-stack page framework | Server ownership | The framework's router replaces the server that verifies page grants. |

## turn-compatibility-preserves — Turn compatibility preserves the stored transcript and reader scope

A transcript stays readable across client releases, and one accountable reader defines each answer.

| Option | Lost on | Cost |
| --- | --- | --- |
| Preserve subjects, scope and older views at their readers *(chosen)* | — | Recall does entity work; a newer widget loses fidelity on an older client. |
| Normalize subjects and components when writing | Reversibility | Old transcripts cannot recover their original labels or view shapes. |
| The overlay and room audience given to the planner | Capability scope | Store-authored text and a composite principal widen the turn. |

## store-driven-job — A store-driven job runs host-registered code per row over a pinned input

No other job kind takes store rows as input, so without this kind per-row paid work over a snapshot has no resumable home.

| Option | Lost on | Cost |
| --- | --- | --- |
| Fixed kind, registered body, pinned input *(chosen)* | — | A new body is a host release; the operator states concurrency for every job. |
| A body as an argument vector | Trust boundary | A manifest writer gains execution with the store's credentials and egress. |
| Input re-read on each resume | Resume determinism | A fold or later landing changes the row set mid-run. |
| A default `max_in_flight` | Spend visibility | Paid concurrency rises with no line in the manifest. |

## residency-divergence-detected — Residency divergence is detected at push, from the bucket manifest

Each site resolves its own `[residency]`, so two sites sharing a bucket can each start cleanly with different allow-sets.

| Option | Lost on | Cost |
| --- | --- | --- |
| The set rides the bucket manifest, checked at push *(chosen)* | — | A read-only site goes uncompared; a policy change starts at the recording site. |
| A separate policy object in the bucket | Consistency | A second object commits apart from the manifest. |
| A comparison at startup reading the bucket | Air-gapped operation | Startup needs the bucket, which `topology.coordinate.air-gap` forbids. |
| A central registry of site policies | Coordination footprint | A service the tree does not ship holds the truth. |

## workers-reach-orchestrator — Workers reach the orchestrator only through the awakeable route

A remote worker reports liveness and outcome through no crossing beyond the three the topology names.

| Option | Lost on | Cost |
| --- | --- | --- |
| The awakeable route, attempt-fenced and signed *(chosen)* | — | A worker needs a route to the relay; one key signs every worker's messages. |
| A worker-pull queue on the orchestrator | Crossing count | A fourth inbound surface with its own authentication. |
| Unfenced callbacks, last writer wins | Successor safety | A partitioned worker's late result replaces its successor's. |
| A pool bound per exclusion key | Concurrency cap | Adding pipelines raises concurrent pulls unseen. |

## synced-control-head — A synced control head carries an issuer-signed apply receipt

Node-owned run states and local version numbers prove no admin apply. An installation without sync keeps unsigned applies local.

| Option | Lost on | Cost |
| --- | --- | --- |
| Signed receipt and project-scoped manifest head *(chosen)* | — | A synced apply needs an admin credential and signing port. |
| Version in node run state | Authorship | Any node writer claims a version; two nodes reuse one number. |
| The bucket manifest's control entry trusted as is | Content authenticity | A data writer publishes schedules without an admin apply. |
| The local snapshot directory replicated directly | Claim ordering | Local version counters collide; a copied pointer carries no authorization. |

Revisit: a bucket writer replays an earlier signed head to a never-synced node; freshness needs an independent monotonic witness.

## native-model-builds — Native model builds share applied cadence and execution history

Cooperative stop admission and model pointer CAS give no atomic cross-host fencing.

| Option | Lost on | Cost |
| --- | --- | --- |
| Applied model definitions in native cadence *(chosen)* | — | Each control version carries resolved SQL; source ordering is explicit. |
| Standalone build over mutable files | Applied identity | Draft SQL changes the running schedule. |
| A separate model scheduler | One execution home | Cadence, exclusion and restart history have two implementations. |

## applied-jobs-retain — Applied jobs retain structure and check current authority

Applied structure fixes what a job reads and lands; current authority still decides admission, checked once at attempt start, not throughout the attempt.

| Option | Lost on | Cost |
| --- | --- | --- |
| Captured structure, current-authority comparison *(chosen)* | — | Snapshots missing contracts need full apply; policy changes can refuse attempts. |
| Output contracts resolved from live files | Replay identity | Unapplied edits alter deduplication, types and landing. |
| Replay entirely under historical authority | Revocation | A snapshot keeps access the current declaration removed. |
| Pending owners rewound automatically | Recorded effects | A structural edit repeats paid calls. |

## delegated-body-deploy — A delegated body is deploy-bound code reached in two modes

A consumer whose domain code is not Rust has no body to register, so without a delegate its paid work runs outside the journal.

| Option | Lost on | Cost |
| --- | --- | --- |
| Named delegate, two modes, closed outcomes *(chosen)* | — | A second body path; a hanging `call` delegate holds its slot until timeout. |
| Registered bodies only | Consumer reach | Domain code in another language runs outside the record. |
| A URL in the manifest | Trust boundary | A manifest writer gains egress with the store's credentials. |
| One mode alone | Delegate classes | `submit` cannot reach a Worker-class delegate; `call` has no home for long work. |

## due-units-admit — Due units admit by staleness, and a wake refills its freed slots

Overload misses rotate across a tied cohort.

| Option | Lost on | Cost |
| --- | --- | --- |
| Staleness tie-break, catalog record, refill inside the wake *(chosen)* | — | One catalog write per dispatch; a unit due after evaluation waits for the next wake. |
| Name tie-break | Fairness under overload | Every miss lands on the name-last units. |
| Strict priority across due instants | Starvation | A busy high class holds the pool. |
| Dispatch record in process memory | Fresh processes | A unit failing before its run row wins every cycle at pool 1. |
| Refill from a fire's end path between wakes | Lease safety | A start after the lease lapsed runs beside another holder. |

## never-run-first — A unit with no run and no dispatch starts ahead of every due instant

Such a unit falls due at the instant its process armed it, and a serve started on every wake arms it anew at each start.

| Option | Lost on | Cost |
| --- | --- | --- |
| Rank it ahead of every due instant *(chosen)* | — | A newly applied unit precedes overdue units for one fire each. |
| Order it by its arming instant | Restarting processes | Each start re-arms the unit behind every backlogged unit, so it never starts. |
| Due at its schedule's latest slot before arming | Persistent backlogs | Each restart moves that slot past every unit still due from an earlier one. |
| Persist its first arming instant | Stateless arming | One more catalog row per unit, read on every arm. |

Revisit: when a newly applied unit must wait behind overdue ones, persist each unit's first arming instant beside its dispatch record.

## connections-claim-version — Connections claim one version and share dispatch

A consent callback can retry while other connections mutate the same control document.

| Option | Lost on | Cost |
| --- | --- | --- |
| Versioned connection claim and shared dispatch *(chosen)* | — | Each connection adds a control entry and a due-history record. |
| One scheduler thread per connection | Capacity | Thread count grows with tenant count. |
| An unversioned connection map | Concurrent edits | Retried callbacks overwrite a different declaration. |
