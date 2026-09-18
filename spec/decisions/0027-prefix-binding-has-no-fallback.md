# 0027 — A bucket prefix binds explicitly and never falls back to the bucket root

**Status:** accepted 2026-09-18
**Decides:** `sync.push.refusal.escaping-key`, `sync.push.refusal.unbound-prefix`, `sync.push.refusal.two-prefix-sources`

## Context

A deployment's prefix is what separates it from every other deployment sharing a bucket.
Beneath `<prefix>/` sit that deployment's objects, its bookkeeping, and — the part that
makes the prefix structural rather than cosmetic — its reserved `manifest.json`, the one
object a consumer reads to learn what the deployment holds. The index is a pure function
of the tree beneath it, so an index written at the wrong root describes the wrong tree.

Two deployments sharing a bucket with no distinct prefixes collide on that reserved root
index. Each push writes an index of its own files alone. The last writer's index hides the
other deployment's tables from every consumer, and the hidden data objects survive under
their own keys, unlisted and unreachable. Nothing errors at any point. A consumer reading
the root index sees a smaller store and has no way to tell it apart from a store that is
genuinely smaller.

The prefix is bound from the environment through `prefix_from`, so one image serves many
deployments from one build. That is the same indirection the key binding uses and it has
the same failure state: the block is present, the variable is not. Here the state is
worse, because the natural fallback — start at the bucket root — is exactly the collision
shape above.

Key confinement is a second question with the same subject. Every key a deployment reads
or writes has to resolve beneath its prefix, and a key that escapes lands in another
deployment's territory or in the bucket's unowned space. Confinement could be a
responsibility of each caller or a property of the wrapper every caller goes through.

## Decision

The prefix binds from one source. An unset variable named by `prefix_from` raises
`SyncPrefixUnbound` at startup rather than starting the process at the bucket root, and
declaring `prefix` and `prefix_from` together raises `SyncPrefixOverspecified`. Prefix
confinement is applied once, inside the object-store wrapper, so every backend inherits it
and a key has one place to fail to be scoped; a key resolving outside the configured
prefix raises `SyncPrefixEscape`, naming the key and the prefix it left.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Bind explicitly, refuse at startup, confine in the wrapper** *(chosen)* | The collision shape is unreachable. One place decides what a key may be, and a violation names both the key and the prefix. | A deployment that forgets the variable does not start, and the deploy environment becomes part of the validation surface. |
| Fall back to the bucket root on an unset variable | Every deployment starts, and a single-deployment bucket needs no configuration at all. | Lost on failure shape. The fallback arrives at the moment a deployment is least watched, and produces mutually hidden stores that nothing reports. Single-deployment buckets are already served by leaving `prefix` unset deliberately. |
| Let each caller prepend the prefix to its own keys | No wrapper indirection, and a caller that legitimately needs the bucket root can reach it. | Lost on the one-implementation criterion. Confinement then has as many failure points as there are call sites, and a new backend is a new set of them. |
| Accept both a literal prefix and a binding under a precedence rule | An operator pins a prefix locally without touching the environment. | Lost on ambiguity. Two sources for one value turns every incident into a question about which won, and the answer is not visible in the file a reader opens. |

## Criteria

1. **Failure shape** — whether being wrong raises an error or produces a silently wrong
   store. *This criterion decided it.* An unbound prefix that falls back is unrecoverable
   by reading: the configuration says what it always said, the push reports success, the
   consumer reports a store, and only listing the raw bucket reveals the objects nobody
   can reach. Startup cost and operator convenience are both recoverable within a deploy
   cycle; a hidden tier of objects is discovered by accident or not at all.
2. **Number of places confinement can fail** — call sites and backends that have to scope
   a key correctly.
3. **Number of sources for one value** — how many places a reader consults to learn what
   prefix the process is using.
4. **Diagnosability of a violation** — whether the error names enough to act on. A refusal
   carrying the key and the prefix it left is actionable; a refusal carrying neither is a
   second investigation.

## Consequences

Startup validation reports the unbound prefix alongside every other unbound binding, so a
deployment learns its whole environment is incomplete in one pass rather than one variable
per restart. Key confinement has one implementation, which means a new object-store
backend inherits it rather than re-implementing it, and a wrapper change is the only way
to weaken it.

The accepted cost is that the deploy environment is now part of what has to be right for
the process to come up. An orchestrator that drops the variable produces a process that
will not start. The bucket root stays reachable only by leaving `prefix` undeclared
entirely, which is a decision written in the configuration rather than an accident of a
missing variable.

Reversing the no-fallback direction is cheap in code and costly in trust: once operators
rely on the refusal, a fallback added later means a deployment can quietly change which
tree it publishes between two builds.

## Revisit triggers

- A deployment shape appears that legitimately needs to read outside its own prefix — a
  cross-deployment catalog, a shared reference tier — which makes the wrapper's confinement
  the wrong boundary rather than a safe one.
- The bucket index acquires a per-deployment identity a consumer can check, so a root
  collision is detectable by reading the index rather than by listing raw keys.
- Startup refusals on unbound bindings are observed to be the dominant cause of failed
  deploys, which would argue for resolving every binding in one reported pass ahead of
  process start rather than during it.
