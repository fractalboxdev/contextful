# 0236 — The write path refuses a credential-shaped value before it reaches storage

**Status:** accepted 2026-09-18
**Decides:** `enforcement.resist.refusal.credential-shaped-value`

## Context

Sources carry secrets by accident. A message pasting an access key, a configuration file
committed to a repository, a support ticket quoting a bearer token, a log line printing a
connection string — every ingestion path eventually lands one. The pattern is recognizable:
credential formats are designed to be machine-identifiable, and shape matching on them is
ordinary work.

What is unusual is where a stored credential ends up. A row is not one copy. It lands in
columnar bytes in the operator's bucket, in sidecar indexes built over the column, in the
run path's durable record of the step that produced it, and in every replica that pulls the
snapshot afterwards. A full replica on a laptop holds it. Retrieval indexes make it
findable by anyone with read access to the table, which is a wider set than the people who
could see the original source object.

The write path already removes values before columnar bytes land, and what it removes leaves
no copy downstream, the durable record included. That capability is what makes the question
here a real choice rather than a wish: the boundary exists, and the question is whether a
credential-shaped value is stopped at it or transformed at it.

The value of the row containing such a value is also asymmetric. A message whose content is
a leaked key is worth close to nothing as context, and the same is true of most rows the
shape match fires on. The exceptions are real but narrow — a corpus about security, a
rotation log — and they are exceptions an operator knows about in advance.

## Decision

The write path raises `EnforceCredentialShapedValue` on a value matching a credential
shape, before that value reaches storage. The refusal is per value rather than per batch, so
the surrounding rows land. A source that legitimately carries credential-shaped text is
ingested under an operator-declared exemption.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the value at the write boundary, before storage** *(chosen)* | The value is absent from the columnar bytes, the indexes, the durable record and every replica at once, because it never entered any of them. | A source legitimately carrying credential-shaped text — a security corpus, a secret-rotation log — cannot be ingested without an operator-declared exemption. |
| Mask it at write time and store the masked row | The row survives, the value does not, and the surrounding context stays queryable. | Loses on the value of what is retained: a row whose content was the credential and nothing else masks down to a row that says a credential was here, which is not context anyone queries for. |
| Store it and flag it for an operator | Nothing is lost; a human decides. | Loses on reach: by the time the flag is raised the value is in the durable record and in every replica that has pulled since, and removing it from all of them is a different and harder operation than never writing it. |
| Refuse the whole batch containing the value | An unambiguous signal; no partial ingestion to reason about. | Loses on proportion: one pasted key in a large pull stops the pull, and the failure recurs on every retry until a human edits the source. |
| Detect at query time and withhold the matching cell | No write-path cost, and the rule can change without re-ingesting. | Loses on reach in the strongest form: the value sits in cleartext in the stored bytes, so an object-store credential reveals it and the query-time control never runs. |

## Criteria

1. **The value of the row against the cost of holding it** — what a retained
   credential-carrying row is worth as context, weighed against its being a liability in
   every replica and every record. *This criterion decides.* Most rows the shape match
   fires on are worth nothing once the credential is gone, so the options that retain
   something retain the liability and not the value.
2. **Reach of the control** — how many copies a given placement of the check covers.
3. **Proportion of the refusal** — whether one bad value costs one value or a whole pull.
4. **Revisability** — whether the rule can change without re-ingesting, which is the
   criterion the chosen option loses on.

## Consequences

Stored bytes contain no credential-shaped value, which holds for the durable record and for
every replica by construction rather than by a sweep, and a stolen object-store credential
yields nothing of that class.

The cost accepted is legitimate content that cannot be ingested by default. A security
corpus, a rotation log, documentation quoting example keys — each of these needs an
operator-declared exemption before it lands, and the operator meets that requirement as a
refusal during a pull rather than as a note in advance.

Shape matching also carries false positives, and a false positive here is data loss at the
boundary: a value that resembles a credential and is not one is refused, the row lands
without it, and nothing downstream records what the value was. That is the same property
that makes the control strong.

The rule is fixed at write time, so tightening or loosening it applies to future pulls
alone. Improving the matcher does not remediate what earlier pulls accepted, and relaxing it
does not recover what earlier pulls refused.

Reversing toward store-and-mask is expensive because the refusal's guarantee is stated over
the durable record and the replicas, and a stored-then-masked history would have to be
remediated everywhere those copies went.

## Revisit triggers

- False-positive rates on real sources are measured and land high enough that operators
  declare blanket exemptions, which would make the control's effective coverage smaller
  than its stated one.
- A remediation path appears that can remove a value from the durable record and from pulled
  replicas, which is the capability whose absence decides the reach criterion.
- Exemption declarations become common enough to constitute an ordinary ingestion mode
  rather than a narrow one.
