# A-topology — System topology decisions

**Status:** accepted

## cloudflare-d1 — Cloudflare execution metadata uses D1

The deciding criterion is one metadata backend across the deployment configuration, the execution binding and its conformance suite. Durable Objects still manage Container lifecycle; D1 replaces only the catalog role.

| Option | Lost on | Cost |
| --- | --- | --- |
| D1 metadata with R2 objects *(chosen)* | — | Metadata operations cross the private Worker binding. |
| D1 and Durable Object SQLite variants | Backend consistency | Two catalog integrations and conformance paths. |

## one-egress — One inference egress and one destination; no vendor SDK, no sink plugin

A provider swap is a URL edit, and the host owns credential, rate limit and span. Synthesized output falls under the same grants, masks and retention as landed data.

| Option | Lost on | Cost |
| --- | --- | --- |
| One compatible endpoint, one destination *(chosen)* | — | A vendor feature outside the compatible wire is unreachable. |
| A vendor SDK per provider | Dependency custody | Each SDK links its own HTTP stack and dictates auth and retry. |
| A plugin interface for backends or sinks | Attack surface | A loaded plugin holds credentials; the destination set is unnameable. |
| A webhook destination | Delivery semantics | At-least-once messaging, which the commit model cannot honor. |

Revisit: a capability a deployment needs that no OpenAI-compatible endpoint exposes.

## build-profiles — Profile capabilities are fixed at build; an absent one is a typed refusal

A capability the running profile does not wire is a typed refusal when reached, never a panic, no-op or look-alike fallback.

| Option | Lost on | Cost |
| --- | --- | --- |
| Build-time profiles; typed refusal on reach *(chosen)* | — | Three artifacts to build, size-gate and release. |
| One binary with run-time feature detection | Footprint | Every heavy dependency links into the replica. |
| Dynamically loaded plugins | Portability | Static musl targets load no plugins. |
| Degrade to a partial answer | Honesty of the outcome | A lexical-only ranking or empty result reads as complete. |

## control-crdt — The control plane edits the configuration as one CRDT document and claims through the shared snapshot package

Team state is the configuration itself, so replicas merge offline edits to it, and local and control-plane applies share one version claim, pointer and replay store.

| Option | Lost on | Cost |
| --- | --- | --- |
| One CRDT document; one shared snapshot package *(chosen)* | — | Concurrent edits can merge into invalid TOML, which apply refuses before claiming. |
| A team-state model beside the configuration | Single source | Two documents to reconcile on every apply. |
| A copy of the snapshot code in the control package | One home | Two claim paths whose version races drift apart. |
| The engine linked into the control profile | Profile role | The control profile links the run path. |

## public-surfaces — The engine and its consumers meet only at public surfaces

Each shared contract has a version and conformance tests. A behavior becomes shared once a second application needs it; no license check guards a data-plane capability.

| Option | Lost on | Cost |
| --- | --- | --- |
| Three versioned contracts; domain-neutral engine *(chosen)* | — | A new shared contract needs review; a consumer supplies its own domain meanings. |
| A general internal RPC surface | Review surface | Every call is a place a capability can enter. |
| A privileged internal API for one consumer | Wholeness | Other consumers receive a subset of the engine. |
| The first application's vocabulary as defaults | Inheritance | Later applications receive meanings they did not choose. |

## deploy-observation — A deploy verifies by observation

Each hostname descriptor names its worker, contract version and gate with an explicit acknowledgement, and the deploy judges an anonymous `GET /` against that gate.

| Option | Lost on | Cost |
| --- | --- | --- |
| Declare, then observe and compare *(chosen)* | — | A deploy waits on probes and a soak. |
| Posture derived from provider configuration | Recoverability of intent | Configuration records no gate, so an accidental opening reads as deliberate. |
| A periodic external monitor | Timing | The exposure window is the monitor's interval. |
| Per-target expected outputs | Detectability | A divergence arrives as a fixture update, which reviewers approve. |

## posture-probe — The posture probe runs in build tooling; the gateway caches nothing

The gateway sits in front of a retrieval container that enforces grants per request, so a cached answer comes from inside the container, after enforcement. Profile footprint decided where the probe runs.

| Option | Lost on | Cost |
| --- | --- | --- |
| Probe in `contextful-ci`, no hop cache *(chosen)* | — | Every request crosses to the container; a pipeline that skips the probe goes unprobed. |
| Probe as an engine subcommand | Footprint | Every profile links a client only a deploy uses. |
| Probe inside the gateway at start | Timing | A gateway answers traffic before its posture is judged. |
| A short hop cache keyed on the enforcement subject | Mediation | A revoked grant still answers from the hop until expiry. |

## domain-crate — The domain crate holds types and ports with no I/O, and every edge points toward it

| Option | Lost on | Cost |
| --- | --- | --- |
| A pure centre of types and ports, edges inward *(chosen)* | — | Adapters restate small conversions; a port change touches every adapter. |
| A centre with its own runtime and format | Footprint | The replica and control plane link a runtime neither uses. |
| A shared utility crate both directions use | Direction reportability | A cycle re-enters legally, with no single offending edge. |
| Ports beside their adapters, no centre | Neutral home | The shared error enum has nowhere to live. |

Revisit: two adapters convert the same shape differently; a port change deferred more than once.

## enforcement-stack — Every access to data traverses the enforcement stack, and no configuration disables it

No flag, environment variable or build feature disables access controls. A surface holding the only handle to data reaches it through an adapter over the stack.

| Option | Lost on | Cost |
| --- | --- | --- |
| Mediation as a build property *(chosen)* | — | A bulk export or offline diagnostic is slower than a direct read. |
| An operator flag marking a trusted path | Failure mode | A flag set once stays set while the deployment looks healthy. |
| Per-surface opt-in enforcement | Coverage | Every new surface is a fresh chance to omit it. |
| Enforcement asserted by review alone | Detectability | A bypass reads as a call-site move in a refactor. |

Revisit: an adapter through the stack misses a stated throughput requirement.

## export-committed — Export reads committed runs and is no second destination

`_ingested_at` is stamped per run, so a cursor over it skips a run that commits late under an earlier stamp. Export target secrets resolve and leave through the same egress as every other secret.

| Option | Lost on | Cost |
| --- | --- | --- |
| Commit-sequence cursor *(chosen)* | — | A commit waits for every lower value to publish; a target sees duplicates on recovery. |
| Export as a second land destination | Delivery semantics | A failing target fails the landing run. |
| A cursor over `_ingested_at` | Skipped rows | A late run under an earlier stamp is skipped. |
| A sink plugin per backend | Credential custody | Loaded code holds target credentials. |

## commit-lock — A commit holds the commit lock to reserve its value and to publish, not to write

A lock spanning the Parquet write serializes a table's landings. A commit instead locks a ticket file naming its value while it writes parts; a dead holder's ticket unlocks with its process, leaving a gap.

| Option | Lost on | Cost |
| --- | --- | --- |
| Lock to reserve and publish, ordered by ticket *(chosen)* | — | A slow part write delays later publications; a vintage table stays serial. |
| Lock over the part write | Throughput | A table's landings write parts serially. |
| Tickets without liveness | Dead landings | A dead ticket holder stalls later commits. |
| Readers skip unpublished values | Prefix | An export cursor passes a value that publishes later. |

## queue-ingress — A managed queue extends ingress behind a port; the writer stays in the engine

On a container with an ephemeral disk, the local spool loses a batch on eviction and a cold boot blocks every sender. Single-node and edge shapes keep no external queue.

| Option | Lost on | Cost |
| --- | --- | --- |
| Queue port, managed queue where declared *(chosen)* | — | A sender wanting the outcome reads it by batch key; an expired key ends a batch as a refusal. |
| Local spool on every shape | Durability | An evicted container loses a spooled batch. |
| A managed queue on every shape | One tree | Laptop and edge shapes gain an external process. |
| Store writes in queue consumers | One writer | Commit ordering rests on locks a consumer does not hold. |

## catalog-read-add-on — A catalog query engine over Iceberg exports is an optional read face beside the Container

A Container query face that sleeps pays a full hydration on its next read: 145 s for 1 GB in 8,157 objects, answering `503` until ready. Basin SQL over an Iceberg export of 10M rows answers a first read in 3.6 s and a warm one in 0.7 to 1.6 s at p50; a warm Container over 1.27M rows answers in 0.2 to 0.4 s. Read latency, idle cost and the frontier a reader sees decide.

At Workers Paid list prices, an always-on `standard-1` Container costs about $27.5 a month whatever its query count. Basin SQL bills $2.50 per TB scanned with a 10 MB minimum per query, so a small query costs $25 per million, beside R2 writes and retained snapshot copies. They meet near 1.1 million queries a month. A 4,000-file table raises warm p50 1.2 to 2.1 times against 40 files and takes 24 s cold, so file size bounds latency and cost.

| Option | Lost on | Cost |
| --- | --- | --- |
| Container face for all, a catalog engine per exported audience *(chosen)* | — | An opted-in audience reads a copy 39 to 54 s behind a 5.1 GB fold. |
| Container face alone | Idle cost | A sporadic audience keeps a Container warm or waits minutes on a first read. |
| Catalog engine for every caller | Read latency | Hot callers pay 2 to 5.2 times the warm p50. |
| A catalog over the store's own files | P5 | Any engine skips the caller's relation. |

Revisit when a catalog engine's warm p50 falls below 300 ms.
