# 0228 — A model-vendor client library linked into a workspace crate is refused on every change

**Status:** accepted 2026-09-18
**Decides:** `enforcement.place.refusal.vendor-client-library`

## Context

Placement policy states where the model consuming a result runs. A zone is declared by the
calling process, a table declares which zones its rows admit, and a cell whose effective set
omits the session's zone arrives null. The whole construction says something about a boundary
the data plane does not cross: authority answers who reads what, placement answers where the
answer is processed.

That claim holds only while the data plane is not itself a caller of a model vendor. A linked
vendor client makes the engine an inference client, and at that moment the policy stops
describing the system and starts describing somebody else's process — the engine can route a
row to a vendor endpoint without any of the zone machinery applying, because the zone machinery
governs what it *serves*, not what it *sends*. An operator reading the zone policy would have
no way to see that second path.

The pressure to link one is real. The synthesis step consumes a model, and a vendor client is
the shortest route to it. The pressure is also recurring — each new vendor, each new
convenience wrapper, each dependency that quietly pulls one in transitively.

That recurrence is what makes a build-time flag insufficient. A flag is set by whoever produces
a build; the placement property is claimed to a reader of the corpus, who does not know which
flags a given deployment was built with. A property asserted to every reader has to hold in
every build.

## Decision

A model-vendor client library linked into any crate raises `EnforceVendorClientLibrary`,
checked on every change. Placement policy selects no model, assembles no prompt and dispatches
no inference call. The one invocation point inside the engine is an operator-configured
endpoint capability the synthesis step uses, so the endpoint is a deployment choice rather than
a linked dependency. What the system owns is the caller's declared zone and the zone the data
admits.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse any linked vendor client; reach a model through an operator-configured endpoint** *(chosen)* | The placement claim is true of every build, and the endpoint a model is reached through is visible in the deployment's own configuration. | A vendor whose protocol that endpoint does not speak is reached by an adapter the operator supplies. |
| Allow a vendor client behind a feature flag | Direct vendor support where it is wanted, absent where it is not. | Loses on what the claim can honestly cover: a flag is set by a build, and the property is asserted to a reader who cannot see the build. |
| Allow one client for the synthesis step | Covers the single legitimate inference call with the least operator work. | Loses on what the placement contract can honestly claim: one linked client makes the placement statement conditional in every build, and the operator-configured endpoint reaches the same model while keeping the path a deployment choice rather than a compiled-in one. |
| State the rule in prose and rely on review | No check to maintain; no false positives on transitive dependencies. | Loses on recurrence: the rule is tested by every dependency change, including transitive ones, and a prose rule is enforced only by whoever happens to look. |

## Criteria

1. **What the placement contract can honestly claim** — whether the property holds in every
   build or only in some. This is the criterion that decided it.
2. **Visibility of the inference path to an operator** — whether the endpoint a model is
   reached through appears in configuration or in a dependency graph.
3. **Durability against a transitive dependency** — whether the rule survives a library that
   pulls a vendor client in without anyone noticing. The prose rule fails this.
4. **Operator effort to reach an arbitrary vendor** — how much work a non-standard protocol
   costs. This is the criterion the chosen option loses on.

What the contract can honestly claim decides it. A zone policy that names where inference runs
is only meaningful while the engine is not a caller of a vendor endpoint; one linked client
makes every statement about placement conditional on facts the reader cannot check. The
feature-flag option produces an identical binary in the common case and an entirely different
claim, which is the worst combination — the property appears to hold and is not stated
anywhere as conditional.

## Consequences

The placement claim is checkable from the dependency graph, on every change, by anyone. The
inference endpoint is a deployment-level configuration, so an operator can see and change where
synthesis calls go without rebuilding, and the audit record carries the zone asserted at the
time of each read rather than a route compiled in months earlier.

The accepted cost: the synthesis step reaches its model through an operator-configured
endpoint, so a vendor whose protocol that endpoint does not speak is reached by an adapter the
operator supplies. That is real work, it sits outside the engine, and it makes an unusual vendor
meaningfully harder to adopt than a linked client would. The check will also fire on a
transitive dependency that pulls a vendor client in for reasons unrelated to inference, which
costs a dependency investigation to clear.

Reversing this is cheap to implement and cannot be done quietly: admitting one client changes
what every statement in the placement contract means, and there is no narrower version of that
admission.

## Revisit triggers

- The endpoint capability is found unable to express a protocol shape a deployment genuinely
  needs, with no adapter that closes the gap.
- The check is observed firing on transitive dependencies that cannot reach a vendor endpoint,
  often enough that contributors route around it.
- A mechanism appears that makes a linked client's outbound calls subject to the same zone
  resolution a served row receives, which would remove the reason for the refusal.
