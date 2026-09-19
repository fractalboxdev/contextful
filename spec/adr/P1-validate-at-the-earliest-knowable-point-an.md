# P1 — An unknown or ineffective name refuses by name at the earliest knowable point

**Status:** accepted

## Context

A misspelled table resolves to zero rows, a skipped restriction widens a credential, and a job bound to nothing fires forever. Each reads as healthy, and the cost grows with every stage the error survives.

## Decision

Every input is judged at the first point holding enough information to judge it: declaration load, manifest validation, plan, build, parse or mint. A table, column, config key, credential element, profile version, verb, destination or job target that is unknown, or declared where it has no effect, refuses there, naming the token and, where one exists, the spelling expected. Nothing is inferred in its place.

- `store.lay-out` refuses an unknown table at every resolving surface and reserves the leading-underscore column prefix and two table namespaces.
- `run.declare` refuses an unenumerated source key at any depth before I/O, derives destination names, and admits `replace` only over a source re-reading its whole input.
- `connector.source` takes format and encoding as declared, admits one pagination shape, and refuses an unmatched table pattern before any request.
- `authority.profile` refuses an unnamed element, an unevaluable restriction, an unimplemented version and an unknown verb at mint and at admission.
- `authority.filter-rows` parses a predicate to a typed boolean subset at manifest load.
- `surface.apply` refuses a job target that binds to nothing.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Refuse by name at the earliest knowable point *(chosen)* | — | Every key, element or verb is registered before use; an older reader refuses a newer manifest. |
| Resolve an unknown to empty, or skip it | Distinguishability | A typo reads as a quiet stream; a skipped restriction admits the credential unconstrained. |
| Warn and continue | Distinguishability in practice | A warning inside a successful scheduled run is read by nobody. |
| Validate at first use | Failure placement | The refusal lands after rows land or quota is spent. |
| Infer format, pattern or name from context | Stability of meaning | A vendor or a heuristic decides what a declaration means. |

## Consequences

- A deployment that validates starts, and run-time failures narrow to data and environment.
- Each source, profile and grammar owns a closed enumeration to maintain.

## Revisit

- An older reader required to accept a newer manifest, which calls for explicitly marked ignorable extensions.
