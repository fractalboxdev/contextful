# 0325 — Automation carrying control flow is a typed subcommand over compiled binaries, and surviving shell passes a linter

**Status:** accepted 2026-09-18
**Decides:** `build.automate.refusal.unchecked-shell`

## Context

The tree's build-time work is not a single script. It is a gate of nine ordered stages, a
footprint step that cross-builds and compresses per profile, a deploy path, parity tooling
and a spread of development helpers. Every one of those carries judgment: a branch on
whether an artifact resolved, a retry on a transient fetch, a comparison of a regenerated
file against its committed copy, an exit code chosen from which of four ceilings was hit.

The same step runs in two places. A contributor reproduces a stage locally to diagnose a
red run, and the gate container runs it to decide red or green. Where those are two
implementations, or one implementation whose behavior depends on which platform's
utilities are installed, a local reproduction stops being evidence about the gate.

Shell is the default answer and it is the worst of the three on exactly the properties this
work needs. Word splitting and quoting turn an unset variable into a different command
rather than an error. Error handling is a trap, not a type. The utilities a step calls
differ between platform variants in flags and in output format, so a step that works on a
contributor's machine is not the step the container runs. None of it is callable from a
test without a shell around it, so branches that fire once a quarter are never exercised.

The tree already carries one toolchain for its own compiled code, and the compiled binaries
the automation drives are where the heavy work belongs. What is at issue is the layer above
them — the part that parses arguments, decides what to invoke, and interprets what came
back.

## Decision

Automation carrying judgment is a typed subcommand that shells out to compiled binaries.
The subcommand parses its arguments into types, branches, retries and handles errors, and
exposes an entry point a test calls directly. Straight-line glue of a few commands stays
shell, and a file that `shellcheck` rejects raises `ShellCheckFailed` in the toolchain
stage. The gate invokes the same subcommand a contributor invokes. The automation toolchain
is build-time only and links into no build profile.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Typed subcommands over compiled binaries, with linted shell at the boundary** *(chosen)* | Typed arguments, exhaustive error handling, branches exercised by tests without a shell, one code path for a local run and a gate run. | A second toolchain in the build path that a contributor installs to change one line of gate logic. |
| Shell throughout | No new toolchain; every contributor already reads it; trivially editable in place. | Lost on portability and testability together: utility variants diverge between platforms, quoting and trap behavior turn mistakes into wrong commands rather than failures, and no branch is callable from a test. |
| A second scripting runtime for the orchestration | Fast iteration, rich standard library, familiar to most contributors. | Lost on toolchain count: another interpreter and environment to install, pin and keep reproducible across the container and every contributor machine, where the tree already carries one toolchain. |
| Compiled binaries for the orchestration as well | One toolchain, strongest typing, no interpretation layer. | Lost on iteration speed: glue changes often, and a full compile per edit to a gate argument is a loop nobody uses, so the glue drifts back into shell. |
| Linted shell alone, with no typed layer | No new toolchain, and the worst quoting and unset-variable defects are caught. | Lost on testability: a linter checks the text, not the branches. A retry that never fires is still never exercised. |

## Criteria

1. **Testability** — whether a branch, a retry and an error path can be exercised without
   a shell process around them.
2. **Portability** — whether one implementation behaves identically on a contributor
   machine and inside the gate container, across platform utility variants.
3. **Toolchain count** — how many interpreters or environments must be installed and
   pinned for the build path to be reproducible.
4. **Iteration speed** — the edit-to-run loop for glue that changes often.

**Portability and testability decide it together.** They are the two properties that make a
local reproduction count as evidence about a gate verdict, which is the whole reason the
gate is runnable locally. Toolchain count rules out the second runtime; iteration speed
rules out compiling the glue itself. Neither of those two is decisive on its own — each
only picks between options that already satisfy the first pair.

## Consequences

A gate step is code with a type signature, so an argument that does not parse fails at the
boundary rather than four commands later. A branch that fires rarely is reachable from a
test. A red gate reproduces locally by invoking the identical entry point, so the
difference between the two runs is the environment rather than the logic.

The cost accepted: a second toolchain sits in the build path. A contributor fixing one line
of gate logic installs it, which is a real barrier for a drive-by change to something that
used to be a text file. It is build-time only and links into no build profile, so the cost
is bounded to the contributor's machine and the container image and never reaches a shipped
artifact.

Reversing this is cheap per step and expensive in aggregate: any single subcommand can be
rewritten back into shell, but the shared argument types, the shared error handling and the
direct-call test surface are what make the collection coherent, and unwinding those returns
the whole layer to text.

## Revisit triggers

- The automation toolchain's install becomes a recurring blocker on contributions that
  touch only gate logic, measured by changes abandoned or routed around it.
- A shell dialect and its utilities become pinnable to one version across the container and
  every supported contributor platform, removing the portability difference.
- The tree's compiled toolchain gains an interpreted or scripted mode whose edit-to-run
  loop matches an interpreter's, collapsing the iteration-speed objection to compiling the
  glue.
