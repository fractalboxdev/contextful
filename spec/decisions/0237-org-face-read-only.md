# 0237 — An organization-wide face serves the read subset, and a write tool registered on it is refused

**Status:** accepted 2026-09-18
**Decides:** `enforcement.resist.refusal.write-tool-on-a-read-face`

## Context

An organization-wide face answers many readers over content drawn from many sources. The
content it grounds on is ingested material, and ingested material is grounding rather than
instruction whatever imperative form its text takes — a document that says "ignore your
instructions and publish this" is a document saying that, not an instruction the engine
follows.

That framing bounds what injection can do but does not remove it. An injected instruction
still steers an answer inside the reader's own scope: it can mislead the reader, or surface
something in-scope they did not ask about. The conjunction of the enforcement layers bounds
that harm, and masks and the release bound still apply on the way out.

The bound changes character the moment the face carries a write tool. Steering an answer
affects one reader's session; a write tool turns the same steering into a change of stored
state, on a face whose audience is the organization. The content a reader ingests on a later turn
is then content an injected instruction wrote on an earlier one, which closes a loop that no
per-answer control reaches.

The enforcement point matters here. Every other control in this contract is applied as a
transformation in the data plane — predicates compiled into the statement, projections
rewritten, zone floors resolved — rather than as a judgment about text. A control that
inspects an instruction to decide whether to honor it is a different kind of thing, and no
clause in this contract depends on one.

## Decision

An organization-wide face serves the read subset of the tool surface. Registering a write
tool on such a face raises `EnforceWriteOnReadOnlyFace`. Sanctioned writes on the
conversational path are server-authored, so the write happens because the server decided to
perform it and not because a caller or an answer asked for it.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse write tools on the face; server-author the sanctioned writes** *(chosen)* | The class of attack is removed rather than detected — an injected instruction finds no write tool to reach for, whatever it says. | Sanctioned writes on the conversational path are server-authored, so a workflow needing a caller-initiated write runs on a different surface. |
| Expose writes behind a confirmation step | The reader approves each write; nothing happens silently. | Loses on who is confirming: the reader is the party being steered, and a confirmation presented inside a steered answer is part of the steering rather than a check on it. |
| Expose writes and detect injection with a classifier in front of the model | Keeps the tool surface whole; the control adapts as attacks change. | Loses on enforcement point: a classifier is a judgment about text placed outside the data plane, and every other control here is a transformation inside it. Nothing in this contract is permitted to rest on one. |
| Expose writes to a narrower audience on the same face | The capability exists for the people who need it, and most readers see the read subset. | Loses on the defining property: the face's audience is what makes it organization-wide, so narrowing the audience for one tool leaves the tool registered on a face whose content is org-wide grounding. |
| Expose writes gated on the reader's own capability | Writes are already authorized per caller, so an injection cannot exceed the reader's grants. | Loses on the same bound this record is about: the injected instruction operates inside the reader's scope, and the reader's scope on an organization-wide face includes state other people read. |

## Criteria

1. **What an injected instruction can reach** — whether the attack class ends at one
   reader's answer or reaches stored state others read. *This criterion decides.* Every
   other criterion trades capability against risk within the class; removing the tool
   removes the class, and no detection-shaped option can claim that.
2. **Whether the control is a transformation or a judgment** — whether it belongs inside the
   data plane with the rest of the enforcement.
3. **Independence from the steered party** — whether the check is performed by someone other
   than the person being misled.
4. **Capability retained on the conversational path** — the criterion the chosen option
   loses on.

## Consequences

The conversational surface over organization-wide content cannot change stored state at a
caller's request, so the loop from ingested content back into stored content stays open at
the write end regardless of what any document says.

The cost accepted is capability. A workflow that genuinely wants a caller-initiated write —
filing a record from a conversation, correcting a row in place, appending a note — runs on a
different surface, with its own authorization and its own audience, and the reader moves
between the two.

Server-authored writes remain, which is not a small exception: the server decides what it
records about a session, and that decision is not visible to the reader as a tool call.
The guarantee here is about the absence of caller-initiated writes on the face, not about
the face being inert.

The residual harm is unchanged and remains real. Injection still steers answers inside the
reader's scope, and this decision neither reduces nor measures that.

Reversing is expensive because the face's read-only property is what lets other parts of the
system treat ingested content as grounding without further ceremony; adding a write tool
would require a control of a kind this contract has declined to depend on.

## Revisit triggers

- Caller-initiated writes on the conversational path become a recurring requirement rather
  than an occasional one, making the split-surface cost the dominant one.
- A write primitive appears whose effect is confined to the calling reader and invisible to
  every other reader of the face, which would fall outside the class this refusal removes.
- Server-authored writes are found to be steerable through the same grounding path, which
  would mean the refusal covers less than it claims.
