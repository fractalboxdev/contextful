# 0155 — The adapter declares where inference physically happened and the operator's zone key is advisory

**Status:** accepted 2026-09-18
**Decides:** `derive.bind.refusal.advisory-zone`

## Context

Every derived row carries a zone column stating where the words on it were produced. That
column is what an enforcement rule reads when a deployment says transcripts of its recordings
are never produced off-device, and what a later reader consults when asking whether a given
passage ever left the machine.

Placement is the one property of a deployment that a later reader cannot reconstruct from the
store. Everything else about a derived row can be re-derived or re-checked: the engine
identity is a digest over resolved binaries and arguments, the parent key is in the row, the
citation interval is in the passages. Whether the inference ran in this process, in a
subprocess on this machine, on a host inside the perimeter, or at a vendor is not recoverable
from anything the row contains — it was a fact about the moment of execution.

Two parties could state it. The adapter knows what it actually does: a stub answers from a
canned transcript with no I/O, a local binary runs on this machine, a vendor client opens a
socket to a public endpoint. The operator writes a `zone` key in the `[derive.<name>]` block,
which is convenient and is also a configuration value the operator can edit at any time.

If the row's value came from that key, the enforcement rule and the configuration it is
supposed to govern would be the same text. An operator relabelling a vendor engine as a local
one would satisfy an on-device policy by editing one line, and the zone column would record
the relabelling as history.

## Decision

`Locality` is `OnDevice`, `OnDeviceWithPublicEgress`, `OnPrem` or `PublicCloud`, and each maps
to one zone tag. The adapter declares its locality and that value lands on every row the
engine produces. The `zone` key in an operator's binding is advisory; a row whose zone column
is taken from it raises `DeriveAdvisoryZone`.

The tag an `OnDeviceWithPublicEgress` adapter writes matches no zone entry until an operator
widens the list deliberately. A request line carrying an address a third party wrote leaves
the device, and a tag claiming otherwise would be false.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **The adapter declares it; the operator's key is advisory** *(chosen)* | The one unrecoverable property is stated by the party that observes it, and no configuration edit changes it | A link engine's tag matches no existing zone entry, so any zone check reading it fails closed until an operator widens the list deliberately |
| Taking the operator's declared zone as the row value | Zero adapter obligation; an operator can describe a private vendor deployment accurately | Lost on recoverability: placement cannot be reconstructed after the fact, and an operator able to relabel a remote engine as a local one by config edit turns zone policy into prose |
| Adapter value, overridable by the operator's key | Accuracy by default with an escape for unusual deployments | Lost on the same criterion: an override is the relabelling, just with an extra key to write |
| Inferring locality from the driver | One rule, no adapter declaration, no drift | Lost on precision: `exec` covers both a local binary and a pinned script that calls a vendor API, so the driver does not determine placement |
| Recording both values on the row | Full information; a reader can compare | Lost on the reading rule: two columns describing one property means every consumer and every enforcement rule picks one, and the pick is where the relabelling moves to |

## Criteria

1. **Whether the property can be recovered from the store after the fact** — whether a wrong
   value can ever be detected or corrected later. **This criterion decided it, alone.** For a
   recoverable property a convenient source with occasional errors is tolerable, because the
   errors are findable. Placement has no second witness: a wrong value is indistinguishable
   from a right one for the life of the row.
2. **Whether policy and the configuration it governs are the same text** — whether satisfying a
   rule can be done by editing the rule's input.
3. **Operator expressiveness** — whether a deployment can describe an unusual placement, such
   as a vendor API running inside its own perimeter.
4. **How a zone check behaves on an unrecognized tag** — open or closed.

## Consequences

An `OnDeviceWithPublicEgress` engine writes a tag no zone list admits until an operator adds
it. A deployment enabling link previews under a zone allowlist gets a refusal on the first
unit and widens the list deliberately, in a reviewable edit. That is the cost accepted:
enabling a legitimate configuration takes a deliberate step, and the first encounter is a
failure rather than a warning.

Operator expressiveness is genuinely reduced. A deployment running a vendor engine inside its
own perimeter cannot say so by configuration; the adapter it uses declares `PublicCloud` and
that is what the rows say. Fixing it means an adapter, not a key.

What is now expensive to reverse: zone values on committed rows are the audit record. Making
the operator key authoritative later would leave two populations whose zone columns were
produced by different parties under the same name.

## Revisit triggers

- An adapter class arrives whose placement is genuinely a deployment property rather than an
  implementation property — a vendor client pointed at a self-hosted endpoint, where the
  adapter cannot know which it is.
- The zone vocabulary grows a tag for egress-bearing on-device work that an allowlist admits
  without a manual widening.
