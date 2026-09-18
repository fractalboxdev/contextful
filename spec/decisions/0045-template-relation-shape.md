# 0045 — A template's SQL names the store's own tables as plain identifiers and nothing else

**Status:** accepted 2026-09-18
**Decides:** `read.guard.refusal.template-relation-shape`

## Context

A template is operator-authored SQL carried in a reviewed manifest. Its text executes raw,
with table functions available, because admission binds on who authored the text rather than
on how privileged the caller is — the caller supplies only an identifier and typed positional
arguments. That is what makes a template useful: adding a metric is adding a template, with no
deploy of the calling application.

It also means the review is the control. Whatever a template's SQL can reach, it reaches with
no per-request check standing between it and the engine, for every caller granted the
template. The reviewer approving a manifest is approving a set of reads, and the reviewer can
only approve what the text visibly says it reads.

Plain table names say it. A bare filesystem path, a table function and a schema-qualified
catalog reference each say something a reviewer has to reconstruct: what the path resolves to
on the deployed machine, what the function will open, what sits in that catalog. A reviewer who
cannot state what a template reads from reading it is not reviewing it.

Table names in a store are not always simple, though. A prefixed table name needs a separator
in it, and a shape rule that forbade every separator would make a class of legitimate tables
unaddressable from a template.

## Decision

Manifest validation and face startup refuse a template whose SQL names anything but the store's
own tables as plain identifiers, raising `TemplateNamesForeignRelation`. A plain identifier
carries a path separator where a prefixed table name needs one, and carries no dot, no star and
no leading separator. Both checks run on the same text, so a template that validates in a
manifest also starts.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Plain identifiers only, checked at validation and at startup** *(chosen)* | A reviewer reads the template and knows what it reads. The check runs twice, ahead of any caller, and costs a request nothing. | A table name needing a dot cannot be addressed from a template, and prefixed names are expressed with a separator instead. |
| Check a template at call time under the caller's grants | One check site; the template's reach narrows per caller automatically. | Loses on cost and on reviewability — the same text is re-decided per request and per caller, and what a reviewer approved depends on who calls it. |
| Permit a qualified catalog reference | Templates reach attached catalogs and system metadata where an operator wants them to. | Loses on reviewability: the approved text reaches outside the store, and what it finds there is a property of the deployment rather than of the text. |

## Criteria

1. **Reviewability** — whether a reviewer approving a template can state what it reads from the
   text alone. Both rejected options fail this, for different reasons.
2. **Cost per request** — what the check adds to a call. Call-time checking fails this; a
   startup check is paid once per process.
3. **Expressiveness** — which legitimate table names remain addressable. This is the criterion
   the chosen option loses on, and the separator allowance is how far it bends.
4. **Agreement between the two check sites** — whether a template accepted into a manifest can
   fail to start, or the reverse. Running the identical rule at both points settles this.

Reviewability decides it. A template executes raw, so the review is the only gate it passes
through, and a gate that depends on facts outside the text is not a gate the reviewer operates.

## Consequences

A manifest is a readable statement of the store's callable surface, and approving one is an act
a reviewer can perform with the manifest alone. Startup fails loudly on a template that a
manifest edit broke, rather than at the first call.

The accepted cost is expressiveness. A table whose name genuinely contains a dot is not
addressable from a template, and any legitimate need for a qualified reference — reading an
attached catalog, joining across stores — has no route through templates at all and needs a
different surface. Every template edit re-runs both checks, which is a small cost paid at
authoring time rather than at request time.

Reversing this is cheap to state and expensive to live with: permitting qualified references
would make every previously reviewed manifest's guarantee weaker than it was when it was
approved.

## Revisit triggers

- A store legitimately carries table names containing a dot, making the shape rule exclude real
  tables rather than hypothetical ones.
- Cross-store joins become a supported read, which would need a template surface that can name
  something outside this store.
- The reviewed-manifest model changes such that template text is no longer executed raw, which
  removes the reason reviewability outranks expressiveness here.
