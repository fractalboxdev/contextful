# D53 — The columnar-read and statement-serialization functions link into every engine-linked build

**Status:** accepted

## Context

Every snapshot read needs columnar-file reading and every statement guard needs statement serialization; both ship as SQL-engine extensions. The engine is statically linked, and its default loads an extension on first use. A loaded extension brings a second copy of the engine's runtime type information: a cast between the copies aborts the process, and on Apple platforms the check compiles out and the fault surfaces as corrupted reads.

## Decision

`assurance.build` links both function sets into each build that links the SQL engine. The read path loads no extension while serving; reaching for one raises `ExtensionAutoloadRefused`.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Link both function sets into every engine-linked build *(chosen)* | — | 55 MiB on every engine-linked binary, the read replica included; one crate's dependency declaration fixes the feature list for every profile. |
| Autoload on first use | Correctness | Two type tables would abort one platform and corrupt another, with serve-time egress and a raced shared directory besides. |
| Extensions in a separate process | Read-path latency | Every columnar read and statement guard would cross a process boundary. |
| Dynamically link the engine | Footprint posture | Profiles hold their dynamic set to the platform C library. |
| Reimplement both capabilities | Correctness | A second reader and serializer would disagree with the engine's own parsing, and the guard would pass statements the engine reads differently. |

## Consequences

- No extension directory, no egress and no first-use warmup on any read.
- Read behavior is identical on the platform that aborts and the one that does not.
- Reversal requires the static-link posture to change first.

## Revisit

- The engine resolves extensions against the host binary's own type tables.
- A profile's footprint budget cannot absorb 55 MiB.
