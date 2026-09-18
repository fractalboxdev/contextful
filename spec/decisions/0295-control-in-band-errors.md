# 0295 — A step over the tool protocol judges success on the tool result

**Status:** accepted 2026-09-18
**Decides:** `control.dispatch.refusal.in-band-tool-error`

## Context

A run decomposes into checkpointed steps inside the orchestrator, and some of those steps
drive the engine over the tool protocol rather than calling into it directly. The protocol
carries its errors in band: a request that failed answers `200` with a body containing a
protocol error object, or a body whose result is flagged as an error. Transport status and
operation outcome are two different facts, and the protocol deliberately keeps the second
out of the first so that a proxy or a router cannot rewrite an application outcome.

A checkpointed step's success is durable. The orchestrator journals the step's result and
never re-runs it, so a step that recorded success on a call that actually failed has
written a permanent claim about work that did not happen. The run reaches its terminal
status as a success, its record says the stage completed, and nothing in the control plane
holds a contradicting fact.

Where that surfaces is the problem. A commit that did not happen has no immediate symptom:
there is simply no new data. The store is internally consistent, the catalog is valid, the
snapshot set is coherent — it is just missing rows. The discovery event is a query, later,
returning less than someone expected, at which point the run record says the work
succeeded and the investigation starts from a false premise.

The second question is what to do with an in-band error once it is read. These errors are
not one kind. A transient upstream failure and a permanent protocol error — a tool that
does not exist, arguments the tool rejects — are both `200` bodies, and retrying the second
on the step's schedule burns the whole retry budget on a call that will never succeed.

## Decision

A step driving the engine over the tool protocol judges success on the tool result rather
than on transport status. The step parses the response body, and a `200` carrying a
protocol error, or a result flagged `isError`, raises `StepToolError` naming the error the
tool returned. A step does not record success on a transport status alone, and every step
driving the engine over this protocol carries the parse rather than treating it as optional
hardening on the paths thought likely to fail.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Parse the result envelope and judge on it** *(chosen)* | A failed operation fails its step at the instant it happens, while the run is live and the failure is attributable to a stage. | Every step driving the engine parses a result envelope rather than checking a status code, and a new result shape has to be taught to that parse or it reads as an unrecognized body. |
| Trust the transport status | Trivial, uniform across every step, and correct for protocols that map outcome onto status. | Lost on where the failure surfaces: a protocol error inside a success response reads as success, the run journals work it never did, and the contradiction appears at query time with the run record asserting the opposite. |
| Retry every in-band error uniformly | One rule, no classification, and transient failures recover without operator action. | Lost on budget: a permanent tool error — an unknown tool, rejected arguments — burns the step's whole retry schedule and delays the failure by the length of that schedule without changing it. |
| Parse only on the steps considered failure-prone | Less code, and the common paths are covered. | Lost on coverage: the step that silently succeeds is by definition the one nobody predicted, and a partial rule gives the same false confidence as no rule on the paths it omits. |

## Criteria

1. **Where the failure surfaces** — at the step, or at a query hours later. **This
   criterion decided.** A failed commit read as a green step is not merely a delayed error;
   it inverts the evidence, because the run record then asserts that the work happened.
   Every other consideration here is about cost of implementation, and none of them price
   comparably against an audit trail that is wrong.
2. **Durability of the wrong answer** — a checkpointed step's success is journaled and not
   re-run, so the mistake persists rather than being corrected on a retry.
3. **Retry budget spent on permanent errors** — whether a classification exists before the
   retry schedule is committed.
4. **Coupling to the protocol's result shape** — the chosen option is the most coupled and
   pays for it below.

## Consequences

A run's record means what it says: a step that reports success drove an operation that
reported success. The failure arrives attributed to a stage, with the tool's own error
text, while the run is still live and the surrounding context is available. Because the
parse is on every step rather than the suspicious ones, coverage does not depend on
predicting which call will fail.

The cost accepted: the step layer is coupled to the protocol's result shape. A new result
form — a different error flag, a wrapped envelope, a streamed result whose error arrives
late — has to be taught to the parse, and until it is, the step reads an unrecognized body
and has to decide between failing on something it does not understand and falling back to
the status it was built to distrust. That is a real maintenance obligation on every version
change of the protocol, and it lands on every step rather than in one adapter unless the
parse is factored into one.

## Revisit triggers

- The tool protocol gains a transport-level mapping for operation outcome, making status a
  faithful signal and the parse redundant.
- An unrecognized result shape is encountered in production, showing the parse's coupling
  cost is being paid in incidents rather than in releases.
- Permanent and transient in-band errors are distinguished well enough by their payloads
  that a classification-driven retry policy becomes worth stating separately.
