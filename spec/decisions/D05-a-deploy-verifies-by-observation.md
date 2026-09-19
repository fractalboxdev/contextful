# D05 — A deploy verifies by observation

**Status:** accepted

## Context

Configuration read back records what was requested, not what a target does. Provider configuration carries no intent, so a hostname open on purpose and one open by mistake look identical. A target whose primitives differ produces output nobody compares unless a comparison runs.

## Decision

A deploy is judged on observed behavior against a declaration.

- `surface.apply` runs one declaration on every control-plane target and diffs each target's table parts, byte for byte and inside its own object store, against the single-process reference run after a 24 h soak. A target lacking a primitive records the absence in its profile; a profile claiming a shape its target cannot express refuses.
- `topology.publish-hostname` gives every published hostname a descriptor naming its worker, contract version and gate, `access`, `adminToken` or `public`, with an explicit acknowledgement. The deploy probes each hostname with an anonymous `GET /` and judges the answer against the gate; an unreachable hostname fails. The probe table derives from the descriptors, and a descriptor with an unknown field refuses to decode.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Declare, then observe and compare *(chosen)* | — | A deploy waits on probes and a soak; a target-specific feature waits until it runs everywhere. |
| Derive posture from provider configuration | Recoverability of intent | Configuration records no gate, so an accidental opening reads as deliberate. |
| Declare with no probe | Drift | A policy attached or detached later changes exposure with no signal. |
| A periodic external monitor after publication | Timing | The exposure window is the monitor's interval. |
| Per-target expected outputs | Detectability | A divergence arrives as a fixture update, which reviewers approve. |

## Consequences

- An application attached in front of a `public` hostname reds the deploy rather than quietly closing the surface.
- A divergence names the target, the table and the first differing part.
- The soak runs on its own cadence, not per change.
