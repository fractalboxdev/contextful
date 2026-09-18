# 0031 — A live probe inside the configured prefix decides whether a backend performs atomic conditional writes

**Status:** accepted 2026-09-18
**Decides:** `sync.probe.refusal.inconclusive-outcome`, `sync.probe.refusal.declared-cas-undemonstrated`

## Context

Two machines writing one bucket rest on one primitive: a conditional write that either
lands or loses, atomically, against the state the writer read. The bucket lease is taken
with a conditional create, renewed with a conditional replace against the entity tag just
read, and the index commits by compare-and-set over a merge of the remote. All three are
the same mechanism, and none of them is safe if the endpoint accepts the precondition and
does not honour it.

An endpoint that ignores a precondition does not fail. It answers success. Both writers'
conditional creates return 200, both believe they hold the lease, and both advance the
same cursor. The damage appears later as skipped records, and the evidence that the
precondition was ignored is gone by then.

"S3-compatible" describes a dialect, not a guarantee. The deployments this runs against
include managed object stores, self-hosted gateways, and compatibility layers in front of
other storage entirely, each with its own version and its own configuration. Whether the
endpoint in front of a given deployment honours a conditional write is a property of that
endpoint on that day, not of the product name in its documentation.

There is one way to tell: write a sentinel, issue writes that ought to lose, read the
bytes back, and compare them against what the winning write put there. A backend that
answers success while ignoring the precondition is caught by the read-back and by nothing
else. The credential matters too — the probe has to exercise the credential a push uses,
against the prefix a push writes to, or it measures something other than the live path.

## Decision

Whether a backend performs an atomic conditional write is decided by a live probe against
the configured endpoint, and by no configuration table, product name or client dialect.
The probe writes its sentinel inside the deployment's prefix, issues a second conditional
create and a tag-mismatched conditional replace, reads the bytes back, and deletes the
sentinel. An unsupported-method response, a forbidden response from a read-only
credential, and a transport error each raise `SyncProbeInconclusive`, and every one of them
reads as capability not demonstrated. Naming `cas` against a backend whose probe did not
demonstrate the capability raises `SyncCoordinationUnproven` and the push stops rather
than degrading to an unserialized write.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Measure the live endpoint with a sentinel and read the bytes back** *(chosen)* | The answer is about the endpoint this deployment talks to, through the credential it uses, at the prefix it writes. A backend that lies by answering success is caught. | Every deployment not declaring single-writer coordination pays one probe round — writes, a read and a delete — at startup, against a prefix its credential can write. |
| A static compatibility table keyed by backend name | Zero startup cost, no write permission needed, and the answer is auditable in the repository. | Lost on relevance. It is a claim about a product rather than about an endpoint, and self-hosted builds, gateway versions and configuration flags all vary underneath one name. |
| Trust the client dialect the endpoint speaks | Free, and already known from configuration. | Lost on the same criterion. The dialect is what the client speaks, not what the endpoint honours; a compatibility layer speaks the dialect precisely while implementing none of its guarantees. |
| Fail the command when the probe is inconclusive | No deployment ever runs with an unmeasured capability. | Lost on safety direction. An unsupported method, a read-only credential and a transport error all already read as not demonstrated, which routes the deployment onto the per-machine lease — the safe reading. Failing instead denies service for a condition whose correct handling is already conservative. |
| Degrade a declared `cas` to single-writer when the probe fails | A misconfigured deployment keeps running. | Lost on failure shape. The operator declared the stronger mode; quietly serving the weaker one means two machines coordinate by a lease neither can see the other holding. |

## Criteria

1. **Failure mode of being wrong** — what happens when the answer about the endpoint is
   incorrect. *This criterion decided it.* A wrong "capable" grants one lease to two
   machines and loses records with no error anywhere; a wrong "not capable" costs a
   deployment its multi-writer coordination and is visible in the reported mode. The two
   errors are not symmetric, so the mechanism that can detect the dangerous one wins even
   at a startup cost.
2. **Relevance of the evidence** — whether the measurement is of this endpoint, this
   credential and this prefix, or of something standing in for them.
3. **Startup cost** — round trips and permissions the answer requires.
4. **Direction of the safe default** — where an inconclusive answer lands. Conservative
   resolution is what lets the probe be lenient about its own failures.

## Consequences

The resolved coordination mode is a measured fact, and every push line prints it alongside
the object counts, so the mode a deployment is actually running under is visible without
inspecting configuration. Diagnostics report the outcome and whether the setting or the
measurement decided, which makes a surprising mode a one-command question.

The costs are two. Every deployment that does not declare single-writer coordination pays
one probe round at startup, requiring write permission on a path inside its own prefix.
And a capable backend reached with a read-only credential returns an inconclusive probe
and silently coordinates by the per-machine lease instead of the bucket lease — a correct
and conservative outcome that reads, from the operator's side, like a capability that is
missing rather than a permission that is.

Reversing toward a configuration table is cheap mechanically and forfeits the one check
that catches a backend answering success while ignoring the precondition.

## Revisit triggers

- Probe startup cost becomes material for a deployment shape with many short-lived
  processes, where one probe per process dominates.
- A backend exposes a capability declaration the endpoint itself signs, making a claim
  about that endpoint rather than about a product.
- Read-only-credential deployments are observed often enough that the inconclusive outcome
  needs to distinguish a missing permission from a missing capability, since the operator
  action differs.
