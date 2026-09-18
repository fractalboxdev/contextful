# 0161 — Four guards run before a socket opens and each settles the unit permanently

**Status:** accepted 2026-09-18
**Decides:** `derive.fetch.refusal.scheme`, `derive.fetch.refusal.address-literal`, `derive.fetch.refusal.loopback-host`, `derive.fetch.refusal.host-not-listed`

## Context

The address a fetch unit dereferences comes out of a row a third party wrote. Four properties of
such an address are decidable from the string alone, and all four are cheap: the scheme, whether
the host is an address literal rather than a name, whether the name is the machine itself, and
whether the name is on the operator's list.

Each has a distinct failure if it is checked late. A scheme outside `http` and `https` reaches a
client that may honour a local file or a data payload. An address literal defeats the list
entirely — a gate that compares names has nothing to compare against a literal, so a literal
either bypasses the check or is compared against nothing and admitted. A name that resolves to the
machine turns an outward-facing engine into a caller of whatever else this daemon or its
neighbours are listening on. And an unlisted name is the whole point of the list.

The decision about permanence is separate and easy to get wrong. The tier accounts attempts per
unit and bounds them by a configured ceiling, and the natural implementation makes every failure
share that accounting. But these four refusals exist for safety, not because something was
temporarily wrong. If their markers were retryable, an operator raising `max_attempts` — a knob
that exists for flaky publishers — would revive every security refusal in the table at once,
without intending to and without being told.

The run-level question is the third one. A batch may hold hundreds of units, and one of them is
pointed at a hostile or malformed address. Aborting the run on that refusal hands a single
publisher the ability to stop everybody else's work.

## Decision

An address whose scheme is neither `http` nor `https` raises `DeriveSchemeUnsupported` before any
socket opens. A host written as an address literal raises `DeriveAddressLiteral`. A host
resolving to the machine by name raises `DeriveLoopbackHost`. A host absent from the operator's
list raises `DeriveHostNotAllowed` naming the host, and the unit records which name it was pointed
at. Each refusal settles one unit permanently, costs no request, and leaves the run to continue.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Four pre-socket guards, each settling the unit, the run continuing** *(chosen)* | No request leaves for a destination the operator did not sanction; one hostile row costs one row | A host the operator genuinely wants is settled on first encounter and adding it does not revive those units |
| Check after connecting, refuse on the response | Simpler control flow; the client's own redirect and resolution logic does the work | Lost on whether the request left: the loopback and scheme cases have already happened by the time the response is read |
| Mark a guard refusal retryable | One attempt-accounting path; a transient list error self-heals | Loses on permanence: raising an operator-facing attempt ceiling would revive every security refusal at once, silently |
| Abort the run on a guard refusal | Loud; nothing proceeds under a suspicious condition | Loses on cost: one hostile publisher stops a batch of hundreds, which is a denial of service handed to the least trusted party |
| Settle, but re-open the unit when the host list changes | Operator-friendly: adding a name fixes the past | Loses on permanence again in a slower form — the list is the operator-facing knob, so editing it becomes a way to revive a refusal, and a bulk edit revives many |

## Criteria

1. **Whether the request left** — the guarantee is about what the network saw, not what the row
   says afterwards.
2. **Revivability** — whether a refusal that exists for safety can be brought back by an
   operator-facing knob that was turned for another reason.
3. **Blast radius of one hostile row** — what a single adversarial publisher can cost.
4. **Operator legibility** — whether the marker tells the operator what to change.

Criteria 1 and 2 together decided it. Criterion 1 eliminates every post-connection design outright
and is not tradeable: a guard whose protection is evaluated after the protected event has occurred
provides none. Criterion 2 outranks the convenience of a single attempt path because the ceiling
exists for flaky publishers and would otherwise double as a security override, which is exactly
the kind of coupling an operator cannot see. Criterion 3 rules out aborting; criterion 4 is why
`DeriveHostNotAllowed` names the host and the unit records the name it was pointed at.

## Consequences

Easier: the network posture of a fetch engine is decidable from configuration plus the guards,
with no runtime observation required. A batch is robust to a poisoned row, and the audit trail
distinguishes "we refused to ask" from "we asked and it went wrong".

Harder: correcting an over-tight list is a two-part operation — edit the list, then cause the
affected units to be re-derived by whatever signal re-opens a settled unit. Until that signal
exists, the correction reaches only units encountered after the edit.

Accepted cost: a host an operator genuinely wants is settled permanently on first encounter, and
adding it to the list does not revive the units already marked. For a list that starts incomplete
against an established table, this is a silent loss of coverage proportional to how much of the
table was scanned before the list was finished; the size of that loss is a row count the operator
can query but nothing reports.

Expensive to reverse: the permanence column and the attempt ceiling are now independent by design
(see 0167). Collapsing them back would re-introduce exactly the revival coupling this rejects.

## Revisit triggers

- Settled `DeriveHostNotAllowed` markers accumulate against names the operator subsequently adds,
  observed as a growing gap between listed hosts and settled units.
- A re-derive signal for a changed parent row is decided, which makes list corrections
  retroactively applicable and changes the cost this record accepts.
- A name-resolution step is introduced before the guards, which would make the loopback guard a
  post-resolution check and change what criterion 1 measures.
