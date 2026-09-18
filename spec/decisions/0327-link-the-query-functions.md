# 0327 — The query engine's columnar-read and statement-serialization functions link into every engine-linked build, and the read path loads no extension while serving

**Status:** accepted 2026-09-18
**Decides:** `build.build.refusal.runtime-extension-load`

## Context

The read path needs two capabilities from the SQL engine that ship as extensions rather
than as core: reading columnar files, and serializing a parsed statement back out. Neither
is optional. Every snapshot read goes through the first, and statement guarding goes
through the second. A read that cannot reach them cannot answer.

The engine is statically linked into the binary. Its default posture for an extension is to
load one on first use, which means a network fetch into a shared extension directory, then
a dynamic load into the running process.

That default is not merely slow here; it is unsound against a static link. The extension
binary carries its own copy of the engine's type definitions, and the host binary carries
one too. Loading the extension places two copies of a type's runtime type information in
one process, the extension's at hidden visibility. A cast between them succeeds against
neither copy, and the engine's own check catches the mismatch and aborts the process. That
check compiles out on Apple platforms, where the identical undefined behavior stays silent
and surfaces later as corrupted reads rather than as a crash — so the platform most
contributors develop on is the platform least able to detect this.

The operational objections stack on top of the correctness one. First use is inside a
query, so a read path that autoloads needs network egress at serve time, which the profiles
exist to avoid. Concurrent processes racing to populate one shared extension directory make
a suite intermittently red for reasons that have nothing to do with the change under test.

## Decision

The functions every read needs — columnar file reading and statement serialization — link
into each build that links the SQL engine. The read path loads no extension while serving a
query, and a read path reaching for one raises `ExtensionAutoloadRefused`.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Linking both function sets into every engine-linked build** *(chosen)* | One copy of every type in the process, so no cast can straddle two runtime type information tables. No egress at serve time, no shared directory to race. | 55 MiB on every engine-linked binary, charged against each profile's footprint budget, and one crate's dependency declaration decides the feature list every profile receives. |
| Autoloading on first use | Zero bytes until a capability is used; the engine's own default, so nothing to configure. | Lost on correctness: two runtime type information copies in one statically linked process abort on the first cast, and the detecting check compiles out on Apple platforms. It separately requires serve-time egress and races concurrent processes on one shared extension directory. |
| Loading extensions in a separate process, reached over an interprocess call | Keeps the two type tables in separate address spaces, so the correctness failure cannot occur. | Lost on read-path latency: every columnar read and every statement guard crosses a process boundary. Lost again on operational complexity — a second process to supervise, for one function each. |
| Dynamically linking the engine so extension and host share one copy | Restores the extension model as designed, and the 55 MiB is paid once on disk. | Lost on footprint posture: the profiles hold their dynamic dependency set to the platform C library, so this trades a refusal in the build for a refusal in the footprint gate. |
| Reimplementing both capabilities outside the engine | No extension mechanism at all, and full control of the footprint. | Lost on correctness for a different reason: a second columnar reader and a second statement serializer would have to agree with the engine's own parsing and type handling exactly, and where they disagree the guard passes a statement the engine reads differently. |

## Criteria

1. **Correctness** — whether the process can reach a state where a cast between two copies
   of one type is attempted.
2. **Network egress on the read path** — whether answering a query can require reaching
   the network.
3. **Suite determinism** — whether concurrent runs contend on shared mutable state outside
   the tree.
4. **Binary footprint** — bytes added to each profile's budget.

**Correctness decides it.** The other three are costs that can be traded; this one is the
difference between a process that answers and a process that aborts, and its detection is
platform-dependent in the direction that hides it during development. Footprint is the
criterion the chosen option loses on, and it is accepted rather than avoided.

## Consequences

Every engine-linked binary carries both function sets and needs no extension directory, no
egress and no first-use warmup. The read path's behavior is identical on the platform that
aborts on the mismatch and the platform that does not, because neither reaches the
mismatch.

The cost accepted: 55 MiB on every engine-linked binary, charged against each profile's
compressed and resident budgets — including the read replica, which is the profile with the
least room. A second consequence is structural: the single crate that declares the engine
dependency decides the feature list every profile receives, so a profile cannot opt out of
a function set another profile needs.

Reversing this is expensive in an unusual way. The refusal is not a policy that can be
relaxed; removing it means accepting a process abort on one platform and silent corruption
on another, so a reversal requires the static-link posture to change first.

## Revisit triggers

- The engine gains a loading mode that resolves an extension against the host binary's own
  type tables rather than shipping a second copy.
- A profile's footprint budget cannot absorb the 55 MiB alongside its other growth, forcing
  a split between profiles that carry the functions and profiles that do not serve reads.
- The detecting check stops compiling out on Apple platforms, which would remove the
  asymmetry between where the failure is loud and where it is silent.
