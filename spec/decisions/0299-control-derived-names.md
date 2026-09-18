# 0299 — A store's credential and binding names derive from its id

**Status:** accepted 2026-09-18
**Decides:** `control.register-store.refusal.authored-credential-name`

## Context

A store entry in the runtime registry says which store exists, where its origin is, and how
the surface authenticates to it. The surface reaching that origin needs two names from the
worker's environment: the secret holding the store's bearer, and the service binding that
dispatches to its origin worker.

The environment those names resolve against holds more than one store's material. It holds
every store's bearer, and whatever else the worker needs — signing inputs, adapter
credentials, keys for surfaces unrelated to the registry. A name in that environment is a
pointer into a shared namespace, and the resolver cannot tell a pointer that was meant for
this entry from one that was not.

The registry is also runtime data. The array arrives from a deploy-time variable precisely
so a store can appear without a rebuild, which is the property that makes the surface
turn-key. It is not reviewed the way code is, and the parties who can set a worker variable
are not necessarily the parties who can read the worker's secrets.

Those two facts together are the exposure. An entry that authors its own credential name
can name any secret the worker holds, and the entry also authors the origin the proxy posts
to. A single edit to a runtime variable therefore reads a secret and sends it somewhere of
the entry's choosing, with no code change and nothing to review. The entry does not have to
be a store at all.

A quieter failure runs alongside it. An authored name and an id are two independent strings
that have to agree. They agree when the entry is written and drift afterwards — a store is
renamed, a secret is rotated under a new name, an entry is copied and edited halfway — and
the drift presents as a store that authenticates on one surface and not another.

There is a related temptation worth naming: inferring the authentication mode from whether a
secret is present. That would remove the need for an authored mode field, and it fails on
the development case, where a loopback store meant to send no bearer fails closed on a
credential nobody set.

## Decision

A store's bearer lives in the secret named by the upper snake-case of its id suffixed
`_QUERY_TOKEN`, and its service binding is the upper snake-case of that same id. Neither
spelling is a field on the entry, and an entry carrying its own credential or binding name
raises `StoreNameAuthored`. A store id is lower kebab-case, so it maps injectively onto a
legal environment-variable name and traverses onto no object key of its own choosing, and a
reserved-name list keeps an id from deriving onto a name the surface itself uses. The
authentication mode stays authored rather than inferred from a secret's presence: `token`
reads the derived secret, `exchange` mints a credential per visitor, `none` sends no bearer.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Derive both names from the id** *(chosen)* | An entry names exactly one thing — its own id — so a runtime edit cannot reach a secret belonging to anything else, and a store's id and its secret cannot drift apart. | Renaming a store rotates its secret name and its binding name, and a reserved-name list is required so an id cannot derive onto a name the surface itself uses. |
| An authored credential and binding name per entry | Maximum flexibility: a store reuses an existing secret, several stores share one, and a name can follow an external convention. | Lost on what an authored name can reach: an entry arriving through a runtime variable names any secret the worker holds and has the proxy post it to a host the entry picked, and an id and its secret name drift apart over time. |
| An authored name validated against an allowlist | Some flexibility with the reachable set bounded. | Lost on the same criterion, deferred: the allowlist is itself configuration, so the question becomes who can edit it, and a correct allowlist is exactly the derived set with extra authoring. |
| Deriving the authentication mode from the secret's presence as well | One fewer authored field; the mode can never contradict the environment. | Lost on the development case: a loopback store meant to send no bearer fails closed on a credential nobody set, and presence is not intent. |

## Criteria

1. **What an authored name can reach** — the set of environment values an entry can point
   at. **This criterion decided.** The registry is runtime data with a wider edit surface
   than code, and the entry also authors its origin, so a name field turns one variable
   edit into read-and-exfiltrate of any secret the worker holds; flexibility elsewhere is
   not priced against that.
2. **Drift between an id and the names that serve it** — two independent strings that have
   to agree will eventually not.
3. **Injectivity of the derivation** — an id has to map onto one legal environment-variable
   name and onto no object key of its choosing, which is why kebab-case is required rather
   than conventional.
4. **Operational flexibility** — sharing a secret across stores, following an external
   naming convention. The chosen option forecloses both.

## Consequences

An entry names one thing, its id, and everything else about how the surface reaches it
follows. A reader of the environment can compute which secret belongs to which store
without reading the registry, and a store that authenticates in one place authenticates in
all of them, since every path needing a credential — the tool calls, the published output
routes, the prompt overlay — passes one resolver. Authoring the mode separately keeps a
development store on `none` without a secret and keeps `exchange` turn-key with no
pre-provisioned material.

The cost accepted: an id is operationally critical rather than cosmetic, so renaming a
store is a coordinated rotation of its secret name and its binding name rather than an edit
to a label. Two stores cannot share a bearer, even where an operator has a good reason. The
reserved-name list is a permanent obligation: every name the surface itself uses in the
worker environment has to be kept out of the derivable set, and forgetting one lets an id
derive onto it.

## Revisit triggers

- Store renames become frequent enough that coordinated secret rotation is a recurring
  operational cost rather than a rare one.
- A legitimate need to share one credential across several stores appears that the exchange
  mode does not already serve.
- The reserved-name list collides with an id an operator reasonably wants, indicating the
  derivation needs a namespace prefix rather than a denial.
