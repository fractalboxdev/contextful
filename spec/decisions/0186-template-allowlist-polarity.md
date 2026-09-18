# 0186 — The query-template allowlist denies by default, against the polarity of every other grant dimension

**Status:** accepted 2026-09-18
**Decides:** `authority.grant.refusal.ungranted-template`

## Context

Every other dimension of a grant is a restriction: an absent aggregate constraint leaves
aggregation unconstrained, an absent row ceiling imposes none, an absent tenant scope
leaves the grant at table grain. Absence means "not narrowed here". A reader of a grant
learns what it cannot do by reading what it says.

Query templates do not fit that shape. A template is a named, parameterized compound
read declared in a project's manifest — a pre-approved query someone wrote once and
blessed. The manifest grows over time as templates are added, and templates are added
without reference to any credential that exists. Under restriction polarity, a
credential minted with no opinion about templates would gain every template added after
it was minted, for as long as it lives.

There is a second surface. Templates are projected as a tool listing to an agent, so
visibility and invocability are two separate decisions about the same identifier, and an
agent that can read a listing can also guess an identifier that is not in it.

## Decision

The query-template allowlist is a granted capability with deny-by-default polarity. An
absent list authorizes no template. A star authorizes every template the manifest
declares. A child names only identifiers its parent named or covered. Invoking a
template outside the list raises `GrantTemplateNotAllowed`, and a template the credential
does not cover is absent from the projected tool listing — so guessing an identifier
reaches the same refusal as reading one from a listing that does not contain it.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Deny by default; star for all; refusal on invocation and absence from the listing** *(chosen)* | A credential's template reach is fixed at mint. The manifest can grow without widening anything already issued. | A polarity inconsistency inside one grant. An operator minting for a template user names templates explicitly, every time, and adding a template means amending credentials. |
| Restriction polarity, matching the other dimensions | One rule for the whole grant; nothing special to remember. | Loses on blast radius: a long-lived credential silently acquires every template added after it was minted, and the acquisition leaves no record because nothing about the credential changed. |
| Hide uncovered templates from the listing, but allow invocation by name | Keeps discovery honest without a second check. | Loses on enumeration: the identifier space is guessable and an agent that guesses reaches the read, so the listing becomes a suggestion rather than a boundary. |
| A separate template capability outside the grant | Clean polarity in the grant; templates modelled as their own thing. | Loses on completeness: two authority objects to check, derived and narrowed separately, and one of them can be forgotten at a call site that checks the other. |
| Templates inherit the grant over the tables they read | No new dimension at all. | Loses on what a template is: a template is pre-approved because of how it reads, not only what it reads, so table coverage is not the property being granted. |

## Criteria

1. **Blast radius over time** — whether a credential's reach can grow after it is minted
   without anyone acting on that credential. *This criterion decides.* Every other cost
   here is paid once, by an operator, visibly; a credential that silently widens as a
   manifest grows is a failure that nobody is present for.
2. **Enumeration resistance** — whether hiding an identifier is the same as denying it.
3. **Completeness of the check** — whether one authority object governs the surface.
4. **Uniformity within the grant** — one polarity rule rather than two. This is the
   criterion given up.

## Consequences

Adding a template to the manifest is now safe by default: no existing credential gains
anything. That makes the manifest an ordinary artifact to edit rather than a change that
has to be reasoned about against the set of live credentials.

An operator minting for a template consumer does more work, and does it again whenever
the set changes. The star form exists for exactly the case where a holder legitimately
gets everything, so the work is not forced where it buys nothing.

The accepted cost is the inconsistency itself. A reader of a grant now holds two rules:
absence narrows, except for templates, where absence denies. That is a genuine hazard —
it is the kind of asymmetry that is read past.

Reversing the polarity later is expensive in the dangerous direction: flipping to
restriction polarity would instantly widen every credential that carries no template
list, which is most of them, with no mint and no record.

## Revisit triggers

- Grant dimensions generally move to explicit presence rather than absence-means-open,
  which would remove the inconsistency by making the rest match this one.
- Template identifiers become unguessable by construction, which would make listing
  visibility a boundary on its own and reopen the invocation check.
- Manifests become per-credential rather than per-project, so a template's existence is
  already scoped and the growth problem that decided this stops occurring.
