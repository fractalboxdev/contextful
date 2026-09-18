# 0163 — A link binding rejects the keys that belong to a driver spawning processes

**Status:** accepted 2026-09-18
**Decides:** `derive.fetch.refusal.exec-keys-on-a-fetch-binding`

## Context

Both drivers are configured in the same place and in the same grammar: a `[derive.<name>]` block
at the top level of the machine's own configuration, with `driver` selecting between `exec` and
`fetch`. The two key sets barely overlap. An `exec` binding carries `command`, `preprocess`,
`env` and `max_output_bytes`; a `fetch` binding carries `allow_hosts`, `allow_image_hosts` and
the byte and timeout bounds the scanner and the probe use. An operator converting one engine into
the other, or copying a working block as a starting point, arrives at a block whose `driver` line
says one thing and whose remaining keys were written for the other.

The `env` key is the one that matters. On an `exec` binding it is the allowlist 0157 governs: a
credential-reference template resolved through the provider chain and hydrated into a child
process for the length of one unit. A fetch engine spawns nothing. There is no child, no
environment block, and no request header derived from one — the engine opens a socket to a host
chosen by a row and sends no authorization at all. That absence is not incidental; it is the
reason a fetch binding is permitted to name more than one host in the first place. An engine
carrying a credential can only be pointed at parties the credential was issued for, and a list of
publishers is not that.

So an `env` key silently ignored on a fetch binding produces a specific and dangerous belief: the
operator has written a credential next to a host list, and reads the configuration as saying that
those hosts are authenticated to. The truth is that the credential travels nowhere, and the
operator's model of which parties hold what is wrong in the direction that makes them list more
hosts, not fewer.

## Decision

`env`, `preprocess`, `engine` and `max_output_bytes` on a fetch binding raise
`DeriveFetchBindingKey` naming the key. A fetch engine carries no credential to any host, which
is what lets it name more than one.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse process-spawning keys on a fetch binding, naming the key** *(chosen)* | A block's `driver` line and its key set cannot disagree; no operator believes a fetch carries a credential | Moving a binding between drivers edits the key set rather than the driver line alone |
| Ignore unknown keys, as most configuration formats do | Tolerant of copied blocks and of keys a future driver adds; no refusal to maintain | Loses on that criterion: the operator believes a credential travels to hosts it never reaches, and sizes the host list on that belief |
| Warn on an unrecognized key | Tolerant and still says something | Lost on whether a silently ignored key leaves a false belief about credential reach: a warning in a scheduled run reaches nobody, so the belief survives; identical outcome to ignoring, with more code |
| Accept `env` on a fetch binding and attach it as request headers | The operator's evident intent is honoured; authenticated fetches become possible | Loses on the reason a fetch may name many hosts at all: it carries no credential to any of them, so honouring the key means every listed publisher receives the material |
| Split the two drivers into separate configuration tables entirely | The key sets cannot be confused at all | Loses on effort and on churn: two grammars for one concept, and the shared bounds and `zone` key are duplicated — worth reconsidering if a third driver arrives |

## Criteria

1. **Whether a silently ignored key can leave an operator holding a false belief about credential
   reach.** Measured by what a reader of the configuration block would conclude.
2. **Tolerance of configuration drift** — how gracefully a binding survives copying and editing.
3. **Whether the refusal names the fix** — a rejected key is useless unless the operator knows
   which line to delete.

Criterion 1 decided it. Criterion 2 is the standard argument for permissive configuration and it
holds for keys whose absence is harmless; it does not hold here, because the belief the ignored
key creates is a belief about who holds a secret, and an operator acting on it widens a host list
that 0160 exists to keep narrow. Criterion 3 is why the refusal names the key rather than
reporting an invalid binding.

## Consequences

Easier: a fetch binding read on its own states the truth — a list of names, some bounds, and
nothing that authenticates. Reasoning about where a credential can reach reduces to reading `exec`
bindings only.

Harder: an operator converting an engine from one driver to the other gets a sequence of refusals
rather than one, and each edit reveals the next. A block copied from documentation for the wrong
driver fails at build rather than running in a degraded shape.

Accepted cost: an operator moving a binding between drivers edits the key set rather than the
driver line alone, and the refusals arrive one key at a time rather than as a single list of
everything wrong with the block.

Expensive to reverse: the four names are now reserved against fetch bindings. If a future fetch
capability genuinely needs a per-request value — a user agent policy, a per-host header — it
cannot reuse `env`, because that spelling now means "this binding is misconfigured".

## Revisit triggers

- A legitimate need for authenticated fetching appears, which would require a key that is
  explicitly per-host rather than a single environment block, and would re-open what a fetch
  binding's host list is allowed to contain.
- A third driver is introduced, at which point the shared-table grammar carries three key sets and
  the separate-tables option is re-weighed on effort.
- Operators are observed hitting these refusals in sequence often enough that reporting the whole
  invalid key set at once becomes the cheaper behavior.
