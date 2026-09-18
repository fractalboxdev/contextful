# 0314 — A rendered view is inferred on the server from a result, never authored by a client or a model

**Status:** accepted 2026-09-18
**Decides:** `console.render.refusal.client-authored-view`

## Context

Beyond prose, everything a reader sees on this surface is a component name drawn from a
closed versioned union plus typed properties. It is never code, never markup and never a
URL. The client resolves the name against the registry it ships and draws nothing for a
name it does not hold. The question this leaves open is who composes the name and the
properties.

The surface carries a content guarantee, not only a code-safety one. Relation names, column
identifiers and infrastructure vocabulary do not reach the page, and machine spellings of
an instant are rewritten into readable prose. That guarantee has to hold over every string
a reader sees, including a caption, a series name and an axis tick. It is enforced as a
walk over the rendered object, with the rule applied chosen by where each string came from:
an authored label takes identifier redaction, a cell takes temporal rewriting, a length cap
and exact-match redaction. A walk of that kind needs an object whose shape it knows, built
by the party that also knows which strings are authored and which are data.

The model's presentational power is deliberately one choice: which admitted tool to call.
Properties come from the result's shape and the store's declared bindings. A view hint in a
store's manifest binds column names and carries no values, so a wrong or hostile hint
produces a wrong-looking picture rather than a fabricated number. Both paths reach numbers
only through columns a governed read actually returned.

A component name outlives the turn that produced it. A payload is JSON and survives a
reload out of browser storage, so a name persists inside a saved transcript and the union
is a compatibility surface rather than an internal enumeration. Whatever composes a view
decides what a client two versions later has to keep able to draw.

## Decision

The server infers a view from a result and refuses one composed elsewhere: a view
specification arriving from a client, or assembled by the model rather than inferred from a
result, raises `ConsoleViewNotServerBuilt`. Column kinds decide the component, properties
resolve from the returned columns, and the sanitizer walks the finished object before it
leaves. A hint from the store's manifest steers that inference by naming columns and never
by carrying a value.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Server-side inference from the result, with a closed component union** *(chosen)* | The content walk runs over an object the server built, provenance per string is known, and a view is reachable only through numbers a governed read returned. | A third party cannot ship a component into this surface, and a picture the property types cannot express waits on a new union member carrying a renderer and a validator arm together. |
| A model emitting component trees against a published catalogue | Expressiveness with no new server code per shape, and a model that already picked the tool picks the picture too. | Lost on reliability of a valid specification. A malformed tree degrades to broken interface, where a wrongly chosen tool degrades to prose the reader can still read; and the walk would have to trust a provenance claim the model made about its own strings. |
| Model-authored markup rendered in a sandboxed frame | Any picture at all, with no union to extend and no registry to ship. | Lost on the content guarantee. A sandbox bounds what code can reach and says nothing about what text is displayed, so the redaction and temporal rules would have no place to run. |
| Client-side inference from raw rows | The server ships rows and nothing else, and each client draws what its own version can. | Lost on the content guarantee again: the sanitizer would run in a context the server cannot attest, and raw rows carrying provenance columns and machine spellings would have to cross to the browser to be inferred from. |

## Criteria

1. **Whether the content guarantee holds as a walk over an object** — whether every
   visitor-visible string is reachable by the rule that governs it. *This criterion decided
   it.* The surface's whole register rests on that guarantee; a rendering path where the
   walk cannot run, or where it must trust a claim about string provenance, removes the one
   mechanism standing between engine vocabulary and a non-technical reader. Expressiveness
   is a real advantage of the rejected options and is worth less than that.
2. **Reliability of a valid specification from a driver model** — how a malformed output
   degrades.
3. **What a saved transcript has to stay able to render** — whether the persisted artifact
   is a bounded name or an open tree.
4. **Fabrication distance** — how many steps separate a rendered number from a column a
   governed read returned.

## Consequences

Failure is soft and uniform: a view failing validation yields no widget rather than an
error, and the answer stands without its picture because the synthesis text is never told a
widget exists. Adding a shape is one place — a union member with a renderer and a validator
arm — and the same place that keeps older transcripts drawable.

The accepted cost is extensibility. No third party ships a component here, and a domain
whose numbers take a shape the union does not carry waits for that member rather than
authoring its own. Withdrawing a name is expensive once transcripts hold it, so the union
grows more easily than it shrinks.

## Revisit triggers

- The union's growth rate outpaces what one registry can ship, so shapes are waiting on
  members more often than readers are getting pictures.
- A property type is needed that inference cannot supply from columns alone, forcing a
  value rather than a column binding into a hint.
- An embedding context requires a component the surface does not own, which re-opens
  third-party registration under a validator the server still runs.
