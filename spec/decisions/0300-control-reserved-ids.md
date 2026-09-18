# 0300 — The two built-in store ids are reserved against configured entries

**Status:** accepted 2026-09-18
**Decides:** `control.register-store.refusal.reserved-id`

## Context

Exactly two stores ship inside the product, on the criterion that they need no
configuration: a bundled fixture with no upstream that renders the entire surface with no
secrets, and a loopback store marked development-only. Both variables unset serve these
alone, which is what makes a fresh deployment a working surface rather than a blank one.

Configured stores arrive from a runtime array, so the built-in set and the configured set
share one id namespace and are populated by different parties at different times. A
configured entry claiming a built-in's id is therefore not an exotic case: an operator picks
an obvious id for their store, and the obvious ids are the ones the built-ins already took.

What that collision costs depends on how the fixture is identified. The fixture is
discriminated by the absence of an upstream rather than by its id — the surface knows it is
the fixture because there is nothing to fetch from, not because it is called something in
particular. So a configured entry that shadows the fixture's id does not simply replace it:
it produces a store with the operator's id, the operator's route, and the fixture's rows,
because the discriminator that would have kept them apart is looking at the upstream rather
than the name. A caller asking that store for its data receives sample rows presented as
their own.

The alternative discrimination — identify the fixture by its id — makes the id the only
thing separating a demonstration from a deployment's data, which puts the whole weight of
the distinction on a string that a configured entry can also write.

## Decision

Both built-in ids are reserved. A configured entry claiming one raises `StoreIdReserved`
rather than being served the fixture's rows under its own name, and the fixture continues to
be discriminated by the absence of an upstream rather than by its id. The two built-ins are
therefore always present and always themselves, and an operator whose store already carries
a reserved id renames it.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Reserve both ids and refuse a configured entry claiming one** *(chosen)* | The demonstration surface and a deployment's data cannot be confused for each other, and the refusal names the collision at the moment it is introduced. | Two ids are unavailable to every deployment, and a store already carrying one has to be renamed. |
| Let a configured entry shadow a built-in | Nothing is reserved; an operator's namespace is entirely their own; no error to handle. | Lost on cross-store disclosure: with the fixture discriminated by the absence of an upstream, a shadowing entry is served the fixture's rows as its own data, and a caller cannot tell sample rows from theirs. |
| Discriminate the fixture by id rather than by upstream, and allow shadowing | Shadowing becomes a clean replacement; an operator can override a built-in deliberately. | Lost on fragility: the id becomes the only thing keeping the two apart, so any path that normalizes, truncates or defaults an id merges them, and the reservation would be doing the work anyway. |
| Namespace the built-ins under a prefix no configured id may use | Frees both plain ids; the reservation becomes a rule about a character rather than about two strings. | Lost on cost of the constraint it replaces: it forbids a prefix on every id instead of two whole ids, which is a larger restriction for the same protection, and the prefix is a name an operator has to learn. |

## Criteria

1. **Whether a caller can be served one store's rows under another store's name.** **This
   criterion decided.** The fixture is discriminated by the absence of an upstream, so a
   shadowing entry produces exactly that outcome, and rows presented under the wrong store
   name are indistinguishable from correct data to the party consuming them.
2. **Fragility of the discriminator** — whether the thing separating the fixture from a real
   store is a structural property or a string that configuration also writes.
3. **Clarity of the failure** — a refusal at registration names the collision; shadowing
   produces plausible data and no error at all.
4. **Size of the namespace taken from every deployment** — two ids, permanently. The chosen
   option pays this.

## Consequences

The built-ins are a fixed point: every deployment has them, they are what they say they
are, and a fresh surface renders with no configuration and no secrets. The fixture's
discrimination stays structural, which means it survives id normalization, truncation and
whatever future path introduces a default id, rather than depending on a string staying
unique. A collision is reported when an entry is registered, with the reserved id named.

The cost accepted: two ids are gone from every deployment's namespace, forever, including
deployments that will never use the fixture or the loopback store. An operator whose store
already carries one has to rename it — which, under derived credential and binding names,
means rotating its secret name and its binding name too, so a reserved-id collision is not
a one-line fix. The reservation also has to be stated somewhere an operator reads before
they pick an id, or the refusal arrives after the name is already in use elsewhere.

## Revisit triggers

- The built-in set grows beyond two, at which point the reserved namespace is large enough
  that a prefix is a smaller imposition than an enumerated list.
- A reserved id is observed colliding with a natural store name often enough that the
  rename cost is recurring rather than incidental.
- The fixture gains a structural marker independent of both its id and its upstream,
  removing the disclosure path that makes the reservation necessary.
