# D26 — Query templates are deny-by-default capabilities

**Status:** accepted

## Context

A query template is an operator-reviewed statement exposed as a tool, pre-approved for how it reads rather than only for what tables it reads. Every other grant dimension restricts: an absent value means no restriction. Template identifiers are guessable, and a template added after a credential was minted changes nothing about that credential.

## Decision

A template is reachable only by a grant that names it, and it executes exactly the statement reviewed.

- `authority.grant` treats the template allowlist as a capability: an absent list authorizes none, a star authorizes every declared template, and a child names only identifiers its parent covered. An uncovered template is absent from the tool listing and refuses on invocation.
- `disclosure.template` binds arguments strictly: missing, unknown and mistyped arguments refuse, and nothing is coerced or defaulted. Placeholders cover exactly the declared parameters, a declaration holds one statement, and an identifier colliding with a built-in tool prefix refuses; all validate at manifest check and again at startup.
- The statement runs unrewritten, outside the caller-facing statement guard, under every enforcement layer. It names only plain store tables: no table functions, bare paths or qualified catalogs.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Deny-by-default allowlist, strict binding, unrewritten statement over plain tables *(chosen)* | — | One grant carries two polarities; adding a template amends every grant that consumes it; a parameter rename breaks callers with no window. |
| Restriction polarity like other dimensions | Blast radius | Long-lived credentials would silently gain every later template. |
| Hide from the listing but allow invocation by name | Enumeration | A guessed identifier would reach the read. |
| Coerce or default arguments | Reviewed-statement fidelity | A caller would receive a row set for values it never sent. |
| Gate templates through the statement guard | Authorship | It would refuse exactly the reads templates exist to express. |

## Consequences

- Identifier confinement is the sole protection on that surface; a defect in it has no second layer.
- Manifest authors meet template validation failures at startup, before any caller.

## Revisit

- Operators routinely grant star, making explicit naming a formality.
- A store feature needed through templates requires a non-plain identifier.
