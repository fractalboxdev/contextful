# 0269 — A template allowlist is a capability that denies by default, and an ungranted template is unlisted and uncallable

**Status:** accepted 2026-09-18
**Decides:** `disclosure.template.refusal.template-grant`, `disclosure.template.refusal.template-identifier`

## Context

A grant carries several array-shaped fields and they do not all mean the same thing. Most are
constraints: they narrow what a principal may reach, and a grant carrying none of them is
unnarrowed. The template field sits in the same array and reads, syntactically, exactly like
one of those — a list of identifiers beside other lists of identifiers.

The polarity of that one field decides what every outstanding grant means the moment a
template is added to the manifest. Read as a constraint, a grant carrying no template list is
unconstrained on templates, so adding a metric to the manifest widens every grant in the
deployment at once, silently, including grants issued years earlier by people who had never
heard of that metric. Read as a capability, the same grant confers nothing, and adding a
metric reaches exactly the principals someone named.

Attenuation makes the polarity visible from the other side. A constraint narrows as it is
attenuated, so a derived token adding to a constraint list is legal and expected. A capability
confers, so a derived token adding an identifier its parent withheld would hold more than the
parent that issued it. One reading of the field makes attenuation safe by construction; the
other makes every attenuation step a place to check.

What an ungranted caller sees is a second question with the same shape. The manifest is the
operator's metric surface: template identifiers name the measurements the deployment considers
worth exposing, and the parameter lists describe how each is sliced. Listing every template
and refusing the calls hands that surface to a caller holding none of it — the caller cannot
execute anything, and it can read the operator's instrumentation strategy off the listing.

Identifiers also have to be unambiguous before an allowlist over them means anything. The
projected tool surface carries built-in tools under known prefixes; a template identifier
colliding with one produces two callable things under one name, and a grant naming that name
grants an ambiguity rather than a template.

## Decision

A grant names the templates it executes in an allowlist, with a star standing for all of them.
The field is a capability and denies by default: a grant carrying no allowlist confers no
template access at all. Attenuation subsets the parent's allowlist and adds no identifier the
parent withheld. Calling a template by guessed identifier raises
`DisclosureTemplateNotGranted`, and listing honors the caller's grants, so an ungranted
template appears nowhere. An identifier colliding with a built-in tool prefix raises
`DisclosureTemplateIdentifierCollision`, so every name an allowlist can carry resolves to one
callable thing.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Capability polarity, deny by default, listing filtered by grant** *(chosen)* | Adding a template reaches only principals someone named, and a caller learns nothing about metrics it cannot call. Attenuation is safe without a per-step check. | Every grant that should reach a template names it, so adding a metric to the manifest also touches the grants that consume it. |
| Treating the field as a restriction | Consistent with the other array fields on the grant; no grant edit needed to roll out a metric. | Lost on what an empty field confers: adding a template would silently widen every outstanding grant, and the widening is invisible at the moment it happens. |
| Listing ungranted templates while refusing the call | Discoverability — a caller can ask its operator for a named capability rather than guessing. | Lost on surface exposure: the listing enumerates the operator's metric surface, including parameter shapes, to a caller who holds none of it. |
| Allowing attenuation to add identifiers | A delegating service can mint a token for a template it knows about without holding it. | Lost on the capability reading: a derived grant would exceed its parent, which removes the one property that makes a delegation chain checkable at any point. |
| Allowing a template identifier to shadow a built-in prefix | Operators pick names freely; no reserved vocabulary to learn. | Lost on nameability: an allowlist entry that resolves to two callable things grants neither one unambiguously, and the ambiguity is resolved by dispatch order rather than by the grant. |

## Criteria

1. **What a grant carrying no allowlist confers.** **This criterion decided.** It is the only
   property that changes the meaning of grants already issued and already in use. A restriction
   reading inverts them the moment the manifest grows, and no review of the new template
   surfaces that, because the change is in the grants and nobody edited a grant.
2. **Surface exposure to an unauthorized caller** — what the listing tells a caller about
   metrics it cannot execute.
3. **Attenuation safety** — whether a derived token can exceed its parent by construction or
   only by a check that has to be present and correct.
4. **Unambiguous resolution of an identifier** — whether every name an allowlist can carry
   names one callable thing.
5. **Rollout cost** — how many artifacts a new metric touches. The chosen option is the worst
   here and it is the one cost accepted.

## Consequences

The grant's template field reads as what it is, and a reader auditing a principal's reach can
answer "which metrics can this token run" from the token alone rather than from the token plus
the current manifest. Delegation chains shrink monotonically without inspection. The listing
becomes a function of the caller, so two callers see two surfaces and neither learns the
other's.

The cost accepted: adding a metric is two edits, not one. Every grant that should reach the
new template names it, so a manifest change fans out into grant changes, and a deployment with
many narrow grants pays proportionally. A star exists for the deployments that want the other
behavior, and reaching for it converts the whole allowlist back into nothing. The consequence
of the filtered listing is that a caller cannot discover what it is missing; it learns the
identifier of a template it should hold from its operator, out of band, or not at all.

Flipping polarity later is not a code change so much as a re-audit: every outstanding grant
would have to be re-read under the new meaning before the flip lands.

## Revisit triggers

- Grant fan-out on a manifest addition becomes the dominant cost of adding a metric, and star
  allowlists spread as the workaround.
- A discovery need appears that the filtered listing cannot serve — a caller that must
  negotiate for a capability it cannot name.
- The built-in tool prefix set changes in a way that retroactively collides with identifiers
  already granted in deployed manifests.
