# 0290 — A control URL is polled on loopback and refused anywhere else

**Status:** accepted 2026-09-18
**Decides:** `control.reconcile.refusal.control-url-is-loopback`

## Context

A control source is subscribed to by a `[control]` block naming either a directory of
snapshot objects or a `url` to poll. Whoever answers that poll writes the deployment's
entire pipeline schedule set: the entries that exist, their cadences, the connectors they
drive, and the destinations their output lands in. The reconciler owns the whole set, so an
id the answering party omits leaves the armed set and an id it adds arms. There is no
narrower blast radius available — schedule integrity is exactly transport integrity.

The poll protocol makes an unauthenticated transport worse than the usual read. A beat
reads a pointer whose body is a version integer and re-parses the named snapshot only when
that integer is strictly greater than the armed version. An attacker who can answer the
poll therefore does not need to forge a document to do damage: answering with a lower
integer pins the deployment on an old snapshot forever, and answering with the same integer
freezes it. Both are silent, because an unchanged pointer is the normal steady state and
costs one small read.

The damage available through a forged snapshot is not limited to cadence. A pipeline
declares its destination, so a snapshot that repoints one entry's output and adds a
`sync-push` cadence beside it turns a read-only compromise of the poll path into
exfiltration of the store, on a schedule, through the deployment's own credentials.

Transport authentication alone does not close this. A bearer-authenticated read over TLS
proves the deployment is talking to the server it expected, which leaves the version
integer forgeable by that server and by anything that can replay to it. Rollback and pinned
staleness need no key at all under that model.

## Decision

A control `url` whose host is a loopback address is polled. Any other host raises
`ControlSourceNotLoopback` at arming rather than being polled. The check binds to the
connection rather than to the string: the client follows no redirect, ignores proxy
environment variables, and pins `localhost` onto the loopback addresses itself, so a name
that merely resolves to a loopback address through a resolver or a hosts file does not
satisfy the test. A control transport reaching off the loopback becomes legal once it
carries all three of a bearer-authenticated read, TLS, and producer-side signing over
`(version, content-hash)` verified before arming.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Loopback only, with a stated three-part requirement for anything further** *(chosen)* | The high-authority path is confined to a boundary an attacker has to already be inside to reach, and the conditions for widening it are written down rather than negotiated per deployment. | A control plane on another host is unreachable until snapshots are signed at produce time, so multi-host control is blocked on work that is not yet built. |
| A remote URL over TLS with a bearer read | Cross-host control today; authenticates the server; encrypts the document in flight. | Lost on forgeability of the version integer: the server, and anything that can compel or replay it, pins or rolls back the whole schedule set with no key. Transport authentication does not authenticate the content. |
| An unauthenticated remote URL | Trivial to stand up; a plain object-store URL works. | Lost on the same criterion at its extreme: any party answering the poll injects entries, including a destination repoint beside a push cadence that exfiltrates the store. |
| Signature verification with no host restriction | Content authenticity without a network boundary, which is the eventual end state. | Lost on timing: the produce-time signing path and the key distribution it implies do not exist yet, and shipping the host relaxation first opens the surface during the gap. |

## Criteria

1. **Forgeability of the version integer** — whether a party on the transport can pin,
   roll back or replace the schedule set. **This criterion decided.** The pointer is read
   on every beat and the snapshot only on advance, so the integer alone controls what the
   deployment believes; authentication of the endpoint leaves it entirely unprotected.
2. **Blast radius of a compromise** — one poll answer rewrites every entry, its cadence and
   its destination, which puts this path at the same level as the credential store.
3. **Silence of the attack** — rollback and pinning produce no error and no diagnostic; the
   deployment reports a healthy reconcile against a stale version.
4. **Deployment reach** — how many topologies the control plane can serve. The chosen
   option is the most restrictive and pays for it below.

## Consequences

The control plane and its subscribers sit on one machine, so the trust boundary for
schedule authority coincides with the host boundary rather than the network. A snapshot
directory on local disk and a loopback server are equivalent in authority, which makes the
two configured forms genuinely interchangeable. The signing requirement is stated as a
contract rather than left implicit, so the remote path is a known piece of work rather than
a judgement call made deployment by deployment.

The cost accepted: a deployment cannot take its schedules from a control plane running
anywhere else. A fleet of daemons across several hosts each need their own local control
source or their own applied document, and the natural topology — one control plane, many
subscribers — is unavailable. That is a real product limitation, not a theoretical one, and
it lasts until produce-time signing and verification before arming are built.

Reversing the restriction later is cheap; reversing it without the signing is the thing this
decision exists to prevent, so the refusal names the host rather than the missing signature.

## Revisit triggers

- Producer-side signing over `(version, content-hash)` and verification before arming both
  exist and are pinned, at which point the host check is replaced by the signature check.
- A deployment topology requires subscribers on hosts other than the control plane's, and
  running a local control source per host is demonstrably not workable.
- The pointer protocol changes so that the version is no longer the sole trigger for
  re-parsing, removing the rollback and pinning exposures that transport authentication
  leaves open.
