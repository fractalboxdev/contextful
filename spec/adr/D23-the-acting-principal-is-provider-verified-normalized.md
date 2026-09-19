# D23 — The acting principal is provider-verified, normalized once, fixed per chain

**Status:** accepted

## Context

The principal a credential acts for is the key links join on, the value audit names, and the authorship column landed rows carry. Derivation is offline, and the strings a similarity match or an operator belief rests on are editable by the party seeking access.

## Decision

Only an identity provider makes a principal; the mint checks and normalizes it once, and nothing downstream rebinds or reinterprets it.

- `authority.identify` checks every subject value at the mint: non-empty, at most 256 B, no control character, no surrounding whitespace. The command line trims a padded value; the automated exchange refuses it.
- A link authorizes only when its method is `scim_email` or `oidc_sub`; an `operator_asserted` link explains and stages, conferring nothing. `disclosure.reach` consumes verified links alone, and confidence enters no threshold.
- A derivation naming a different `on_behalf_of` from its parent refuses; acting for another person goes through issuer-mediated exchange.
- `authority.issue` refuses a write or execute grant naming no `on_behalf_of`, so a null authorship value has exactly one cause.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Verify at the provider, normalize at the mint, fix per chain *(chosen)* | — | Directories with longer identifiers are inexpressible; unprovisioned accounts read nothing; a tool acting for many people pays a round trip per person. |
| Check and normalize at each point of use | Consistency | One padded identity would behave as two principals. |
| Authorize heuristic or operator-asserted links above a threshold | Failure asymmetry | A false match would be a silent source-wide disclosure. |
| Allow rebinding within a parent-carried set | Offline forgery | The mint of the parent would choose whom every descendant impersonates. |
| Refuse an unbound write at the write, not the mint | Where the failure lands | The credential would admit and fail at its first useful action. |

## Consequences

- Two surfaces treat a padded value differently, so a script calling the command line accepts what the exchange refuses.
- Credentials minted before the principal-required rule verify until they lapse.

## Revisit

- A directory's identifiers legitimately exceed the 256 B bound.
- Provisioning latency becomes the dominant reason subjects read nothing.
