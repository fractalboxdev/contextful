# 0287 — An unrecognized or unserviceable trigger selection refuses at startup rather than downgrading

**Status:** accepted 2026-09-18
**Decides:** `control.arm.refusal.unknown-trigger`, `control.arm.refusal.external-trigger-without-a-face`

## Context

The trigger adapter decides when the engine looks at the armed set, and the two adapters
differ in durability rather than in behavior. `in-process` advances the schedule while the
process is awake and is not durable. `external` waits on a platform cron, an alarm or a host
crontab calling the wake route, and is durable. Both reach the armed set through one due-ness
function, so the engine decides what is due, the ordering and each entry's next fire; the
adapter decides only when the engine looks.

The selection is not a manifest field. It is the serve command's `--trigger` option or its
environment equivalent, which means it travels as a deployment variable — the class of value
most easily mistyped, most easily inherited from another environment, and least likely to be
reviewed.

The hosts that need `external` are the ones that suspend. On such a host the in-process
adapter's tick does not run while the process is asleep, so schedule advance stops. Nothing
errors: the process is healthy when it is awake, the armed set is intact, and every entry
reports its next fire correctly. A stalled schedule produces no error of its own. The only
symptom is that ingestion stops advancing, which reads exactly like an upstream with no new
rows — and an upstream with no new rows is a normal, common, expected state.

`external` has its own precondition. The wake arrives as a request to a route, so a deployment
serving no HTTP face has nowhere for the heartbeat to land. The platform cron then fires on
schedule, fails to reach a route, and — depending on the platform — reports its own success
while nothing ran.

## Decision

An unrecognized trigger value raises `TriggerAdapterUnknown` at startup rather than
downgrading onto the non-durable adapter. `external` selected on a deployment serving no HTTP
face raises `TriggerFaceMissing` at startup. The selected adapter and its durability print at
startup and read the same on the health surface, so a deployment's durability is answerable
without reading its process arguments.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse both cases at startup, and publish the selected adapter's durability** *(chosen)* | A deployment either runs the durability it asked for or does not run. Durability is readable from the health surface rather than inferred from process arguments. | A typo in a deployment variable stops the process at boot rather than degrading it, so a rollout carrying one bad value fails closed. |
| Downgrade an unknown value to the in-process adapter | The deployment stays up; a wrong value costs nothing visible. | Ingestion stops advancing on a suspending host with nothing erroring. The one symptom is indistinguishable from a quiet upstream, so the failure can persist for as long as nobody independently notices missing data. |
| Accept `external` and open the wake route lazily on first use | Tolerant of startup ordering; a face added later just works. | The first heartbeat meets a missing route while the cron reports success. The deployment appears scheduled and is not, and the discrepancy lives on the platform side where the engine's diagnostics do not reach. |
| Accept `external` with no face and log a warning | Nothing is blocked; the operator is told. | A warning at boot is read once, by whoever was watching the rollout. The stalled schedule that follows has no signal of its own. |
| Infer the adapter from the host rather than taking it as a value | No value to mistype; the right adapter is chosen automatically. | The engine would have to detect suspension behavior it cannot observe from inside, and a wrong inference produces the same silent stall with nobody having made a choice to review. |

## Criteria

1. **Distinguishability of the failure** — whether a wrong trigger looks different from an
   upstream with no new rows. Every degrading option fails this, and it is the reason the
   failure would persist.
2. **Whether the deployment runs the durability it declared** — a selection that silently
   becomes something else makes the declaration meaningless.
3. **Where the symptom appears** — inside the engine's diagnostics, or on the platform's cron
   side. Lazy route opening fails this.
4. **Answerability of durability after the fact** — whether an operator can read what is
   running without reconstructing process arguments.
5. **Rollout tolerance** — whether one bad variable can stop a deployment from starting. This
   is the criterion the chosen option loses on.

Distinguishability decides it. A stalled schedule produces no error of its own, so a
degradation is not a lesser failure than a refusal — it is the same failure with the signal
removed, discovered days later by somebody asking why a table is short. A boot refusal costs a
failed rollout, which is loud, immediate, and attributable to the change that caused it.

## Consequences

A running deployment's trigger is the one that was asked for, and its durability is a published
fact on the health surface rather than an inference. The two silent-stall paths — a mistyped
value and a durable adapter with nowhere to wake — are closed at the only moment they are
cheap to close.

The accepted cost is fail-closed rollout behavior: a deployment variable carrying a typo, or
an `external` selection reaching a profile that serves no face, stops the process at boot. A
rolling deploy that changes both the trigger value and the served surfaces in one step can
therefore fail on the ordering between them, and the remedy is to serve the face first.

Reversing this toward a downgrade is cheap in code and removes the property entirely, so there
is no partial version of it: any deployment permitted to degrade silently is one whose
durability claim cannot be trusted.

## Revisit triggers

- The engine gains a positive liveness signal for schedule advance — a published time since
  the last due-ness evaluation — so a stalled schedule becomes distinguishable from a quiet
  upstream by observation rather than by refusing to boot.
- The wake route becomes available on every profile, which removes the second refusal's cause
  rather than its rule.
- Rollouts are observed failing on the ordering between serving a face and selecting
  `external`, which would price the boot refusal against a deferred check.
