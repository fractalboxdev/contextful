# D15 — Executables are content-pinned; replay uses the recorded artifact

**Status:** accepted

## Context

Recorded output replays only against the program that produced it. Yet a remote tag is repointable, a host build embeds its triple, and a script is rewritten in place.

## Decision

The program a reviewer approved is identified by a content digest, and a live run completes against the digest its record names.

- `connector.package` refuses a remote artifact without a 64-hex pin at parse, refuses a plain-HTTP reference, and re-hashes resolved bytes before load. A local artifact is pinned under either the store policy key or the per-connector flag; the refusal quotes the computed digest.
- A committed pin comes from a digest-pinned container on one fixed platform; writing a pin from a host build is refused.
- Re-resolving a connector while a journaled run is live is refused; a new version applies to runs admitted after the change.
- `run.own` pins connector identity, component world and plan hash while an execution owner is pending; a terminal status releases the pin in the same transaction that publishes the position.
- `run.exec` requires a digest on a path-form command; a bare name on the search path carries none.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Pin by digest, hold the pin while work is pending, apply changes to new runs only *(chosen)* | — | Pinning needs a container runtime; an urgent fix waits for in-flight runs; a bare-name binary changes with no configuration change. |
| Trust version tags | Review binding | The reviewed and running programs would match only by the publisher's continued behavior. |
| Reload in place, or adopt the new build on resume | Replay fidelity | Recorded output would replay into code that never produced it, silently. |
| Pin on the previous run's health | Rebuildability | A finished fire with nothing to replay would refuse every rebuild. |
| Digest every command, including bare names | Runnability | Each package upgrade would refuse every derive pipeline until re-pinned. |

## Consequences

- Unpinned local loads run whatever bytes sit on disk unless a switch is set.
- An operator whose pending work pins a build restores that build or rewinds explicitly.

## Revisit

- Production found running unpinned local artifacts, arguing for inverting the default.
- The toolchain stops embedding the host triple, removing the need for the container.
