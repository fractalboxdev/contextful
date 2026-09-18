# 0294 — The dispatchable unit is a run that starts from the store alone

**Status:** accepted 2026-09-18
**Decides:** `control.dispatch.refusal.step-is-not-a-head`

## Context

A reconciler beat ends by dispatching what is due. The question is what one dispatch
starts: an orchestrator instance per due entry, or one instance per beat that walks the due
set in order.

The armed set carries no edge between two entries. There is no success-triggers-start
orchestration in it, and cross-entry ordering is expressed through the model graph rather
than through the schedule. The stage order within a tick — validate, rebuild-catalog,
acl-sweep, pipeline runs, compact, build, sync-push — is a same-tick tiebreak rather than a
barrier: nothing not due is pulled forward and nothing waits on anything.

That makes a per-beat chain a poor fit for what the schedule actually says. Every entry in
the chain shares one cadence, since the chain starts when the beat does, and shares one
failure, since an orchestrator instance that fails takes the remainder with it. Two entries
against unrelated upstreams, authored with different cadences, become one unit whose blast
radius is the union of both.

The opposite decomposition has a different problem. Where a deployment's entries really are
landing steps of one dependent run — a pull that stages rows, a transform that reads them,
a publish that reads the transform — dispatching each step independently requires a trigger
that says "the step before me staged its output". The schedule grammar carries two forms,
an interval and a five-field cron, and neither expresses that. Dispatching a step on a
cadence guesses at its predecessor's completion, and a misread dispatches a second instance
of work already running, which is a silent double run rather than an error.

## Decision

The reconciler starts one durable orchestrator instance per due dispatchable unit. A unit
is dispatchable when it starts from the store alone — when nothing outside the store has to
have happened first for it to be correct to run. Where a deployment's entries are landing
steps of one dependent run, the run is the unit and cadence rides the entry at its head; a
due id that is a step raises `DispatchUnitNotAHead` and is reported rather than started.
Two dispatchable units share neither a cadence nor a failure, and work deferred by the fire
pool's bound or by an exclusion key stays due and is reconsidered on the next tick.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **One instance per due dispatchable unit, with steps refused as units** *(chosen)* | Independent entries fail independently and hold their own cadences; a dependent run keeps its ordering without needing a trigger the grammar cannot express. | Entries authored as a chain run as one unit even where they are genuinely independent, and the head entry's cadence governs the whole run. |
| One chain per beat | One orchestrator instance to reason about per beat; ordering is free; concurrency control is trivial. | Lost on blast radius: every entry shares one cadence and one failure, so an unrelated upstream's outage stops entries that have nothing to do with it. |
| Decomposing a chain so each step dispatches independently | Maximum independence; every step retries and scales on its own. | Lost on expressiveness: it needs an upstream-staged-its-output trigger the schedule grammar does not carry, so each step would fire on a cadence that guesses at its predecessor. |
| Dispatching steps independently with a cadence chosen to outrun the predecessor | Available today with no grammar change. | Lost on silent double runs: a misread fires a second instance of work already in flight, which produces duplicate landings rather than a refusal, and the padding is a number with no principled value. |

## Criteria

1. **Whether the schedule grammar expresses the trigger the option needs** — the grammar
   has an interval form and a cron form and nothing else. **This criterion decided.** An
   option requiring a trigger that does not exist is not a trade against the others; it is
   unavailable until the grammar grows, and the ordering it would replace is already
   expressible through the model graph.
2. **Blast radius of a single failure** — how many unrelated entries one failing upstream
   or one crashed instance takes with it.
3. **Whether a misread produces a silent double run** — a duplicate landing is worse than a
   refusal, because it is discovered downstream rather than at dispatch.
4. **Cadence fidelity** — whether each entry fires on the cadence it was authored with. The
   chosen option gives this up inside a chain.

## Consequences

Entries against unrelated upstreams are genuinely independent: one vendor's outage, one
malformed page, one crashed instance is confined to its own unit, and the rest of the due
set proceeds. The per-source exclusion key does its job, since two fires of one entry
cannot overlap and a vendor's rate limiter sees a single caller. The refusal on a
non-head id turns the most likely authoring mistake — putting a cadence on the middle of a
chain — into a report rather than a run that double-lands.

The cost accepted: a chain is one unit even when its steps are independent. An operator who
authored three entries as a dependent run, and later made them independent in fact, still
gets one instance with one cadence and one failure until they re-author the relationship.
The head entry's cadence governs the whole run, so a slow tail step effectively lengthens
the head's interval, and there is no way to give the tail its own cadence short of
splitting the chain. Discovering which entries are heads requires reading the run's shape
rather than the schedule, which is one more thing an operator has to hold.

## Revisit triggers

- The schedule grammar gains a trigger expressing that an upstream staged its output, at
  which point independent step dispatch becomes available and this comparison reopens.
- Chains are observed where the head's cadence is materially wrong for the tail, often
  enough that per-step cadence is the common request rather than the rare one.
- The fire pool's bound is regularly the binding constraint on a tick, indicating that
  per-unit dispatch has produced more concurrency than the deployment can host.
