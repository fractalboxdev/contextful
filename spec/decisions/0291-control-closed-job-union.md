# 0291 — Maintenance jobs are a closed union of in-process engine operations

**Status:** accepted 2026-09-18
**Decides:** `control.fire.refusal.job-carries-no-argv`

## Context

A `[[job]]` block gives an operator a cadence for maintenance work — folding tables,
rebuilding a catalog, sweeping permissions, publishing models, pushing a replica. The
scheduler fires these on the same clock and through the same due-ness test as pipeline
entries, in produce-before-publish order within a tick.

What a job is permitted to do sets the trust level of every path that can write a job
block. Jobs live in a daemon's own configuration and enter no control-plane document, so
today the writer is an operator with filesystem access to the machine. That is not a stable
guarantee. The operator portal writes configuration. A control source can be repointed. A
deployment's configuration can be templated, mounted, or generated. Any of those becoming
the writer of a job block turns "what a job can do" into "what that writer can do as the
daemon process".

A job carrying an argument vector or a host command answers that question with: anything.
The daemon holds the store's credentials, its signing inputs, its object-store bindings and
its network egress. An argument vector in a scheduled block is arbitrary execution with all
of it, on a cadence, with the output going wherever the command sends it. The failure is
silent by construction — a job that succeeds is a log line.

A closed union answers it with a set someone can read in one sitting. The set is also what
makes an exhaustive match possible, so adding a kind is a compile-time obligation across
every site that dispatches one rather than a runtime lookup that can miss.

## Decision

The scheduler fires a closed union of kinds — `acl-sweep`, `build`, `compact`,
`rebuild-catalog`, `sync-push`, `validate` — reached by an exhaustive match, beside the
implicit pipeline-run kind every scheduled entry registers. Each kind is an in-process
engine operation reached through the same path as its command verb, so a job runs code the
engine already exposes rather than code a configuration file names. A block naming a kind
outside the union, an argument vector, or a host command raises `JobKindUnknown` at
validation, before the block joins any armed set. Maintenance work outside the union
arrives as a new kind in a release.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A closed union of in-process engine operations** *(chosen)* | Whoever can write a job block gains exactly the six operations, and adding a kind is an exhaustive-match obligation the compiler enforces at every dispatch site. | Maintenance work outside the six kinds needs an external scheduler or a new kind and a release, so an operator with a one-off need has no in-product answer. |
| A job carrying an argument vector | Every maintenance need is expressible today, with no release; the operator is never blocked. | Lost on the trust boundary: it grants anyone who can write the manifest, or repoint a snapshot that reaches it, arbitrary execution as the daemon with the store's credentials and egress. |
| A plugin registry of job kinds | Extensibility without a core release; third-party maintenance work becomes possible. | Lost on the same criterion: the same execution surface behind a longer path, now with a loading and trust story to build that does not exist. |
| An allowlist of host commands | Narrower than a free argument vector; expressible in configuration. | Lost on reviewability: the allowlist is per-deployment data, so no reader of the product can state what a job can do, and argument handling reintroduces the execution surface through the arguments. |

## Criteria

1. **Attack surface at the daemon's trust boundary** — what a party who can write a job
   block obtains. **This criterion decided.** The daemon holds the store's credentials and
   its egress, so the gap between six named operations and arbitrary execution is the gap
   between a bounded maintenance surface and full compromise of the deployment; no amount
   of convenience on the other side is priced comparably.
2. **Reviewability of the set** — whether a reader can state, from the product rather than
   from a deployment, what scheduled maintenance is able to do.
3. **Compile-time completeness** — whether adding a kind forces every dispatch site to
   handle it. An exhaustive match does; a registry lookup does not.
4. **Operator reach without a release** — how much maintenance an operator can schedule
   with the binary they have. The chosen option is the worst here.

## Consequences

The set of things a scheduled job can do is a property of the product rather than of a
deployment, which means the security review of the job path is done once. Each kind reaches
the same code as its command verb, so a job and a manual invocation cannot drift in
behavior, and a kind is testable through its verb. Adding a kind is a deliberate act with a
compile error at every site that has to learn it.

The cost accepted: an operator with a maintenance need outside the six kinds has no path
through this product. They run an external scheduler against the command surface, which
means that work sits outside the tick's produce-before-publish ordering, outside the
store-global exclusion key, and outside the fire watermark — so it can overlap an entry
writing the tree in a way an in-union job cannot. Every new kind costs a release, and the
pressure to widen the union will arrive as a stream of single-deployment requests.

Reversing toward an argument vector is a one-line change in the validator and is
irreversible in effect: every deployment that adopted it has to be audited before the union
can be closed again.

## Revisit triggers

- The union is extended more than a small number of times in close succession, indicating
  the maintenance surface is genuinely open-ended rather than closed.
- A deployment is found running an external scheduler for maintenance that overlaps store
  writes, demonstrating the exclusion-key gap in practice.
- A sandboxed execution kind exists with its own capability grants, making a general job
  expressible without granting the daemon's own authority.
