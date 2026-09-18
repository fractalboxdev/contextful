# 0076 — Re-resolving a token returns the written payload, and a conflicting payload is refused

**Status:** accepted 2026-09-18
**Decides:** `run.suspend.refusal.conflicting-resolution`

## Context

An awakeable suspends a run durably. The engine mints an opaque single-use token, persists a
pending row beside the journal, and an external party resumes the run by posting that token
back with a payload. The resume payload is written as the awaited step's output under the
deterministic key derived from the token, which places it inside the journal's exactly-once
contract: the first write under a key is the value, and every later read under that key
returns what was written.

The external party is not under the engine's control and does not share its delivery
guarantees. A browser callback fires twice because the user refreshed. A vendor's webhook
retries because it did not see the acknowledgement. A tool posts its result, times out
reading the response, and posts again. Most of those repeats carry the identical payload;
some do not, because the external side recomputed something between attempts.

The consequence lives in replay. A run that resumes and then crashes reads the awakeable back
as a recorded step output and receives the payload without suspending again. If the recorded
value can change after it was first read, a resumed run and the original run observe different
values under one key, and every step downstream of the suspension replays against a different
input than it originally ran against. That is precisely the divergence the journal exists to
prevent, arriving through the one door that opens outward.

## Decision

Resolving a token a second time with the identical payload returns the value already written.
A second resolution carrying a different payload raises `AwakeableAlreadyResolved` and leaves
the written value untouched. The refusal answers `409` on the resume route. The recorded value
is single-valued for the whole life of the run under every resolution attempt, so one key
never yields two observations.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Return the written value; refuse a conflicting one** *(chosen)* | One key, one value, forever. Retry-safe for the honest duplicate and loud for the dishonest one. | An external party that legitimately needs to correct a payload has no path short of a new suspension. |
| Last write wins | The external party can always correct itself, and no error arm exists. | Loses outright on single-valuedness. A replayed run reads a payload the first resumption never saw, and every step after the suspension replays against different input. |
| Ignore a conflicting payload, answer success | The run stays correct and the caller sees no error. | Loses on the caller's side. The external party believes its corrected value landed and acts on that belief; the disagreement is invisible to both ends. |
| Refuse every second resolution, identical or not | The simplest rule, with no payload comparison at all. | Loses on retry safety. An ordinary at-least-once delivery turns into an error the external party must distinguish from a real failure, which is the failure mode webhooks produce most often. |

## Criteria

1. **Whether a resumed run can observe two different values under one key.** The journal's
   determinism boundary, applied at the one point external data enters.
2. **Retry safety for an at-least-once caller.** Whether an ordinary duplicate delivery is
   harmless.
3. **Whether disagreement is visible to the party that caused it.** Whether the external side
   learns its value did not take.
4. **Availability of a correction path.** Whether a genuinely wrong payload can be fixed.

Criterion 1 decides it. Single-valuedness is what makes a resumed run identical to an
uninterrupted one, and it is the property every recorded step already holds — an awakeable that
did not hold it would be the one recorded value that replays differently. Criteria 2 and 3
select the shape of the answer within that constraint; criterion 4 is what is given up.

## Consequences

The resume route becomes safe to retry: an identical re-post answers `200` with the written
payload, so a vendor's delivery guarantees compose with the engine's without coordination. A
conflicting post answers `409`, which names the disagreement to the side holding the wrong
belief.

The accepted cost is that there is no correction path. An external party that posted a wrong
payload and noticed cannot replace it; the run proceeds on the first value, and fixing it means
failing that run and firing again with a fresh suspension. That is expensive when the
suspension sits behind a human approval gate, since the human approves twice.

Payload comparison is over the written bytes. A payload that is semantically identical but
serialized differently — key order, whitespace, a numeric formatting difference — reads as a
conflict and is refused. An external party that re-serializes between retries sees `409` where
it meant to be idempotent.

## Revisit triggers

- A caller class appears whose retries legitimately re-serialize, making byte comparison the
  wrong equality for this check.
- An approval-gate correction path is needed often enough that a supersede-with-authorization
  route costs less than the repeated gate.
- The journal gains a general mechanism for revising a recorded value under an explicit
  authority, which would give this refusal a principled escape.
