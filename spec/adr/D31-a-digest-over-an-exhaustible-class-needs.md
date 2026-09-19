# D31 — A digest over an exhaustible class needs generalizing truncation

**Status:** accepted

## Context

A national identifier, phone number, email address or medical record number has an enumerable value space. A keyed digest over it is a lookup table for any party able to compute the digest: enumeration returns the person. Hashing is chosen for equality filters and joins, which a generalizing step can keep.

## Decision

A digest over an exhaustible class ships only behind truncation, so an exhaustive search returns a crowd rather than a person.

- `authority.mask` refuses a sensitivity class outside the class registry with `EnforceUnknownClass` at manifest check.
- A keyed hash standing alone over an exhaustible class refuses; `combine = "truncate:<n>"` is the secondary that satisfies it. Random identifiers and opaque tokens carry a bare keyed hash.
- Truncation is the one secondary admitted behind a hash or tokenization; bucketing and numeric range generalization over a digest refuse.
- A truncation width at or past the primary's output width refuses. Narrower widths are the operator's judgment, since the manifest declares no domain size.
- `authority.mask` receives primary and secondary as one compiled value whose halves do not separate.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Refuse a bare digest; admit it behind truncation; refuse only no-op widths *(chosen)* | — | A join on such a column lands on a crowd; a too-wide truncation passes and leaves a near-unique mapping. |
| A bare digest relying on key secrecy | Cost of recovery | Any party computing the digest would recover identities by enumeration. |
| Refuse hashing outright for these classes | Utility | Equality filters and joins would disappear. |
| A fixed width threshold or declared domain size | Honesty | The threshold would fit one domain; a declared size would be a guess treated as fact. |
| Treat an unknown class as unclassified | Failure direction | A typo would silently downgrade protection. |

## Consequences

- A band and a joinable key need two columns rather than one mask.
- Sufficient narrowness is unverified by the engine.

## Revisit

- Manifests gain a measured domain size per class, making width derivable.
- A per-person join over an exhaustible class becomes a requirement.
