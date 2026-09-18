# 0306 — A suggestion offered to a reader is one the analyst answers under the answer-hygiene contract

**Status:** accepted 2026-09-18
**Decides:** `console.speak.refusal.unanswerable-suggestion`

## Context

The console advertises what a store can answer before anyone asks. Suggestion chips derive
at load from the store's own catalogue: list the tables, read each content table's schema,
turn column kinds into business-language labels. A time column earns a latest-of chip, a
categorical column a by-facet chip, a numeric column a top-by-measure chip. The generator
assumes no domain table, so a store added to a deployment earns its chips with no change to
the product.

The same surface holds an answer-hygiene contract. The analyst's persona and audience
contract say the reader learns what the data says about their question and nothing about
how the answer was assembled, and a question reaching for machinery — the statement that
ran, the engine underneath, the name of a table — earns one friendly sentence declining
followed by what the data says.

These two rules meet at the chip set. A chip is a question the product asks on the reader's
behalf. Generation is mechanical and the catalogue includes tables whose subject is the
engine's own machinery, so a generated chip can be a well-shaped question about exactly the
material the analyst is required to turn away. A reader who clicks it is invited into a
decline by the surface that offered it.

A decline is a correct answer to a question a reader chose to ask. It is an incoherent
answer to a question the product chose for them.

## Decision

The suggestion set is filtered against the decline rule when it is built. A suggested prompt
whose answer under these rules would be a decline raises `ConsoleSuggestionUnanswerable`,
rather than offering a question the analyst turns away. A table whose subject is the
engine's own machinery contributes no chip, and the filter runs at build time over the
generated set rather than at answer time over the reader's click.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Filter the generated set against the decline rule at build time** *(chosen)* | A reader is never invited into a decline, and the check is deterministic over a set the product produced, so it is testable with no model in the loop. | The filter carries a notion of what counts as machinery, and that notion has to track the engine's own table set as it grows. |
| Offer every generated suggestion and let the decline handle it | No filter to write or maintain; the decline already exists and already works. | Lost on the reader's experience of the surface: a product that asks a question and then refuses it reads as broken rather than as governed, and the decline's friendly sentence does not repair a refusal the reader did not choose. |
| Relax the decline rule for a suggested prompt, on the grounds that the product vouched for it | Both features work as generated; no coordination between them. | Lost on the audience contract, which the surface holds without exception. A prompt's origin does not change who is reading the answer, and an exception carved for chips is an exception an answer can be steered into. |
| Curate the suggestion set by hand per store | Perfect control over what is offered; no heuristic to maintain. | Lost on discovery: a store new to a deployment would ship with no suggestions at all until someone wrote them, which removes the property that a store earns its chips with no change to the product. |

## Criteria

1. **Whether a reader is ever invited into a decline** — that is, whether the surface asks a
   question it then turns away. **This criterion decided.** The decline is a coherent move
   in response to a reader's own curiosity and an incoherent one in response to the
   product's suggestion; no saving in filter maintenance is worth a surface that visibly
   contradicts itself on a click.
2. **Determinism of the check** — whether the filter can be exercised over a generated chip
   set without a live model. The generator is deterministic, so the filter over it can be
   too.
3. **Suggestion coverage on a thin store** — how many chips survive when a store holds
   little beyond engine tables.
4. **Filter maintenance** — the cost of keeping the machinery notion aligned with the
   engine. This points the other way and is the accepted cost.

## Consequences

The chip set and the answer contract stay consistent by construction, so the two can be
tested together over a fixture store with no model. The narrower filter over chips also has
a natural counterpart: dropping a chip costs a suggestion, where dropping a column would
cost the reader an answer, so the chip filter can be stricter than the one over rendered
headers without harming a result.

The cost accepted: a store whose only well-shaped chips point at machinery offers fewer
suggestions, and in the limit offers none — the empty chip set is a normal state the
composer and the file gallery keep working around. The filter also holds a list of what
counts as machinery, and that list is a second place the engine's reserved namespaces are
written down; it drifts if the engine adds a table kind and the filter is not told.

## Revisit triggers

- The engine adds a reserved table kind and the filter continues to generate chips over it,
  showing the list has drifted.
- Stores in normal use routinely produce an empty chip set, so the filter is removing the
  feature rather than sharpening it.
- The decline rule changes shape such that a decline stops reading as a refusal, which would
  remove the incoherence the filter exists to prevent.
