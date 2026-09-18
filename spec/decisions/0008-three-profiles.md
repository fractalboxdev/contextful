# 0008 — Three build profiles compile from one workspace, each linking the dependencies its role names

**Status:** accepted 2026-09-18
**Decides:** `topology.package.refusal.component-host`

## Context

Three roles want the same source tree and nothing like the same dependency set. A read
replica pulls native connectors, syncs manifest and table parts from a bucket, and serves a
read-only SQL replica — and it wants to run on a function-class target, which fixes both a
compressed size and an idle resident set it has to stay under. A daemon runs pipelines and
serves agent retrieval, which means the execution core, the in-process scheduler, the SQL
query face, the transform stages, the component host, the full-text sidecar, the vector
sidecar, the embedding backend and the tool server. A control plane holds team state, the
edit-time collaborative configuration document and identity.

The arithmetic settles it before preference does. The SQL engine, the transform library,
the component host, both sidecars and the tool server do not fit under a function-class
artifact while also serving daemon retrieval. There is no single artifact that is both the
replica and the daemon.

That in turn decides where a component connector can run. A component host is linked by one
profile, so a dispatch of a component connector on a profile without one has nothing to
execute it. The tempting recovery — falling back to a native source whose name resembles
the connector's — substitutes different code for the connector the caller asked for, and
does it silently.

## Decision

Three profiles compile from the one workspace and are selected at build time by Cargo
feature bundle: `contextful-edge`, `contextful-full`, `contextful-control`. Each links the
dependencies its role names and nothing beyond them, and a profile is fixed when the
artifact is built — no run-time detection widens a running binary into another profile's
feature set. A component connector executes where a component host is linked, which is the
full profile and the container or delegated worker shapes built from it. A dispatch of a
component connector on a profile that links no host raises `ComponentHostMissing`, naming
the connector and the profile, rather than falling back to a native source of a similar
name.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Three feature-bundle profiles from one workspace** *(chosen)* | A function-class artifact is reachable, and the same tree still produces a full daemon. One version, one set of crossings, one domain crate. | Three build matrices, and the profiles resolve the same crates under different feature unifications, so a bug appearing under one combination can pass a gate that builds a superset. |
| One binary with run-time feature detection | One artifact to build, ship, document and support. | Lost on footprint: unused heavy dependencies still link, so the replica carries the component host and both sidecars and misses its budget by a wide margin. |
| Separate repositories per artifact | Each artifact's dependencies are obviously its own; no feature unification. | Lost on versioning: the domain crate and the three crossings would need a cross-repo negotiation on every change, turning a single commit into a release sequence. |
| A dynamically loaded plugin model | Heavy capability added at deploy time rather than build time; one small base artifact. | Lost on portability against static musl targets, which are the deployment shape the edge and cluster cases depend on. |

## Criteria

1. **Footprint arithmetic** — whether a function-class artifact and a full daemon feature
   set can be the same binary. **This criterion decided.** The heavy dependencies of the
   daemon exceed the replica's budget by construction, so no amount of run-time cleverness
   produces one artifact that satisfies both; the split is forced rather than preferred.
2. **Single-version coherence** — whether the domain crate and the three crossings stay in
   one commit.
3. **Portability** — whether the artifacts target static musl without dynamic loading.
4. **Build and test cost** — three matrices against one. The chosen option is the worst
   here and lost this criterion knowingly.

## Consequences

Each profile's dependency lattice is stated and auditable: the SQL engine read-only into
edge and fully into full; the transform library, the component host, both sidecars, the
embedding backend, the tool server, the execution core and the scheduler into full alone;
native connectors and bucket sync into edge and full. A missing host is reported at
dispatch with the connector and profile named, so the operator learns they built the wrong
artifact rather than silently receiving a different source's output.

The cost accepted: three build matrices, three artifact sets, three budgets to measure. And
because the profiles resolve the same crates under different feature unifications, a bug
that appears only under one combination can pass a gate that builds a superset — the
superset build is not evidence about the subset.

Reversing the split is impractical while the function-class target is supported; dropping
that target is what would reopen the question.

## Revisit triggers

- The heavy dependencies shrink, or the function-class budget rises, until one artifact
  fits both roles.
- A feature-unification bug reaches a release, showing the superset gate is not adequate
  coverage for the subset profiles.
- A fourth role appears that fits none of the three bundles, indicating the partition is by
  history rather than by dependency.
