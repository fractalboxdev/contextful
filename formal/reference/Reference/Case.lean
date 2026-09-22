import Reference.Coverage
import Reference.Narrowing
import Lean.Data.Json

/-!
# One case in, one decision out

A case is one JSON object naming its operation:

* `{"op":"covers_name","pattern":P,"name":N}` — does pattern `P` cover table name `N`;
* `{"op":"covers_pattern","pattern":P,"other":Q}` — does `P` cover every name `Q` covers;
* `{"op":"narrow","parent":[G…],"child":[G…]}` — narrowing legality of the child grant list.

A grant object carries `actions` and `tables` (arrays of strings), and optionally `tenant`
(`{"table","value"}`), `templates` (array of strings), `aggregate` (`min_group_size`,
`max_contributor_share`, `functions`, `max_groups`, optional `max_rows`) and `max_rows`.
An integer field holds an unsigned 64-bit integer written without fraction. A `null`
optional field is absent; an unknown key is ignored.

The decision is `{"verdict","error","dimension"}`. Decoding fails at the first fault, in
this order: the case's own shape, then each grant in turn, parent grants before child
grants — its shape, each action against the vocabulary, each table pattern.
-/

namespace Reference

open Lean

def caseMalformed : String := "CaseMalformed"

abbrev Decode := Except String

def malformed {α : Type} : Decode α := throw caseMalformed

/-- A present, non-null field. -/
def field? (j : Json) (k : String) : Option Json :=
  match j.getObjVal? k with
  | .ok .null => none
  | .ok v => some v
  | .error _ => none

def required (j : Json) (k : String) : Decode Json :=
  match field? j k with
  | some v => pure v
  | none => malformed

def str : Json → Decode String
  | .str s => pure s
  | _ => malformed

def strs : Json → Decode (List String)
  | .arr a => a.toList.mapM str
  | _ => malformed

/-- An unsigned 64-bit integer written without fraction or exponent shift. -/
def nat64 : Json → Decode Nat
  | .num n =>
    if n.exponent == 0 && n.mantissa ≥ 0 && n.mantissa < (2 : Int) ^ 64 then pure n.mantissa.toNat
    else malformed
  | _ => malformed

def number : Json → Decode JsonNumber
  | .num n => pure n
  | _ => malformed

def optional {α : Type} (j : Json) (k : String) (f : Json → Decode α) : Decode (Option α) :=
  match field? j k with
  | none => pure none
  | some v => some <$> f v

def isObject : Json → Bool
  | .obj _ => true
  | _ => false

def decodeTenant (j : Json) : Decode Tenant := do
  unless isObject j do malformed
  let table ← str (← required j "table")
  let value ← str (← required j "value")
  pure { table, value }

def decodeAggregate (j : Json) : Decode Aggregate := do
  unless isObject j do malformed
  let minGroupSize ← nat64 (← required j "min_group_size")
  let share ← number (← required j "max_contributor_share")
  let functions ← strs (← required j "functions")
  let maxGroups ← nat64 (← required j "max_groups")
  let maxRows ← optional j "max_rows" nat64
  pure { minGroupSize, share, functions, maxGroups, maxRows }

def decodeGrant (j : Json) : Decode Grant := do
  unless isObject j do malformed
  let actionWords ← strs (← required j "actions")
  let tableWords ← strs (← required j "tables")
  let tenant ← optional j "tenant" decodeTenant
  let templates ← optional j "templates" strs
  let aggregate ← optional j "aggregate" decodeAggregate
  let _ ← optional j "max_rows" nat64
  let actions ← actionWords.mapM fun w =>
    match Action.parse w with
    | some a => pure a
    | none => throw "GrantActionUnknown"
  let tables ← tableWords.mapM fun w =>
    match Pattern.parse w with
    | some p => pure p
    | none => throw "GrantPatternMalformed"
  pure { actions, tables, tenant, templates, aggregate }

def pattern (s : String) : Decode Pattern :=
  match Pattern.parse s with
  | some p => pure p
  | none => throw "GrantPatternMalformed"

/-- The shape of a grant list, before any grant decodes. -/
def grantArray : Json → Decode (List Json)
  | .arr a => pure a.toList
  | _ => malformed

def coverage (b : Bool) : Decision :=
  { verdict := if b then "covered" else "not_covered" }

/-- Decode a case and decide it. -/
def decide (j : Json) : Decode Decision := do
  unless isObject j do malformed
  let op ← str (← required j "op")
  match op with
  | "covers_name" =>
    let p ← str (← required j "pattern")
    let n ← str (← required j "name")
    let p ← pattern p
    pure (coverage (p.coversName n))
  | "covers_pattern" =>
    let p ← str (← required j "pattern")
    let q ← str (← required j "other")
    let p ← pattern p
    let q ← pattern q
    pure (coverage (p.covers q))
  | "narrow" =>
    let parent ← grantArray (← required j "parent")
    let child ← grantArray (← required j "child")
    let parent ← parent.mapM decodeGrant
    let child ← child.mapM decodeGrant
    pure (narrow parent child)
  | _ => malformed

def Decision.toJson (d : Decision) : Json :=
  let opt : Option String → Json
    | some s => .str s
    | none => .null
  Json.mkObj [("verdict", .str d.verdict), ("error", opt d.error), ("dimension", opt d.dimension)]

/-- The decision for one case text; a text that is not JSON is malformed. -/
def run (input : String) : Decision :=
  match Json.parse input >>= decide with
  | .ok d => d
  | .error e => .refused (if e == "GrantActionUnknown" || e == "GrantPatternMalformed" then e else caseMalformed)

end Reference
