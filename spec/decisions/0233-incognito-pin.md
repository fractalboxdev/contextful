# 0233 — Incognito pins the session, and a wider asserted zone is refused rather than honored

**Status:** accepted 2026-09-18
**Decides:** `enforcement.place.refusal.widening-under-incognito`

## Context

Incognito is a single control with three spellings — a toggle in the desktop client, a
command-line flag, a request field — and one promise: nothing leaves the machine for the
duration of the session. It resolves the session to the fail-closed pair, a local device
and an on-premises environment, so a row admitting neither leaves the result.

An inference zone, by contrast, is asserted per request by the calling process. A library,
a tool call, an agent step or a downstream service can each set it, and the assertion is a
claim the engine records rather than an identity it verifies. Within one session, several
components may assert zones without any of them being aware of the toggle the user set.

That is what makes the interaction between the two a question. If incognito were an initial
value for the session's zone, any later assertion would replace it, and the component doing
the replacing is exactly the kind of component a user cannot audit: a dependency, a plugin,
a tool the agent selected. The user would see a toggle that is on and a session that sent
rows to a vendor model.

Incognito also reaches a reader nothing else reaches. The uncredentialed local owner's
reads are otherwise unrestricted — no grant narrows them, no predicate applies — and
incognito applies zone resolution to them anyway. For that reader, the toggle is the only
placement control in force.

## Decision

Incognito pins the session. A session asserting a zone wider than the pin raises
`EnforceIncognitoWidening` rather than adopting the asserted zone. The pin binds every
request in the session, the uncredentialed local owner's reads included, and it is cleared
by ending the session rather than by any field a request carries.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A pin no request can exceed, with a wider assertion refused** *(chosen)* | The toggle means one thing for the whole session, and no component inside the session can remove the guarantee the user set. | A session that legitimately needs a cloud model mid-task restarts without the pin; there is no in-session escape by design. |
| Treat incognito as an initial zone value a request may replace | Composes with the ordinary per-request zone mechanism, no special case in resolution. | Loses on what the toggle promises: the widening is silent and is performed by a library or a tool call the user never sees, so the control gives a guarantee that something else removes. |
| Honor the wider assertion and record it in the audit entry | Nothing is refused, and the widening is discoverable afterwards. | Loses on the same criterion at a different moment — the audit tells the user after the rows left, and the toggle exists to stop the crossing rather than to document it. |
| Scope the pin to credentialed reads alone | Simpler resolution; the local owner's unrestricted path stays uniformly unrestricted. | Loses on reach: the uncredentialed local owner is precisely the reader with no other placement control, so exempting them removes the toggle's only effect for the reader who most relies on it. |
| Pin the session but downgrade a wider assertion silently to the pin | No refusal surfaces to a caller that did nothing wrong. | Loses on diagnosability: a component asserting a public-cloud zone under a pin is misconfigured, and a silent downgrade leaves it misconfigured while appearing to work. |

## Criteria

1. **What the toggle promises its user** — whether the guarantee the control states holds
   for the whole session, against every component inside it. *This criterion decides.* A
   privacy control whose guarantee a dependency can remove is worse than no control, since
   the user acts on a belief the system does not hold; every option that permits in-session
   widening fails here whatever it buys elsewhere.
2. **Reach over the readers who have no other control** — whether the uncredentialed local
   owner is covered.
3. **Diagnosability** — whether a component asserting an impossible zone learns that it did.
4. **Task continuity** — whether work in flight survives the pin, which is the criterion
   the chosen option loses on.

## Consequences

A user who turns incognito on holds a guarantee that survives every library, tool call and
agent step in the session, and the same guarantee applies to local reads that no grant
would otherwise touch.

The cost accepted is a hard boundary mid-task. A session that reaches a step genuinely
needing a cloud model does not negotiate: it fails, and the user restarts without the pin,
losing whatever session state was accumulated. There is deliberately no partial release.

A team-wide incognito default compounds this. An administrator can set the pin for
everyone, and an individual turns it off per session only where their capability grants
that — so a user without the grant meets the boundary with no path around it other than an
administrative change.

The refusal also surfaces to callers that are merely unaware rather than malicious, which
means well-behaved components that always assert their zone will fail under incognito until
they learn to read the pin.

Reversing toward a default-with-override is expensive because the guarantee is what the
control is for; a version of incognito that can be widened in-session is a different
feature wearing the same name.

## Revisit triggers

- The refusal is observed firing mostly on components that assert a zone unconditionally
  rather than on genuine widening attempts, which would argue for a way to assert "whatever
  the session allows".
- Mid-task restarts become common enough to measure, which would make task continuity worth
  weighing against a scoped, explicit in-session release.
- A session-scoped state mechanism appears that survives a pin change, removing the loss
  that makes the restart expensive.
