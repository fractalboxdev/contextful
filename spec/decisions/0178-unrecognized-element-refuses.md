# 0178 — An element the profile does not name refuses, and is never admitted as unconstrained

**Status:** accepted 2026-09-18
**Decides:** `authority.profile.refusal.unrecognized-element`

## Context

A credential is blocks of facts, checks, rules and restrictions. The profile names which
of those this engine admits. A checkpoint therefore meets elements it does not recognize
routinely: a credential minted against a newer profile version, a credential minted by a
deployment that extended its own, a credential an attacker assembled to see what happens.

The usual convention for an unknown field is to skip it. That convention is safe where
unknown fields carry additional information, and it is unsafe here, because the elements
in question are mostly *restrictions*. A restriction subtracts authority. Skipping a fact
a checkpoint does not understand withholds something from the holder; skipping a
restriction a checkpoint does not understand hands the holder everything that restriction
was withholding. The two failure directions are not symmetric, and the second one is
reachable by anyone who can append a block — a holder appends a restriction the checkpoint
skips, presents the credential, and reads more than the block it signed says it may.

Classifying an element into the safe direction is not available either. Whether an element
narrows or widens is a statement about its semantics, and an element the profile does not
name is precisely one whose semantics the engine does not have. A checkpoint that tries to
decide "this looks like a fact, that looks like a check" is guessing about attacker-shaped
input.

Refusing also has to survive the refusal. A restriction field the profile declines to
evaluate stays declared in the wire shape rather than being deleted, because claims
parsing skips an unknown key: a deleted field would let a credential carrying it verify
with the field unread and its holder unconstrained — the same defect arriving by a
different door.

## Decision

A credential carrying a block version, predicate, rule or restriction the profile does not
name raises `ProfileElementUnrecognized`. It is never admitted as though it were
unconstrained. A field the profile refuses to act on stays declared in the wire shape, so
an unrecognized value is refused rather than silently unread.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse any unnamed element** *(chosen)* | The set of things a credential can say equals the set the checkpoint understands, so no element ever takes effect as nothing | Widening the profile is a version bump, and a checkpoint running an older profile turns back credentials a newer mint produces |
| Skip the unknown element and admit the rest | Forward compatibility comes free; old checkpoints keep serving new credentials | Lost on the direction of the error: a skipped restriction widens the credential, and appending one is something any holder can do |
| Skip unknown facts, refuse unknown restrictions | Keeps forward compatibility where it is safe and refuses where it is not | Lost on classification: deciding which an unnamed element is requires the semantics the profile does not have, so the classifier is guessing about attacker-shaped input |
| Admit the credential with its authority reduced to nothing | Fails closed without a refusal; nothing is ever wrongly widened | Lost on legibility: a credential that admits and then reads nothing is indistinguishable from an unprovisioned link or a withdrawn grant, so the defect presents as an empty result |
| Delete refused fields from the wire shape | A smaller shape carrying only what is evaluated | Lost on the same direction of error: parsing skips an unknown key, so a credential carrying the deleted field verifies with it unread and its holder unconstrained |

## Criteria

1. **Direction of the error** — whether the failure mode of not understanding an element
   is a denial or a widening.
2. **Classification cost** — what the checkpoint must know about an element to route it.
3. **Legibility** — whether the outcome tells its holder what went wrong.
4. **Forward compatibility** — whether an older checkpoint keeps serving newer
   credentials.

Criterion 1 decided it, over criterion 4. Forward compatibility is the real and
continuing cost, and it is the property the skip convention exists to provide. It loses
because the elements at stake are the ones that subtract authority: an engine that
tolerates unknown elements has handed every holder a way to remove its own restrictions
by encoding them in a form the checkpoint will not read. Criterion 2 then closes the
compromise, since the safe half of the skip cannot be identified without the semantics the
refusal is admitting it lacks.

## Consequences

Every element a checkpoint admits is one the profile names and the engine has a mapping
for, which is what makes introspection able to report a credential's declared scope
without running anything, and what lets the formal model quantify over a finite set. There
is no path by which a restriction takes effect as nothing.

The cost accepted is version rigidity. Widening the profile mints a new version rather
than relaxing an existing one, and until every checkpoint implements it, a credential
minted against the new profile is refused by the old ones. A mixed fleet therefore mints
against its oldest deployed profile, so the fleet moves at the speed of its slowest
checkpoint. The wire shape also carries fields nothing evaluates, which reads as dead
weight to anyone who has not met the deleted-field defect.

Reversing toward skipping is a decision about the whole authority surface rather than a
flag: every refusal downstream of this one assumes an admitted credential says only
things the engine understands.

## Revisit triggers

- Mixed-version fleets become common enough that minting against the oldest deployed
  profile is a real constraint on shipping, rather than a bookkeeping detail.
- An element class appears that is provably additive by construction — carried in a space
  the profile reserves for facts that cannot narrow — which would make criterion 2
  satisfiable for that space.
- A holder is observed unable to diagnose a refusal because the message names an element
  it cannot see, which would re-open what the refusal reports rather than what it decides.
