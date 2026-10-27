# P1 — An unknown or ineffective name refuses by name at the earliest knowable point

**Status:** accepted

## Context

A misspelled table resolves to zero rows, and a skipped restriction widens a credential. Each reads as healthy.

## Decision

Every input is judged at the first point holding enough information to judge it. An unknown name, or one declared where it has no effect, refuses there. The refusal names the token and, where one exists, the expected spelling. Nothing is inferred in its place.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Refuse by name at the earliest knowable point *(chosen)* | — | Every key, element or verb is registered before use; an older reader refuses a newer manifest. |
| Resolve an unknown to empty, or skip it | Distinguishability | A typo reads as a quiet stream; a skipped restriction admits the credential unconstrained. |
| Validate at first use | Failure placement | The refusal arrives after rows are written or quota is spent. |

## Consequences

- A deployment that validates starts, and run-time failures narrow to data and environment.
- Each source, profile and grammar owns a closed enumeration to maintain.

## Revisit

- An older reader required to accept a newer manifest, which calls for explicitly marked ignorable extensions.
