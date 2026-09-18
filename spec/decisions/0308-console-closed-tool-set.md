# 0308 — The visitor endpoint dispatches a server-chosen name from a closed read subset

**Status:** accepted 2026-09-18
**Decides:** `console.ground.refusal.unadmitted-tool`, `console.ground.refusal.mutating-tool`

## Context

The console is a browser surface behind an identity perimeter. The page calls same-origin
routes and the server attaches the store's credential outbound, so no upstream secret
reaches the browser. Two independent layers decide what a reader receives: the perimeter
decides who reaches the page, and the presented capability decides what the engine returns.

Between those two layers sits the turn. A turn calls tools — a catalogue listing, a schema
read, a structured query, a free-text search, a file listing, a file preview, and for a
store whose operator configured a key, a public-web lookup. A turn also performs exactly one
write, the distillation that lands the session's conclusions.

The request body of that turn arrives from a browser. Anything read out of it is under the
control of whoever can reach the page, and the perimeter admits people, not intentions — a
reader with a legitimate identity can still craft a body. So the question is what the body is
allowed to decide. If it names the tool, then the set of capabilities a reader reaches is
whatever the body says, bounded only by whatever validation sits behind each name. If it can
name the write, then a page can be induced to make one.

The packs that govern a turn also need to be enumerable ahead of a call, because the content
guarantees this surface publishes — product language, no direct path under the tables,
grounding from admitted reads only — are claims about what the turn can possibly do.

## Decision

A tool name and its arguments are chosen on the server from the turn's admitted set, and
neither is read out of the request body. A call naming a tool the turn's packs do not admit
raises `ConsoleToolNotAdmitted` and dispatches nothing. The visitor-facing endpoint admits a
read subset; a client-reachable path naming a write raises `ConsoleMutatingToolRequested`,
and the single write a turn performs is authored on the server.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Server-chosen name and arguments from a closed, enumerable read subset** *(chosen)* | A content guarantee on a public surface holds against an adversarial body, because the body decides nothing about capability. The admitted set is listable ahead of any call, so an audit reads it rather than inferring it. | Every reader-visible capability is enumerated server-side, so exposing one is a server change rather than a prompt change, and an agent integrating against this surface cannot reach a tool the packs withhold. |
| Client-named tools validated against an allowlist | Familiar shape; the client drives and the server bounds. | Lost on the adversarial-body criterion: the name is checked but the arguments still arrive unvalidated, so each tool's own argument surface becomes the boundary and the guarantee is only as strong as the weakest tool's parameter handling. |
| A single generic query tool the model fills in | Maximum reach over the store with one name to expose and nothing to maintain. | Lost on enumerability: one name admits every statement, so the packs have nothing to bind and no audit can say what a turn could do. The guarantees would have to be re-established inside the statement, which is the hardest place to establish them. |
| Mutating tools admitted under a per-reader grant | A reader with write authority gets a richer surface without a separate path. | Lost on the adversarial-body criterion in its sharpest form: a write reachable from a page is a write a page can be induced to make, and a grant check decides whether the reader may write, never whether the reader meant to. |

## Criteria

1. **Whether a content guarantee on a public surface survives an adversarial request body** —
   that is, whether anything the browser sends can widen what the turn reaches. **This
   criterion decided.** A name read off the request is a grant the surface did not make, and
   every other consideration is a cost measured against a property the surface either has
   completely or does not have at all.
2. **Enumerability of the admitted set ahead of a call** — whether an audit can state what a
   turn could have done without replaying it.
3. **Separation of read from write** — whether the endpoint a page reaches can reach a
   mutation at all, independent of who is asking.
4. **Cost of exposing a new capability** — the work between deciding a tool should be
   reachable and it being reachable. This points the other way and is the accepted cost.

## Consequences

The packs become the surface's security description: reading them says what a turn can
reach, with no need to reason about request handling. The one write a turn performs is
authored in code at a known point, so it carries the reading-session scope by construction
rather than by the model remembering to. Argument construction moves entirely server-side,
which is also where the store's declared bindings and the session's vantage already live, so
every call carries them without the model being asked to.

The cost accepted: the surface is closed. Adding a capability is a server change and a
deploy, so a store with an unusual need cannot be served by a prompt adjustment, and an
agent integrating against this endpoint reaches exactly the packs' set and nothing beyond
it. That rigidity is the same property the guarantee rests on, so it cannot be relaxed
selectively without relaxing the guarantee.

## Revisit triggers

- Stores routinely need a capability the packs withhold, so the closed set is blocking
  legitimate use rather than adversarial use.
- A separate, non-browser-reachable endpoint is needed for agent integration, which would
  move this decision's scope rather than change it.
- Argument validation becomes strong enough and uniform enough that a client-named call
  could carry the same guarantee, which is the ground the allowlist option lost on.
