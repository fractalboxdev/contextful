# 0173 — The command line trims a padded subject value and the automated exchange refuses one

**Status:** accepted 2026-09-18
**Decides:** `authority.identify.refusal.padded-value`

## Context

Subject values are checked at the mint, and surrounding whitespace is one of the things
checked. Two surfaces mint. A person runs the command-line mint and types or pastes a
value; a shell, a clipboard and a terminal all append a trailing newline or a space
without being asked, and the person did not put it there. A calling system runs the
automated exchange, where the subject value is produced by a claim template reading a
verified assertion — the padding is not a slip of the hand, it is a template that is
wrong and will be wrong on every request it serves until somebody changes it.

The same input arrives on both, and it means opposite things. On the command line it is
transport noise around a value with exactly one correct reading. On the exchange it is
the only evidence anyone will ever get that the mapping from a verified claim onto the
subject tuple is misconfigured.

Trimming carries a second cost on the automated path that it does not carry on the
interactive one. The exchange mints continuously and unattended. A padded value silently
trimmed produces credentials that work, so the broken template is never noticed — and
it is noticed the moment the template's other consumer, or a later version of it, stops
padding, at which point the identity has changed without anything having been edited.

## Decision

The command-line mint trims a padded subject value and mints under the trimmed value. The
automated exchange refuses one and raises `AuthoritySubjectMalformed`, minting nothing. A
value failing any other hygiene rule — empty, oversized, carrying a control character —
raises that same identifier on both paths.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Trim interactively, refuse on the exchange** *(chosen)* | A person is not stopped by their terminal; a broken claim template fails on its first request rather than on its thousandth | Two surfaces behave differently on one input, so a script shelling out to the command line accepts what the route refuses |
| Trim on both | One rule, minimum surprise, nothing ever turned away for whitespace | Lost on silence: a misconfigured template mints working credentials forever, and the identity it maps onto changes the day the padding does |
| Refuse on both | One rule, and the mint never rewrites what it was given | Lost on ergonomics: a pasted value with a trailing newline is refused for a defect with a single correct reading, on the surface where a human is present to be told |
| Trim on both and report the trim | Nothing is turned away and nothing is silent | Lost on where the signal lands: the exchange's caller is a program that does not read warnings, so a reported trim on that path is a refusal nobody performs |
| Refuse on the exchange and on the command line under a flag | Ergonomics by default, strictness available | Lost on effort for what it buys: the flag's correct setting is determined by which surface is minting, which the mint already knows |

## Criteria

1. **Silence** — whether a misconfiguration can mint working credentials indefinitely
   without anyone learning of it.
2. **Ergonomics at the interactive surface** — whether a person is refused for something
   their terminal did.
3. **Uniformity** — how many rules the mint carries for one input.
4. **Where the signal lands** — whether the party who can fix the defect is the party the
   surface is able to tell.

Criterion 1 decided it against uniformity. One rule on both surfaces is plainly cheaper
to state and to hold in mind, and criterion 3 is the only thing the split gives up.
What outranks it is that a trimmed exchange mint is indistinguishable from a correct one
at every later point — the credential verifies, the reads succeed, and the divergence
appears later as an identity that silently became a different identity. The interactive
path has a human standing in front of it who will see the value they get; the automated
path has nobody, which is why the two cannot share an answer.

## Consequences

A claim template that pads is caught on its first request, by the party that wrote it,
with an error identifier naming what is wrong. A person minting by hand is not fighting
their clipboard. Every other hygiene violation stays uniform across both surfaces, so
the split is one rule wide rather than a second rule set.

The cost accepted is that two mint surfaces disagree about one input. A script that
drives the command-line mint is a program on the surface built for a person, and it gets
the lenient treatment the automated route would have refused — so an automated caller
can obtain by shelling out exactly what the route denies it. Closing that would mean
detecting non-interactive use, which is a weaker signal than the surface itself.

## Revisit triggers

- Automated callers are observed driving the command-line mint in production, which
  makes the surface a poor proxy for whether a human is present.
- The claim template gains its own validation at configuration time, so a padding
  template is refused before it ever mints and the exchange's refusal stops being the
  first signal.
- A third mint surface appears whose audience is neither clearly interactive nor clearly
  automated.
