---
contract: topology
---

# System topology

## The problem topology solves

An operator chooses a **Contextful** binary and a place to run it. Contextful runs
connectors, stores their output as Parquet files and serves queries under access
controls. The engine carries no business rules; applications supply domain meaning
({{topology.bound-application.domain-neutral}}).

## Three components, three shared contracts

One Cargo workspace builds execution, storage and query code ({{topology.compose.workspace}}).
The components share exactly three contracts: the connector interface, the part and
manifest layout, and the capability-token format ({{topology.compose.three-crossings}}).

Because every binary shares the layout, a query-only binary reads what a full binary wrote
({{topology.compose.run-path-droppable}}). A function returning stored rows requires an
access-control admission value, so a call omitting it fails to compile
({{topology.compose.mediation}}).

## Build profiles

Each build profile is a Cargo feature bundle chosen at build time
({{topology.package.profile}}). `contextful-full` runs connectors, schedules work and
serves queries ({{topology.package.full-profile}}). `contextful-edge` syncs stored files
and serves read-only SQL ({{topology.package.edge-profile}}).

`contextful-control` edits team configuration and applies canonical TOML
({{topology.package.control-profile}}). No run-time detection widens a running binary
into another profile ({{topology.package.fixed-at-build}}). A subcommand needing an
unlinked capability fails, naming the subcommand and profile
({{topology.package.capability-absent}}).

## Coordinating writers

Coordination rests on one property: a linearizable conditional write
({{topology.coordinate.primitive}}). Each new lease holder gets a higher fence, so a
stale writer's fence sits below the holder's ({{topology.coordinate.fence-advances}}). A
single node uses a local catalog file, a cluster uses Postgres, and Cloudflare uses D1
({{topology.coordinate.backends}}).

## One deployment, end to end

A Linux writer syncs corpus files to R2. A Cloudflare Linux Container reads a
synchronized copy behind the read router. The [deployment maps](../targets.md) show
every provider's shapes.

```mermaid
flowchart LR
  CALLER(["MCP or HTTP client"]):::muted
  subgraph SERVER["Linux writer processes"]
    TICK["pipeline serve scheduler"]:::accent
    subgraph CHILD["contextful pipeline run child"]
      STEP["Wasmtime component host"]:::accent
    end
    STORE[("Parquet and manifest files")]:::store
  end
  subgraph PROV["Cloudflare"]
    HOP["read router"]:::accent
    BUCKET[("R2 corpus bucket")]:::store
    subgraph RET["Linux Container running contextful-edge"]
      FACE["Rust HTTP handlers"]:::accent
      SQL["DuckDB library"]:::accent
      COPY[("local snapshot files")]:::store
    end
  end
  TICK -- "spawn child" --> STEP
  STEP -- "parts + manifest" --> STORE
  STORE -- "push" --> BUCKET
  BUCKET -- "S3 API hydration" --> COPY
  CALLER -- "query + credential" --> HOP
  HOP -- "HTTP + same credential" --> FACE
  FACE -- "admit query" --> SQL
  SQL -- "scan snapshot" --> COPY
```

One reconciler at a time holds the deployment's cadence lease
({{topology.coordinate.cadence-lease-ttl}}) and renews it while the applied document
schedules work ({{topology.coordinate.cadence-lease-renewal}}). A run child invokes the
connector, and the writer advances its cursor by one conditional update
({{topology.coordinate.cursor-cas}}).

Bucket sync copies the corpus to R2, and the edge binary pulls it and serves queries.
The read router forwards a caller's request to the query container, which reaches
readiness before it finishes loading stored data ({{topology.publish-hostname.container-readiness}}).

The deployment probe checks each hostname against its declared access policy. A public
response on a private hostname fails the probe ({{topology.publish-hostname.posture-mismatch}}).
A parity check compares each target's parts with the reference deployment's parts
({{topology.deploy.parity}}).
