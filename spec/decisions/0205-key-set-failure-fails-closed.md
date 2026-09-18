# 0205 — A checkpoint that opted into a published key set and cannot obtain one refuses every credential

**Status:** accepted 2026-09-18
**Decides:** `authority.verify.refusal.key-set-unavailable`

## Context

A verifying checkpoint holds public key material and nothing else. It reaches that
material one of two ways: static pins written into its configuration, which cover a
rotation performed by hand, or a project-published key route that returns the current
version and every non-retired one, which covers a rotation performed on a cadence. The
published route is the shape that makes a 90 d rotation invisible to the deployments
consuming it, and opting into it is the operator's declaration that the route is where
the truth about issuer keys lives.

The route is a network dependency on the admission path, and the refresh interval and the
staleness bound exist to keep it a shallow one. A set is refreshed on a 300 s lifetime
with one refresh-and-retry when a signature fails, and a fetch failure serves the
last-known-good set while its total age stays under 1 h. Beyond that age the set is no
longer evidence about which versions are live: a key retired during the outage keeps
verifying credentials minted under it.

A checkpoint can hold static pins and a route at once. That makes the failure case a
question rather than an accident, because there is a set of keys sitting locally that the
checkpoint could verify against instead of refusing. Those pins are, by construction, the
keys someone wrote down at deployment time — the frozen state a scheduled rotation exists
to leave behind.

The failure also has to answer consistently across hops. A gateway and the engine verify
the same credential; if one falls back to pins while the other refuses, admission depends
on which hop the request reached first, and the same credential is admitted or refused
depending on topology.

## Decision

A checkpoint that opted into a published key set and fails to obtain one refuses every
credential and raises `KeySetUnavailable`. The engine declines to start on that
condition; a gateway answers `503`. A static pin configured alongside the route rescues
neither, and a pin that does not parse answers the same way rather than being skipped.
Opting in is the whole of the declaration: from that point the route is the source of
truth about which key versions verify, and its absence is the absence of an answer rather
than an invitation to use an older one.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse every credential, `503` at a gateway and a refusal to start at the engine** *(chosen)* | One answer at every hop; a route outage is visible immediately rather than as silently frozen key material | Key-route availability becomes admission availability for every deployment that opted in |
| Fall back onto the static pins | Admission survives a route outage with no operator action | Lost on silent restoration of frozen key material: the deployment verifies against keys written down at deployment time — the exact state the route exists to leave — with nothing in its behavior saying so |
| Serve the last-known-good set with no age bound | Rides out an outage of any length | Lost on the frozen-key criterion with no bound at all: a retired version keeps verifying for as long as the route stays unreachable, so retirement stops being a withdrawal instrument |
| Answer an authentication error rather than a server error | A caller sees a familiar credential failure | Lost on whether the error describes what happened: no credential the caller could present would succeed, and a client retries a mint it does not need |

## Criteria

1. **Whether a failure can silently restore frozen key material.** *(decided it)* Every
   other criterion trades availability against a bounded window of wrong verification.
   This one is unbounded: a static-pin rescue leaves the deployment verifying against a
   pinned set indefinitely, with nothing in its behavior distinguishing that state from a
   healthy one. A rotation instrument that a single unnoticed failure disables is not an
   instrument.
2. **Whether admission depends on which hop answered first.** A fallback each hop takes
   independently makes the verdict a function of topology.
3. **Whether the error a caller receives describes what happened.** A caller can act on
   "this deployment cannot verify anything right now"; it cannot act on a credential
   error that no new credential fixes.
4. **Availability of the admission path.** Weighed and conceded, bounded by the 1 h
   staleness window, which covers a short outage without any fallback at all.

## Consequences

Rotation becomes safe to automate, since a checkpoint's view of live key versions cannot
diverge from the project's without the divergence being loud. Debugging an admission
outage gets easier: the condition names itself at start-up rather than surfacing as
credentials that verify against keys nobody expected.

The cost accepted is that key-route availability becomes admission availability for
deployments that opt in. A route outage longer than the staleness window stops reads, and
the operator's recovery is to restore the route or to reconfigure onto static pins
deliberately. A malformed static pin takes the face down rather than degrading to the
other key material present, which turns a configuration typo into an outage.

Reverting to a fallback later is cheap in code and expensive in meaning: any deployment
that ran under the fallback has an unaudited window in which its live key set is unknown.

## Revisit triggers

- Measured key-route availability over a quarter sits below the availability the read
  path itself achieves, making the route the weakest element of the path.
- A deployment shape appears where the key route is reachable only from the mint side and
  a checkpoint cannot reach it at all, so opting in stops being a live choice.
- The staleness bound is observed to expire during ordinary maintenance rather than only
  during incidents, indicating the 1 h window is set against the wrong distribution.
