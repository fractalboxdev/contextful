# 0157 — The child environment is built from nothing plus an explicit allowlist

**Status:** accepted 2026-09-18
**Decides:** `derive.exec.refusal.env-name`, `derive.exec.refusal.unresolved-reference`

## Context

An `exec` engine runs third-party binaries the operator named: a media fetcher, a transcoder,
a speech tool. These are ordinary programs off a package manager or a vendor download, and the
engine has no view into what any of them reads at startup. A subprocess inherits its parent's
environment by default on every platform the engine targets, and the parent here is a long-lived
daemon that has already resolved credentials for the connectors, the store and whatever else the
machine binds. Inheritance therefore hands a speech tool the same variables that authorize the
machine's own network identity, for the length of every unit it processes.

The second half of the question is timing. An operator writes the environment allowlist by hand
in the machine's own configuration, including credential-reference templates that name entries in
a provider chain. A reference is a string until something dereferences it. A pipeline runs on a
schedule with nobody watching, and a derive run reaches an engine once per unit, so a mistyped
reference name that is only noticed at use time is noticed in the middle of a batch, after the
scan, the anti-join and the lease have already been paid for.

Variable names are also not free-form. A cleared environment is assembled key by key, and the
platform APIs that set one accept bytes that no shell will ever read back — a name carrying `=`,
a leading digit, or a non-ASCII byte lands in the child's block and is silently unreachable from
the tool that was meant to read it.

## Decision

The child receives a cleared environment plus the operator's explicit allowlist and nothing
further. An allowlist entry whose name is not ASCII alphanumeric or underscore, or that leads
with a digit, raises `DeriveEnvNameInvalid` naming the entry. A credential-reference template is
parsed at engine build; one naming no resolvable entry raises `DeriveSecretUnresolved` at that
moment. Resolved values hydrate per unit and drop with the call rather than being held on the
engine between units.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Cleared environment plus a named allowlist, references resolved at build** *(chosen)* | No ambient process state reaches a spawned binary; a typo is a build-time refusal naming the entry | The operator enumerates every variable the tool reads, including undocumented ones |
| Inherit the parent environment, subtract a denylist | Tools work out of the box; no enumeration | Lost on ambient reach: every credential the daemon holds reaches every tool it spawns, and a denylist cannot name a variable a future dependency introduces |
| Inherit, but clear only variables matching credential-shaped names | Cheaper than enumeration, catches the obvious cases | Lost on ambient reach: shape matching is a guess, and a vendor naming its token `SETTINGS_B` passes it |
| Resolve credential references lazily at first use | One fewer build step; unused references cost nothing | Lost on discovery latency: a typo surfaces at the first scheduled tick, mid-batch, rather than while the operator is watching |

## Criteria

1. **Ambient reach** — whether process state the operator did not name can arrive at a
   third-party binary. Measured by asking what a tool reading the whole environment block would
   find.
2. **Discovery latency** — how long between writing a bad reference and being told. Measured in
   operator attention: while editing, or at an unattended tick.
3. **Configuration burden** — how much an operator writes to make a working tool work.
4. **Name legibility** — whether a key that reaches the child block is one the child can read.

Criterion 1 decided it. Inheritance is the only option that is wrong by default rather than
wrong when misconfigured, and the blast radius is every credential in the process against every
binary an operator ever names. Criterion 3 is the cost paid to satisfy it and is bounded: an
operator who omits a variable gets a tool that fails loudly on its own terms, which criterion 1's
failure mode does not.

## Consequences

Easier: reasoning about what a spawned tool can see reduces to reading one configuration block.
A credential reaches exactly the engine whose binding names it, for the length of one unit, and a
run audit can state that without qualification.

Harder: standing up a new engine is a discovery exercise. A tool that reads a proxy setting, a
certificate bundle path, a locale or a temporary-directory override from the environment breaks in
ways whose error message does not mention the environment at all, and the operator learns the
list by hitting each one.

Accepted cost: an operator enumerates every variable a tool reads, including the ones its
documentation does not mention. Build-time reference resolution also means an engine whose
credential provider is briefly unreachable refuses to build rather than degrading, which turns a
provider outage into a refusal at a moment the operator is present for.

Expensive to reverse: rows already produced carry an engine identifier hashed over the resolved
chain. Widening inheritance later does not invalidate them, but the guarantee a run audit states
about credential reach is retroactively weaker for every reader who trusted it.

## Revisit triggers

- An operator's allowlist exceeds a size where enumeration stops being reviewable in one screen,
  observed as bindings carrying more than a few dozen entries.
- A credential provider's availability becomes the dominant cause of build refusals, observed as
  `DeriveSecretUnresolved` raised against names that resolve on retry.
- A platform's process API stops rejecting the names this refusal catches, making the name check
  redundant rather than protective.
