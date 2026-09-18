# 0274 — A default configuration opens no outbound connection

**Status:** accepted 2026-09-18
**Decides:** `accountability.record.refusal.default-egress`

## Context

The engine runs inside deployments whose whole reason for choosing a local-first store is that
the material stays where they put it. Some of those deployments are air-gapped. Others are not,
but are audited by people whose method is to watch what the process talks to.

That method is the constraint. An auditor evaluating this engine does not read its source, and
does not take its documentation as evidence; they run it and look at the network. What that
observation can establish is narrow and absolute: either the default configuration produced
outbound traffic or it did not. Any claim about the content of that traffic — that a usage
counter is anonymous, that a license check carries no identifiers, that an update probe
transmits nothing about the deployment — is a claim the auditor cannot check from the trace.
They see a connection to a host, and they either trust the assertion about its payload or they
do not.

That asymmetry is why the polarity of an update check matters more than its payload. An
opt-out probe and an opt-in probe can be byte-identical on the wire. What differs is what the
default does, and the default is what gets audited, because it is what runs on a machine nobody
configured. A deployment that never opened the configuration file is the case the auditor is
trying to reason about.

Usage telemetry has the same shape and a stronger pull, because the absence of it is a real
cost to the people building the system: no visibility into version spread, into which features
are used, into field error rates. Every one of those is information that improves the product,
and every one of them is unavailable from a default that makes no call.

The separation already drawn between the two record surfaces settles the local half of this.
Telemetry is a projection an operator queries, and its export endpoint resolves unset, sending
spans to a no-op sink until someone points them at a collector. Nothing in the engine's own
operation depends on reaching anything.

## Decision

A default configuration issues no usage ping, no license-server contact and no auto-update
probe. A build that introduces an outbound call reachable without operator configuration raises
`UnconfiguredEgress`. An update check is opt-in through `[update] check = true`, fetches a
release manifest, and transmits nothing about the deployment. The span export endpoint resolves
unset, and spans go to a no-op sink until an operator points them at a collector.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **No outbound call in the default configuration** *(chosen)* | The binary works fully air-gapped with no configuration, and the absence of traffic is provable from a network trace without trusting any claim about payloads. | No visibility into version spread, feature use or field error rates; operators discover available updates by checking themselves. |
| Anonymous aggregate usage counters | Field data on versions, features and errors, at what is genuinely a small privacy cost. | Lost on provability: "anonymous" is a property of the payload, and an auditor reading a trace sees a connection, not an anonymity guarantee. The claim has to be trusted, which is what the default is supposed to remove the need for. |
| An opt-out update probe | Deployments learn about security releases without anyone opting in — the outcome most likely to matter. | Lost on the same criterion: the default is what gets audited, and an opt-out probe makes the unconfigured case the one that calls out. |
| A first-run prompt choosing the posture | Informed consent rather than a silent default; the operator decides knowingly. | Lost on unattended deployment: an engine started by an orchestrator answers the prompt by default anyway, so the posture question returns unchanged. |
| An opt-in check that reports the running version to size the response | A precise "you are behind" answer rather than a manifest the client compares against. | Lost on payload trust even inside the opt-in path: fetching a manifest and comparing locally transmits nothing, and the precision gained does not justify reintroducing a claim about payload contents. |

## Criteria

1. **Auditability of the default** — whether the absence of outbound traffic is provable from a
   network trace, with no claim about payload contents taken on trust. **This criterion
   decided.** It outranks the value of field data because the deployments this engine is for
   are chosen on exactly this property; a default that requires trusting an assertion about
   what a connection carries gives up the thing being sold in order to improve the thing being
   built.
2. **Air-gapped operability with no configuration** — whether an unconfigured binary is fully
   functional with no network at all.
3. **Value of field data** — version spread, feature use, error rates. The chosen option
   forgoes all of it.
4. **Security-update reach** — how many deployments learn of a release. The chosen option is
   the worst here and the cost is real.
5. **Behavior under unattended start** — whether a posture decision survives an orchestrator
   launching the process.

## Consequences

An auditor's verification is one observation with no interpretation attached: run it, watch the
interface, see nothing. That holds for the whole default configuration rather than for a list
of features someone has to enumerate, which is why the rule is stated as a property of the
build — a call reachable without operator configuration is a defect, whatever subsystem
introduces it.

The cost accepted: the people building this system have no field visibility. Version spread is
unknown, so it is not knowable how many deployments run a release with a known fault. Feature
use is unknown, so deprecation decisions rest on conversations rather than counts. Field error
rates are unknown, so a failure mode that is common in deployment and absent in testing is
discovered when someone reports it. The security consequence is the sharpest: a deployment that
never enables the update check does not learn a release exists, and the response to that is
documentation and operator process rather than a mechanism. No estimate exists for how many
deployments would enable an opt-in check.

Reversing this is a one-line change in a build and an unbounded change in what the engine can
claim, since an auditor's past observation no longer describes the current binary.

## Revisit triggers

- A payload-provable reporting mechanism becomes available — one where a trace alone
  establishes what was sent, without trusting a claim about contents.
- Deployments are observed running versions with known security faults long after a release,
  making update reach the dominant cost rather than a stated one.
- A deployment profile appears where the operator's own environment already mediates and logs
  all egress, making the default's provability redundant.
