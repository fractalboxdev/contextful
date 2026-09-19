# D22 — A derive engine is a machine-bound argv child with a cleared environment

**Status:** accepted

## Context

A derive engine runs a local binary or reaches a vendor, and row data flows into its arguments. Pipeline manifests arrive from control planes and agents; the machine owner is the party that bears what runs on the machine.

## Decision

A manifest requests an engine by name; the machine's own configuration defines what that name executes, and row data never becomes syntax.

- `run.bind`: a manifest names an engine and introduces no command; a `[derive.<name>]` block in the machine's configuration states the argv chain or host list, environment allowlist, pins and bounds. An unbound name, or a command key in a manifest, refuses.
- Task and driver are checked as a pair at build.
- The shared port owes engine id, a bare endpoint host, locality and run audit; each modality adds an anchor trait. The adapter declares locality, and the operator's zone key is advisory.
- A remote media address reaches an engine only when the adapter declares the capability and the operator opts in.
- `run.exec` spawns an argument array with no shell. The child gets a cleared environment plus a named allowlist, credential references resolve at build, and each unit is bounded by wall clock and captured output; on the deadline the process group is killed.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Machine-local binding, argv child, cleared environment, group kill *(chosen)* | — | A pipeline is not self-contained; pipes become chain steps; the operator enumerates every variable a tool reads. |
| Command carried in the manifest | Who may introduce a command | Any manifest author, including an agent, would gain argv execution. |
| A shell string, or escaped row data | Data becoming syntax | A filename with a space or `$()` would be read as command text. |
| Inherit the environment minus a denylist | Ambient reach | Every daemon credential would reach every spawned tool. |
| Operator-declared locality as the row value | Recoverability of placement | A config edit would relabel a vendor engine as on-device. |

## Consequences

- A manifest half is inert on an unprepared machine.
- A long but legitimate chain dies at its deadline; verbose output is truncated.
- A link engine's locality tag matches no zone until an operator widens the list.

## Revisit

- Operators routinely wrap steps in pinned scripts to recover shell features.
