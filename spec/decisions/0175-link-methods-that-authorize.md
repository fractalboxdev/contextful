# 0175 — Only a provider-verified link authorizes, and an operator-asserted one explains

**Status:** accepted 2026-09-18
**Decides:** `authority.identify.refusal.link-method`

## Context

A source keeps its own principals. A message carries the account that posted it, a
document carries the account that wrote it, and neither is the subject a credential
carries. An identity link is the mapping between the two, and it is what an authorization
join reads to decide which of a source's rows a caller reaches.

The mapping can be established three ways, and they are not the same kind of fact. A
directory provisioning feed writes it from a field the identity provider verified; a
sign-in writes it from the subject claim the provider issued; an operator writes it by
hand because they know who the account belongs to. The first two are assertions by the
party whose job is verifying identity. The third is an assertion by a party who is
usually right.

Usually right is the problem. The mapping's whole security value is that it cannot be
chosen by the person it maps. A link made from a provider-verified field has that
property. A link an operator asserted has it only as long as the operator asserted it
from evidence rather than from resemblance — and the evidence they most often have is a
display name or an address on a profile page, both of which are editable by their own
holder anywhere single sign-on is unenforced. A person who picks their own display name
picks their own link, and a confidence score attached to the match does not change that:
a high-confidence match on an attacker-chosen string is high-confidence wrong.

Operator-asserted links are still worth storing. An operator staging a mapping before
provisioning is wired has nowhere else to put it, and an explanation trace that says why
a row was attributed to a subject loses its evidence if the row is discarded.

## Decision

`method` records how a link was made — `scim_email`, `oidc_sub` or `operator_asserted` —
and `confidence` is recorded beside it without relaxing anything. The two methods that
authorize are `scim_email` and `oidc_sub`. A link whose method is `operator_asserted`
authorizes nothing; an authorization join consuming such a row raises
`AuthorityLinkUnverified`. The row exists for an explanation trace and for staging a
mapping.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Store every method; authorize on the two provider-verified ones** *(chosen)* | The link cannot be selected by the person it maps, and staging and explanation keep their rows | A deployment with no identity provider authorizes by link not at all, so it reads nothing from a source through links |
| Do not store operator-asserted links at all | One kind of row, no method to check, nothing to misread | Lost on staging and explanation: an operator building a mapping ahead of provisioning has nowhere to put it, and an attribution trace loses the only record of why a row was attributed |
| Authorize operator-asserted links above a confidence threshold | An operator can unblock a reader without waiting for provisioning | Lost on the threat: confidence measures agreement between two strings, and the string the match runs against is editable by its own holder, so a high score is an attacker's high score |
| Authorize them only for read, never for write | Halves the exposure while keeping the convenience | Lost on where the exposure is: reading another principal's rows is the disclosure the link exists to prevent, so the half retained is the half that matters |
| Authorize them where an operator countersigns each row | Keeps a human decision in the path with an audit trail | Lost on effort for what it buys: the countersignature attests the operator's belief, which is the fact already in question |

## Criteria

1. **Unforgeability of the mapping** — whether the person being mapped can influence what
   they are mapped to.
2. **Explanation** — whether an attribution can be traced to the evidence that produced
   it.
3. **Staging** — whether a mapping can be prepared before it can authorize.
4. **Reach without a provider** — how much a deployment with no identity provider can do.

Criterion 1 decided it, over criterion 4. Reach without a provider is a real cost and the
loudest one: a small deployment that has not stood up provisioning gets nothing from
links at all, and the obvious relief — honor the operator's assertion — is available and
cheap. It loses because the link is the single point where a source's principals become a
caller's reach, and a mapping the mapped party can influence turns that point into a
self-service one. Every other guarantee in the read path sits behind it.

## Consequences

An authorization join reads one column and needs to know nothing about how carefully a
link was made. Confidence stays recorded and stays inert, so a scoring change can never
widen anyone's reach. Staging is safe: an operator can write the whole mapping in advance
and it authorizes the moment provisioning confirms it, with no cutover.

The cost accepted is that a deployment without SCIM or OIDC cannot authorize by link at
all. Its readers reach nothing through a source's own principals, and the workaround is
to stand up provisioning rather than to configure around it. That is a real barrier for
the smallest deployments, which are the ones most likely to have an operator who
genuinely does know every account by name.

Reversing this is cheap in code and expensive in what it invalidates: any reach granted
through an operator-asserted link is reach nobody verified, and there is no later
evidence with which to audit it.

## Revisit triggers

- A link method appears that is neither provider-verified nor operator-asserted — a
  source-side proof of possession, say — and needs classifying.
- A deployment class with no identity provider becomes a supported shape, which makes
  criterion 4 a requirement rather than a cost.
- An operator-asserted link is found being consumed by a join that is not recognized as
  an authorization join, which means the refusal's coverage is narrower than its
  statement.
