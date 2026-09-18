# 0010 — One declaration produces byte-identical output on every control-plane target, with the single-process run as the reference

**Status:** accepted 2026-09-18
**Decides:** `topology.deploy.refusal.target-shape`, `topology.deploy.refusal.parity-gate`

## Context

The same engine binary and the same `contextful.toml` deploy to every provider. What
differs is which of that provider's primitives back the control loop, the read path and
heavy compute. A managed platform's scheduler, queue, object store and per-object database
are not the same mechanisms as a single process's in-memory tick, embedded queue and local
catalog file, and a provider that lacks a primitive lacks a shape the declaration might
name.

The promise a deployer is buying is that the provider is an operational choice, not a
semantic one — the same declaration produces the same tables wherever it runs. That promise
is worth exactly as much as the evidence behind it, and the failure mode without evidence
is silent: a target that rounds a timestamp differently, orders a batch differently, or
compacts at a different boundary produces plausible output that nobody questions.

Some divergences only appear with accumulated state. A first run on an empty store agrees
everywhere; the disagreement shows up after a compaction boundary, a cursor wrap, or a
retry that lands differently on one target's queue semantics.

## Decision

One declaration runs on every control-plane target with a managed mapping and produces
byte-identical table parts. The single-process reference run is the expected output, and
each target is diffed against it inside that target's own object store after a 24 h soak. A
byte difference raises `ParityDivergence`, naming the target, the table and the first
differing part. A target whose primitives cannot express a shape records the absence in its
profile, and a profile claiming a shape its target does not provide raises
`TargetShapeUnsupported`, naming the shape and the primitive it lacks. A parity-shaped
feature that runs on one target is held until it runs on the others or is expressed through
a portable abstraction.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Byte equality against one reference run, soaked before comparison** *(chosen)* | A divergence is a diff with a target, a table and a first differing part — detectable without a reader noticing bad data. | A parity-shaped feature waits for its slowest target or is expressed through a portable abstraction, and a soak runs before every comparison, so the signal is slow. |
| Per-target expected outputs | Each target's quirks are captured; no feature is held for another target. | Lost on detectability: a divergence arrives as a fixture update, which is the same action as accepting it. The property stops being checkable by construction. |
| Parity asserted by review rather than by diff | No soak, no fixtures, no comparison infrastructure. | Lost on the same criterion: review cannot see a byte difference in a columnar part, and the differences that matter are exactly the ones that look correct. |
| Shipping a parity-shaped feature on one target and backfilling | Features reach users at the speed of the fastest target. | Lost on the guarantee itself, which stops holding the moment one target is ahead — a deployer's choice of provider then changes what the product does. |

## Criteria

1. **Detectability** — whether a divergence is caught mechanically rather than by a reader
   noticing wrong data. **This criterion decided.** Per-target expectations convert a
   divergence into a fixture update, which is indistinguishable from accepting it; only a
   single reference makes the difference something the gate can refuse.
2. **Whether a deployment choice can change results** — the property the promise rests on.
3. **Coverage of state-dependent divergence** — the 24 h soak places accumulated-state
   differences inside the comparison rather than outside it.
4. **Delivery speed** — how fast a feature reaches the first target. The chosen option is
   the worst here and lost this criterion deliberately.

## Consequences

The reference run is a single process with an in-process scheduler and a local catalog,
which makes the expected output cheap to produce and easy to reason about. Target profiles
become honest: an unexpressible shape is recorded as an absence rather than emulated
approximately, and a profile that overclaims is refused at the named primitive.

The cost accepted: the release cadence of any parity-shaped feature is the cadence of its
slowest target, and the alternative — a portable abstraction — is usually more work than
the direct implementation on either target. The soak makes the signal slow: a regression
is reported a day later at the earliest, so parity failures are found well after the change
that caused it.

Reversing this toward per-target expectations is easy to do incrementally and effectively
irreversible, since every accepted per-target fixture is a divergence the reference no
longer describes.

## Revisit triggers

- Byte equality is broken by something outside the engine's control — a provider's storage
  layer rewriting parts — making the comparison report provider noise.
- The 24 h soak is repeatedly too short to surface a class of divergence that reaches
  production anyway.
- Held features accumulate to the point that the slowest target is setting the product's
  roadmap rather than its guarantees.
