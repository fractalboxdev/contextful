# 0257 — An access explanation names the groups on the path and not their members

**Status:** accepted 2026-09-18
**Decides:** `visibility.explain.refusal.individual-named`

## Context

The explanation prints the closure it walked: the chain of groups from a subject's source
principals to the grant that admitted or failed to admit them. Each node on that chain is a
group, and each group has members the mirror already holds in `access_group_members`.

Rendering those members is the obvious next step for a reader who wants to act on the
output. Told that a platform group reaches the resource, the natural question is who is in
it, and the tables can answer.

Answering turns the diagnostic into a disclosure about people who never asked anything. The
subject asking about their own denial receives a list of colleagues who do reach the
resource — a statement about those colleagues' access, delivered to someone the source
never shared it with. That is a different kind of fact from the one the reader came for,
and the reader's own entitlement to the resource has no bearing on it.

Delivery has the same shape. A denial is a statement about two things at once: that a named
person does not reach something, and that the something exists. Posting the explanation
where a team reads it discloses the second half to everyone present, including people for
whom the resource's existence is itself the sensitive fact.

## Decision

A reader-facing explanation names the groups along the path and not their members. An
explanation rendering member identities of a group on the path raises
`VisibilityIndividualNamed`. The explanation reaches the person who asked for it and no
audience.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Group names on the path, delivered to the asker alone** *(chosen)* | The diagnostic discloses nothing about third parties, and the path is still specific enough to act on. | A reader debugging an unexpected denial gets the group and takes a second step to find who administers it. |
| Name the individuals holding the grant | Answers "who do I ask?" in one call, from data already held. | Loses on what the diagnostic discloses about third parties: it hands the asker a roster of who reaches a resource, which is a fact about those people and not about the asker. |
| Publish the explanation to a room so a team can act together | Removes the second step entirely; the person who can fix it is already reading. | Loses on the same criterion in a second form: stating that a named person does not reach a named resource discloses the resource's existence to everyone in the room. |
| Name members only where the asker already reaches the group | Preserves the useful answer in the common case. | Loses on the case that motivates the feature: the asker who is denied is by construction outside the group whose membership they want. |

## Criteria

1. **What the diagnostic discloses about people who did not ask.** *This criterion
   decides.* Every other consideration here is convenience for one reader, and this one is
   a disclosure to that reader about others, which the mirror has no mandate to make at
   all — the source drew an audience for the resource, not for its access list.
2. **Whether the output is specific enough to act on.** A group name is actionable; an
   unexplained denial is not.
3. **Number of steps to remediation.** Given up — one extra step.
4. **Whether the resource's existence is disclosed.** Governs delivery, not content.

## Consequences

Remediation becomes a two-step path: the explanation names the group, and the asker finds
its administrator elsewhere — a directory, an owner field, a person. Where no such lookup
exists in a deployment, the second step is a conversation, and the diagnostic's value drops
accordingly.

The accepted cost is that gap. The output is precise about the mechanism and silent about
the people, which is the right split for disclosure and the wrong split for speed. A reader
in a hurry experiences the explanation as telling them almost, but not quite, what they
need.

Private delivery costs the same way: the person who could resolve the denial does not see
it unless the asker forwards it, and forwarding is the asker's own act with the asker's own
judgment attached.

## Revisit triggers

- A group-ownership field lands in the mirror, which would let the explanation name an
  administrator without naming a membership.
- A deployment appears where group membership is already public to every subject, making
  the disclosure vacuous for that source.
- Askers are observed forwarding explanations wholesale into rooms, which would mean the
  private-delivery rule is being satisfied on paper and defeated in practice.
