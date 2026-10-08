import Reference.Coverage
import Lean.Data.Json

/-!
# Grant narrowing legality

Rendered from `authority.grant.fields`, `authority.grant.actions`,
`authority.grant.template-allowlist`, `authority.grant.group-ceiling`,
`authority.attenuate.narrowing`, `authority.attenuate.widens`,
`authority.attenuate.tenant-dropped`, and the narrowing table in the authority contract's
Shapes section. Written from the specification text alone.

A proposed child grant list is legal under a parent grant list when every child grant
lies within one parent grant on every dimension. Widening is judged across every child
grant before tenant scope, the order the derivation flowchart gives.
-/

namespace Reference

open Lean (JsonNumber)

/-- The action vocabulary: `read`, `write`, `execute`, `admin`, `forget`. -/
inductive Action where
  | read | write | execute | admin | forget
  deriving Repr, DecidableEq

def Action.parse : String → Option Action
  | "read" => some .read
  | "write" => some .write
  | "execute" => some .execute
  | "admin" => some .admin
  | "forget" => some .forget
  | _ => none

/-- A tenant scope: a table and an opaque byte string, compared byte for byte. -/
structure Tenant where
  table : String
  value : String
  deriving Repr, DecidableEq

/-- An aggregate grant's constraints. The share stays an exact decimal. -/
structure Aggregate where
  minGroupSize : Nat
  share : JsonNumber
  functions : List String
  maxGroups : Nat
  maxRows : Option Nat

/-- A grant. An absent tenant, aggregate or row ceiling leaves its dimension
unconstrained; an absent template allowlist confers no template. -/
structure Grant where
  actions : List Action
  tables : List Pattern
  tenant : Option Tenant
  templates : Option (List String)
  aggregate : Option Aggregate

/-- The dimensions `AttenuationWidens` names, in the order the clause lists them. -/
inductive Dimension where
  | actions | tables | templates | aggregate
  deriving Repr, DecidableEq

def Dimension.name : Dimension → String
  | .actions => "actions"
  | .tables => "tables"
  | .templates => "templates"
  | .aggregate => "aggregate"

/-- Exact decimal order: `m₁ / 10^e₁ ≤ m₂ / 10^e₂`. -/
def decLe (a b : JsonNumber) : Bool :=
  a.mantissa * (10 : Int) ^ b.exponent ≤ b.mantissa * (10 : Int) ^ a.exponent

/-- The groups-per-query ceiling an aggregate grant takes effect with: at least 1. -/
def groupCeiling (declared : Nat) : Nat := max declared 1

/-- `*` in an allowlist authorizes every declared template. -/
def allTemplates : String := "*"

/-- A child allowlist names only identifiers its parent named or covered. Absent on the
child confers no template, so it is narrower than anything; an absent parent allowlist
confers none, so only an allowlist naming nothing lies within it. -/
def templatesWithin (c p : Grant) : Bool :=
  match c.templates, p.templates with
  | none, _ => true
  | some cs, none => cs.isEmpty
  | some cs, some ps => ps.contains allTemplates || cs.all ps.contains

/-- Each aggregate constraint holds or tightens. Absent on the child inherits the
parent's; absent on the parent is unconstrained. -/
def aggregateWithin (c p : Grant) : Bool :=
  match c.aggregate, p.aggregate with
  | none, _ => true
  | some _, none => true
  | some ca, some pa =>
    ca.minGroupSize ≥ pa.minGroupSize
      && decLe ca.share pa.share
      && ca.functions.all pa.functions.contains
      && groupCeiling ca.maxGroups ≤ groupCeiling pa.maxGroups
      && match ca.maxRows, pa.maxRows with
         | some cr, some pr => cr ≤ pr
         | _, _ => true

/-- The dimensions on which child grant `c` is broader than parent grant `p`. -/
def widened (c p : Grant) : List Dimension :=
  (if c.actions.all p.actions.contains then [] else [.actions])
    ++ (if c.tables.all (fun ct => p.tables.any (fun pt => pt.covers ct)) then [] else [.tables])
    ++ (if templatesWithin c p then [] else [.templates])
    ++ (if aggregateWithin c p then [] else [.aggregate])

/-- A parent tenant scope is kept byte for byte; adding a scope to an unscoped parent
narrows. -/
def tenantKept (c p : Grant) : Bool :=
  match p.tenant with
  | none => true
  | some t => c.tenant == some t

/-- The dimension a refusal names for a child grant within no parent grant: the first
dimension, in clause order, of the nearest parent grant — the first one widened on the
fewest dimensions. With no parent grant at all, the child is broader on every dimension
and the first, `actions`, is named. -/
def nearestDimension (c : Grant) (parent : List Grant) : Dimension :=
  let ws := parent.map (widened c)
  let best := ws.foldl
    (fun (acc : Option (List Dimension)) w =>
      match acc with
      | none => some w
      | some b => if w.length < b.length then some w else some b)
    none
  match best with
  | some (d :: _) => d
  | _ => .actions

/-- A decision the reference prints: a verdict, an error identifier, a dimension, and
the zone a placement case resolves. -/
structure Decision where
  verdict : String
  error : Option String := none
  dimension : Option String := none
  zone : Option String := none

def Decision.refused (error : String) (dimension : Option String := none) : Decision :=
  { verdict := "refused", error := some error, dimension }

/-- Narrowing legality of a child grant list under a parent grant list. -/
def narrow (parent child : List Grant) : Decision :=
  let within (c p : Grant) : Bool := (widened c p).isEmpty
  match child.find? (fun c => !(parent.any (within c))) with
  | some c => .refused "AttenuationWidens" (some (nearestDimension c parent).name)
  | none =>
    if child.all (fun c => parent.any (fun p => within c p && tenantKept c p)) then
      { verdict := "admitted" }
    else
      .refused "AttenuationTenantDropped"

end Reference
