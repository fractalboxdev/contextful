# 0112 — The engine resolves an enumerated set of environment bindings and refuses any other spelling

**Status:** accepted 2026-09-18
**Decides:** `connector.declare-capability.refusal.binding-unsupported`, `connector.declare-capability.refusal.binding-unbound`

## Context

A source key binds from the environment through a `<key>_from = "env:<NAME>"` spelling.
The suffix is what marks a binding site, which makes the set of bindable keys a matter of
text pattern rather than of type: any key at all can be written with the suffix, and the
file parses.

That is the shape that produces the characteristic failure. A source has a key with a
default — a page size, a region, a base URL. An operator writes the key's name slightly
wrong, or writes a key the connector does not read at all, and adds the `_from` suffix.
Nothing refuses it. The read runs, uses the default, and succeeds. The manifest now states
a binding that governs nothing, and the only way to discover it is to notice that changing
the environment variable changes no behavior — a test nobody runs on a pipeline that works.

Shape is the second half of the same problem. A binding that resolves to a value of the
wrong form is bound, so a presence check passes it. A list-shaped key whose variable holds
only separators resolves to an empty list; a scalar key bound to a variable holding a comma
resolves to text nobody meant. Those fail at the first read against the vendor, minutes into
a run, with a diagnostic about the vendor rather than about the binding.

## Decision

The pairs the engine resolves are enumerated in one place, each entry carrying the key and
the shape of the value it expects — a scalar, or a comma-separated list. Validation locates
candidate sites by suffix and answers them from that enumeration. A key the connector does
not read raises `ConnectorBindingUnsupported`. A key the connector does read that the
environment did not supply raises `ConnectorBindingUnbound`, tested against that binding's
declared shape, so a list variable holding separators alone fails the preflight rather than
the first read.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **An enumeration with a declared shape per entry** *(chosen)* | A misspelled or unread key is a diagnostic; a malformed value surfaces at preflight. | Adding a bindable key is an edit in two places — the connector and the enumeration. |
| Pass any `<key>_from` through to the connector | No enumeration to maintain; a connector gains a bindable key by reading it. | Lost on distinguishing bound from defaulted: a typo leaves the manifest reading as bound while the read quietly used its default, and the manifest is the only record anyone consults. |
| Check presence and not shape | Half the maintenance; catches the typo case. | Lost on where the failure surfaces: a list variable holding only separators is present, so it passes the preflight and fails on the first vendor read, where the diagnostic is about the vendor. |
| Derive the bindable set from the connector at load by reflection | One source of truth; no duplicate edit. | Lost on the sandbox boundary: a component exposes no key inventory across the interface, and adding one makes the world's surface depend on host configuration vocabulary. |

## Criteria

1. **Whether a bound manifest is distinguishable from one that silently defaulted.** *This
   criterion decided it.* A manifest is the artifact a reviewer, an operator and an agent
   all read as the statement of what a pipeline does. A binding that reads as governing
   behavior and governs nothing corrupts the one surface every other control depends on.
2. **Where a malformed value surfaces.** Preflight against first read, and whether the
   diagnostic names the binding or the vendor.
3. **Maintenance cost of the enumeration.** How many places an author edits to add a key.
4. **Compatibility with the sandbox boundary.** Whether the check can be performed without
   widening the world.

## Consequences

Validation answers every binding site in the manifest before a session opens, and the
answer names the key. An operator who mistypes a key learns the supported set from the
diagnostic rather than from the source.

The accepted cost: the enumeration is a second place to edit. A connector author adding a
bindable key who forgets the enumeration entry ships a key that refuses as unsupported —
the failure is loud, but it is a failure in the authoring loop that would not exist under
pass-through. That duplication is the price of the property, and it is paid by the author
rather than by the operator.

The shape vocabulary is two entries, scalar and comma-separated list. A binding needing a
third shape — a duration, a URL, a JSON document — has no way to state it and either widens
the vocabulary or accepts a scalar and validates late.

## Revisit triggers

- A third value shape is needed by more than one binding, which would make the two-entry
  vocabulary the constraint rather than a simplification.
- The component interface gains a way for a guest to declare the configuration keys it
  reads, which would make the enumeration derivable rather than authored.
- Authors are observed shipping keys absent from the enumeration often enough that the
  duplicate edit is the dominant authoring error.
