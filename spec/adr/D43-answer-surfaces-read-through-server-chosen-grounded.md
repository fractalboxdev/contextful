# D43 — Answer surfaces read through server-chosen, grounded calls

**Status:** accepted

## Context

An answer surface faces an adversarial request body, content that can carry injected instructions, and readers who repeat what they see. Any capability the body or the model can name is a capability an attacker can induce.

## Decision

The server decides every capability a turn exercises, and every string a reader sees passes through a server-built, governed path.

- `surface.plan-turn` chooses a tool name and its arguments from the turn's admitted read subset; a name outside it raises `ConsoleToolNotAdmitted`. An organization-wide face registers no write tool, and the one write a turn performs is server-authored.
- `surface.ground` reaches a stored conclusion only through recall; planner scaffolding naming a memory relation refuses. A conclusion distilled from a reading session lands under its own scope.
- `surface.render` infers a view on the server from a result over a closed component union; a client- or model-authored view refuses.
- `surface.speak` offers only suggestions the answer rules would answer, filtered when the set is built.
- `surface.brief` renders a greeting only when derived inside its budget; otherwise it silently does not exist.
- `surface.publish-answer` delivers to the asker alone; posting to an audience is the asker's separate act, carrying their provenance.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Server-chosen calls, recall as the one door, server-built views *(chosen)* | — | A caller-initiated write needs another surface; a component cannot ship from a third party; a conclusion-only answer waits on recall. |
| Client-named tools against an allowlist | The adversarial body | Arguments would still arrive unvalidated, so the weakest tool would set the boundary. |
| Writes behind a reader confirmation | Who confirms | The steered reader would confirm inside the steered answer. |
| One generic query tool the model fills in | Enumerability | No audit could say what a turn could do. |
| Memory exposed to the planner with a documented predicate | The checks | A predicate the model writes is one it can omit. |

## Consequences

- An injected instruction finds no write tool to reach for.
- The admitted set is listable ahead of any call.
- A broken greeting shows up only in spans.
