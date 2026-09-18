# 0040 — Tool listing is filtered by the caller's grants and a guessed identifier is refused

**Status:** accepted 2026-09-18
**Decides:** `read.register.refusal.ungranted-template`

## Context

Every template in a store's reviewed manifest projects into a tool: the template identifier
becomes the tool name and its declared positional parameters become a typed schema. A caller
connecting to the read face receives a listing of those tools and calls them by name.

Template identifiers are not arbitrary strings. They are authored by an operator to be
readable, and they name the metric, the segment or the report the template computes. In a
store serving several teams the identifiers spell out what those teams measure: which cohorts
are tracked, which vendors are reconciled, which churn definition is in use. An identifier is
therefore itself a disclosure, independent of whether the caller can run it.

The listing is also the only discovery surface. The describe payload covers tables, not
templates, and there is no directory a caller can consult for a template it lacks a grant
for. Whatever the listing withholds is not obtainable by any other route the read face
offers.

Two properties are therefore in tension for one caller: a listing wide enough to be useful
for discovering what exists, and a listing narrow enough that it discloses nothing about
other callers' work.

## Decision

Tool listing is filtered by the caller's grants. A template absent from a caller's listing is
also unreachable by a guessed identifier and raises `TemplateNotGranted`. A listing therefore
discloses no identifier the caller cannot run, and the refusal on a guess discloses no more
than the listing already did.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Filter the listing by grants; refuse a guessed identifier** *(chosen)* | The listing is exactly the callable surface. A caller cannot learn that another team's template exists, by listing or by probing. | A caller cannot discover a template in order to request access to it, so widening a grant starts outside the read face. |
| List every template and refuse on call | A caller sees the whole catalog and knows what to ask for. | The listing enumerates identifiers the caller may not run, which is the disclosure the identifiers themselves carry. |
| List every template with a per-entry granted flag | The same discoverability, with the caller's own reach made explicit. | Loses on the same criterion for the same reason: the flag changes the presentation of the disclosure, not the disclosure. |

## Criteria

1. **Non-disclosure** — whether the listing becomes an existence oracle for another caller's
   metrics. Both listing-everything options fail this.
2. **Discoverability for a legitimate caller** — whether a caller can find out what it could
   ask for. This is the criterion the chosen option loses on.
3. **Consistency between listing and call** — whether probing an unlisted name yields more
   information than the listing did. Refusing a guess with the same error the listing implies
   keeps these two answers in agreement.

Non-disclosure decides it. Discoverability has a route outside this face — an operator tells a
caller what exists — while non-disclosure has no substitute once the listing has spoken. A
surface that answers "does template X exist" for any X the caller can type is an enumeration
tool, and rate limits do not change that, only its speed.

## Consequences

The listing becomes a precise statement of the caller's reach, and a client application can
render it directly without a second check. An operator reviewing a manifest knows that
approving a template grants it to exactly the grant patterns named, with no ambient
visibility.

The accepted cost falls on onboarding. A caller that needs a template it cannot see has no
way to name it, so the request begins with a human conversation rather than a listing. That
cost scales with the number of templates and the number of teams, and it is unmeasured for a
store with many small teams sharing one manifest.

Reversing this is cheap on the listing side and not on the refusal side: widening the listing
is a filter change, while the guessed-identifier refusal is what stops the listing from being
bypassed, and relaxing that undoes the decision entirely.

## Revisit triggers

- A store's template count grows past the point where an operator can reasonably act as the
  discovery channel for every caller.
- A request-access path exists on the operator surface, giving discoverability a route that
  does not run through the read face's listing.
- Template identifiers acquire a declared public-name field distinct from their internal
  identifier, which would let a listing disclose a name carrying no information about the
  metric behind it.
