# A-topology — System topology decisions

**Status:** accepted

## One inference egress and one destination; no vendor SDK, no sink plugin

`connector.infer` sends model access through one operator-configured endpoint speaking OpenAI-compatible HTTP; no model-vendor SDK links into any crate, a provider swap is a URL edit, and the host owns credential, rate limit and per-call span. `run.land` resolves the local store and refuses any other destination name at assembly; a synthesized artifact writes back through the same destination and becomes queryable corpus. The destination interface has no host arm, so a guest cannot supply one.

| Option | Lost on | Cost |
| --- | --- | --- |
| One compatible endpoint, one destination *(chosen)* | — | A vendor feature outside the compatible wire is unreachable; an export is a copy of the store's own layout. |
| A vendor SDK per provider behind an abstraction | Dependency custody | Each SDK links its own HTTP stack into every profile and dictates auth and retry. |
| A plugin interface for backends or sinks | Attack surface | A loaded plugin holds credentials, and the destination set is no longer nameable. |
| A sidecar translating to each vendor's wire | Operational cost | A second deployable with its own credentials and failure modes. |
| A webhook destination | Delivery semantics | At-least-once messaging, which the run's commit model cannot honor. |

Consequences: the dependency audit refuses a model-vendor SDK and a second outbound path to a model; synthesized output falls under the same grants, masks and retention as landed data.
Revisit: a capability a deployment needs that no OpenAI-compatible endpoint exposes.

## Profile capabilities are fixed at build; an absent one is a typed refusal

Three profiles, `contextful-edge`, `contextful-full` and `contextful-control`, compile from one workspace by feature bundle, each linking only its role's dependencies; the CRDT library links into `contextful-control` alone. A capability the running profile does not wire is a typed refusal when reached, never a panic, no-op or look-alike fallback; a profile without the embedded SQL engine refuses both read tools. `topology.package` publishes each profile's wiring, a client's required faces match at the handshake, and a spawned engine finding no project manifest exits before writing protocol framing.

| Option | Lost on | Cost |
| --- | --- | --- |
| Build-time profiles; typed refusal on reach *(chosen)* | — | Three artifacts to build, size-gate and release; an absent capability surfaces at the handshake or first call. |
| One binary with run-time feature detection | Footprint | Every heavy dependency links into the replica. |
| Dynamically loaded plugins | Portability | Static musl targets, the edge deployment shape, load no plugins. |
| Degrade to a partial answer | Honesty of the outcome | A lexical-only ranking or an empty result reads as complete. |
| Separate repositories per artifact | Versioning | The shared domain and its crossings version across repositories. |

Consequences: the dependency audit refuses a control-only library in another profile's resolved graph.

## The engine and its consumers meet only at public surfaces

`topology.compose` admits exactly three crossings between read path and run path: the connector interface world, the columnar-part-plus-manifest layout, and capability tokens, each versioned with a conformance suite both sides run. The engine owns no domain; an application names its vocabulary in a per-store lexicon, and a behavior becomes a shared abstraction once a second application needs it. `topology.bound-application` gives the commercial layer only the public surfaces, and a paid feature is net-new, never a license check on a data-plane capability.

| Option | Lost on | Cost |
| --- | --- | --- |
| Three versioned crossings; public surfaces only *(chosen)* | — | A fourth crossing is a design question, and the commercial layer waits for each public surface it needs. |
| A general internal RPC surface | Review surface | Every call is a place a capability can enter. |
| Direct dependencies from read path onto run path | Composability | The run path stops being droppable and the edge profile is lost. |
| A privileged internal API for the commercial layer | Wholeness | The open engine becomes a subset of itself. |
| The first application's vocabulary as engine defaults | Inheritance | Every later application receives meanings it did not choose. |

Consequences: `assurance.gate` enforces these boundaries over the package graph and source, since they have no run-time trigger.

## A deploy verifies by observation

`surface.apply` runs one declaration on every control-plane target and diffs each target's table parts byte for byte against the single-process reference run after a 24 h soak; a profile claiming a shape its target cannot express refuses. `topology.publish-hostname` gives every hostname a descriptor naming its worker, contract version and gate (`access`, `adminToken` or `public`) with an explicit acknowledgement; the deploy probes each with an anonymous `GET /`, judges the answer against the gate, and fails an unreachable hostname. A descriptor with an unknown field refuses to decode.

| Option | Lost on | Cost |
| --- | --- | --- |
| Declare, then observe and compare *(chosen)* | — | A deploy waits on probes and a soak; a target-specific feature waits until it runs everywhere. |
| Derive posture from provider configuration | Recoverability of intent | Configuration records no gate, so an accidental opening reads as deliberate. |
| Declare with no probe | Drift | A policy attached or detached later changes exposure with no signal. |
| A periodic external monitor | Timing | The exposure window is the monitor's interval. |
| Per-target expected outputs | Detectability | A divergence arrives as a fixture update, which reviewers approve. |

Consequences: a divergence names the target, the table and the first differing part.

## The domain crate holds types and ports with no I/O, and every edge points toward it

`topology.package` keeps `contextful-core`, the shared types, typed error enum and port traits linked into all three profiles, pure: it performs no I/O and declares no async runtime, component host, HTTP client or columnar-format implementation, and one raises `DomainCrateImpurity`, naming the dependency and the feature that pulled it. Every adapter crate depends on the centre and the centre on none; an outward edge raises `TopologyDependencyInversion` with its manifest line.

| Option | Lost on | Cost |
| --- | --- | --- |
| A pure centre of types and ports, all edges inward *(chosen)* | — | Adapters restate small conversions; a port change touches every adapter in one commit. |
| A centre carrying its own runtime and format implementation | Footprint | The replica and control plane link a runtime and format neither uses. |
| A shared utility crate both directions depend on | Direction reportability | A cycle re-enters legally at every hop, with no single offending edge. |
| Ports beside their adapters, no centre | Neutral home | The error enum crossing both halves has nowhere to live. |

Consequences: a new target is an adapter crate behind existing ports.
Revisit: two adapters convert the same shape differently; a port change deferred more than once; the edge profile's budget no longer the binding constraint.
