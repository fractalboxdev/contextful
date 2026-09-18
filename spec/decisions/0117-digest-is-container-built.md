# 0117 — A committed connector digest comes from a digest-pinned container on one fixed platform

**Status:** accepted 2026-09-18
**Decides:** `connector.package.refusal.host-built-pin`

## Context

A committed digest is a claim that these bytes are what this source builds to. The claim is
only useful if a second party building the same source reproduces the same value. With the
toolchain held constant — one compiler commit, one target, one lockfile, one set of paths —
that reproduction fails across machine architectures. Build-script and proc-macro units are
host-kind: they compile for the machine doing the building, not the machine the artifact
targets, and the host triple enters their metadata hash. That hash reaches symbol names,
symbol names reach linker layout, and layout reaches the bytes.

The instruction stream is identical. Each host reproduces itself exactly, and the two hosts
disagree with each other. Stripping symbols restores no layout. Forcing one codegen unit
does not converge them. No source edit and no compiler flag closes the gap, because the
input that differs is the identity of the machine, and that input is not expressible in the
build.

Path remapping over the source root and the package cache is applied in every build, so
where a checkout sits on disk is not an input. That removes one source of divergence and
leaves the architectural one standing.

The consequence of getting this wrong is specific and bad. An author on one architecture
builds, pins, and commits. Everything on their machine agrees: the digest matches the
artifact, the connector loads, the tests pass. A colleague on the other architecture builds
the same source, the artifact differs from the committed digest, and the engine refuses its
own guest at boot. The failure lands in a restart loop on somebody else's machine rather
than in the gate.

## Decision

`contextful connector pin <manifest>` builds the guest inside a digest-pinned container on
one fixed platform, writes the artifact next to the manifest, and writes the digest into the
manifest. `--verify` rebuilds and compares without writing, failing on drift. `--local`
builds with the host toolchain for a build-run-edit loop and states plainly that the digest
it prints is host-local; writing a pin from a host build raises `ConnectorHostPinRefused`, a
host build being one that reads its toolchain off the path.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Pin only from a digest-pinned container on one fixed platform** *(chosen)* | The committed value follows the source for every builder, and the container itself is pinned so the builder does not drift either. | Writing a pin needs a container runtime, so verification splits into a text-only check and one that runs where the builder can. |
| Document the host dependence and leave the digest to whoever builds | No container dependency; the fastest possible authoring loop. | Lost on where the failure lands: the natural workflow yields a correct pin, a correct artifact and an engine refusing its own guest at boot on another architecture, which surfaces as a restart loop rather than in the gate. |
| Strip symbols, or force one codegen unit | No new tooling; a build flag would close it. | Lost on effect: neither converges the two hosts, because the differing input is the host triple inside a metadata hash, which no flag removes. |
| Record a digest per architecture | Every builder verifies on their own machine. | Lost on what a pin means: the manifest would carry a set of acceptable byte strings, and the reviewer could no longer name the one program the controls bind. |
| Accept a host-built pin with a warning | No refusal to work around; the loop stays local. | Lost on default direction: the warning is printed at the moment the author is succeeding, and the committed pin is wrong for half the population. |

## Criteria

1. **Whether the committed value follows the source.** *This criterion decided it.* A
   digest whose value depends on who ran the build is not a statement about the source, and
   every control resting on the pin rests on that statement. The other criteria trade
   convenience; this one decides whether the artifact means anything.
2. **Where a divergence surfaces.** In the gate, or in a restart loop on a machine
   downstream.
3. **Authoring-loop cost.** Whether an author can iterate without a container.
4. **Reviewability of the pin.** Whether a reader can name one program from the manifest.

## Consequences

Two people on different architectures commit the same digest for the same source, and
`--verify` is a real check rather than a machine-local tautology. `--local` keeps the fast
loop available and labels its output so the number cannot be mistaken for a committable one.

The accepted cost: writing a pin needs a container runtime. Verification therefore splits
in two — a text-only check that runs anywhere, and a rebuild-and-compare that runs only
where a container can. A contributor without one can author and iterate but cannot produce
the committed value, which makes pinning a step somebody else performs. `--verify --native`
exists for that gap: it checks the host triple, the compiler and the package-manager build
against pinned constants, clears every variable, refuses every configuration key that would
move the bytes, and answers under a distinct exit status so an unable-to-check runner never
reads as a wrong committed digest.

The fixed platform is now a dependency of the project. If its container image becomes
unavailable or its toolchain unbuildable, every pin in the tree becomes unverifiable at
once.

## Revisit triggers

- The build toolchain stops carrying the host triple into host-kind unit metadata, which
  would make the two architectures converge and remove the reason for the container.
- The fixed builder platform's image or toolchain becomes unavailable, which makes every
  committed pin unverifiable and forces a re-pin across the tree.
- `--verify --native` is observed to disagree with the container's answer on inputs it
  claims to have controlled, which would mean its pinned-constant check is incomplete.
