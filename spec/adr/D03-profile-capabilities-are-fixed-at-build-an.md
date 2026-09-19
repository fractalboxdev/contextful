# D03 — Profile capabilities are fixed at build; an absent one is a typed refusal

**Status:** accepted

## Context

An edge replica, a full engine and a control plane need different dependencies. A binary that detects features at run time links all of them, and one that degrades when a capability is absent answers a different question from the one asked.

## Decision

Three profiles, `contextful-edge`, `contextful-full` and `contextful-control`, compile from one workspace by feature bundle. Each links only the dependencies its role names, and no run-time detection widens a built binary.

- The CRDT library links into `contextful-control` alone; other profiles read the TOML it materializes on apply.
- A capability the running profile does not wire is a typed refusal when reached, never a panic, a no-op or a native fallback of similar name. A profile without the embedded SQL engine refuses both read tools rather than answering from the lexical arm or with an empty result.
- `topology.package` publishes each profile's wiring as an explicit list, and a driver answers a describe call naming what it serves.
- A client declares its required faces, matched at the handshake; an engine reporting no set satisfies no requirement.
- A spawned engine finding no project manifest exits before writing any protocol framing.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Build-time profiles; typed refusal on reach *(chosen)* | — | Three artifacts to build, size-gate and release; a caller learns about an absent capability at the handshake or first call. |
| One binary with run-time feature detection | Footprint | Every heavy dependency links into the replica. |
| Dynamically loaded plugins | Portability | Static musl targets, the edge deployment shape, load no plugins. |
| Degrade to a partial answer when a capability is absent | Honesty of the outcome | A lexical-only ranking or an empty result reads as a complete answer. |
| Separate repositories per artifact | Versioning | The shared domain and its crossings version across repositories. |

## Consequences

- A binary's capabilities are a property of its build, readable before it runs.
- A client that needs a face fails at the handshake, not mid-session.
- The dependency audit refuses a control-only library in the resolved graph of another profile.
