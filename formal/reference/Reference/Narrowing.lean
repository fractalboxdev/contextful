import Contextful.Authority
import Reference.Decision

/-!
# Grant narrowing legality

The decision a narrowing case prints, over `Authority.Grant.widened`,
`Authority.Grant.includedIn` and the expiry order. Widening is judged across every child
grant first, then expiry, then tenant scope: the order the derivation flowchart gives.
A widening names the first dimension of the nearest parent grant.
-/

namespace Reference

open Authority (Action Dimension Grant)

def parseAction : String → Option Action
  | "read" => some .read
  | "write" => some .write
  | "execute" => some .execute
  | "admin" => some .admin
  | "forget" => some .forget
  | "read-individual" => some .readIndividual
  | _ => none

def dimensionName : Dimension → String
  | .actions => "actions"
  | .tables => "tables"
  | .templates => "templates"
  | .aggregate => "aggregate"
  | .maxRows => "max_rows"
  | .maxDurationMs => "max_duration_ms"
  | .maxResponseBytes => "max_response_bytes"
  | .horizon => "horizon"

/-- The first of the shortest lists: a later list replaces the held one only when strictly
shorter. -/
def fewest : List (List Dimension) → Option (List Dimension)
  | [] => none
  | w :: ws =>
    match fewest ws with
    | some b => if b.length < w.length then some b else some w
    | none => some w

/-- The dimension a widening refusal names for a child grant within no parent grant: the first
dimension of the nearest parent grant, the first one widened on the fewest dimensions. With no
parent grant, the child is broader on every dimension and the first, `actions`, is named. -/
def nearestDimension (c : Grant) (parent : List Grant) : Dimension :=
  match fewest (parent.map c.widened) with
  | some (d :: _) => d
  | _ => .actions

/-- Whether child grant `c` lies within no parent grant on the widening dimensions. -/
def broaderThanAll (parent : List Grant) (c : Grant) : Bool := !parent.any (fun p => (c.widened p).isEmpty)

/-- Narrowing legality of a child grant list and expiry under a parent grant list and expiry. -/
def narrow (parent : List Grant) (parentExpiry : Nat) (child : List Grant) (childExpiry : Option Nat) : Decision :=
  match child.find? (broaderThanAll parent) with
  | some c => .refused "AttenuationWidens" (some (dimensionName (nearestDimension c parent)))
  | none =>
    if childExpiry.all (fun e => decide (e ≤ parentExpiry)) then
      if child.all (fun c => parent.any (fun p => c.includedIn p)) then .admitted
      else .refused "AttenuationTenantDropped"
    else .refused "AttenuationExpiryExtended"

/-- A child is admitted exactly when each of its grants lies within one parent grant on every
dimension, tenant scope included, and its expiry falls no later than its parent's. -/
theorem narrow_ok_iff_includedIn :
    ∀ (parent : List Grant) (pe : Nat) (child : List Grant) (ce : Option Nat),
      (narrow parent pe child ce).verdict = "admitted" ↔
        (∀ c ∈ child, ∃ p ∈ parent, c.includedIn p = true) ∧ ∀ e, ce = some e → e ≤ pe := by
  intro parent pe child ce
  unfold narrow
  cases hf : child.find? (broaderThanAll parent) with
  | some c =>
    simp only [Decision.refused]
    simp only [String.reduceEq, false_iff, not_and]
    intro hall
    have hc := List.find?_some hf
    obtain ⟨p, hp, hinc⟩ := hall c (List.mem_of_find?_eq_some hf)
    have hw := ((Bool.and_eq_true _ _).mp hinc).1
    simp only [broaderThanAll, Bool.not_eq_true', List.any_eq_false] at hc
    exact absurd hw (by simpa using hc p hp)
  | none =>
    have hexp : (ce.all (fun e => decide (e ≤ pe)) = true) ↔ ∀ e, ce = some e → e ≤ pe := by
      cases ce <;> simp
    have hinc : (child.all (fun c => parent.any (fun p => c.includedIn p)) = true) ↔
        ∀ c ∈ child, ∃ p ∈ parent, c.includedIn p = true := by
      simp [List.all_eq_true, List.any_eq_true]
    by_cases he : ce.all (fun e => decide (e ≤ pe)) = true
    · by_cases hi : child.all (fun c => parent.any (fun p => c.includedIn p)) = true
      · simp only [he, hi, ↓reduceIte, Decision.admitted, true_iff]
        exact ⟨hinc.mp hi, hexp.mp he⟩
      · simp only [he, ↓reduceIte, Bool.false_eq_true, hi, Decision.refused, String.reduceEq, false_iff, not_and]
        exact fun h _ => hi (hinc.mpr h)
    · simp only [he, ↓reduceIte, Bool.false_eq_true, Decision.refused, String.reduceEq, false_iff, not_and]
      exact fun _ h => he (hexp.mpr h)

theorem fewest_spec :
    ∀ ws : List (List Dimension), ws ≠ [] →
      ∃ w ∈ ws, fewest ws = some w ∧ ∀ v ∈ ws, w.length ≤ v.length := by
  intro ws hne
  induction ws with
  | nil => exact absurd rfl hne
  | cons w ws ih =>
    by_cases hws : ws = []
    · subst hws
      exact ⟨w, List.mem_singleton_self w, rfl, fun v hv => by rw [List.mem_singleton.mp hv]; exact Nat.le_refl _⟩
    · obtain ⟨b, hb, hfb, hmin⟩ := ih hws
      have hf : fewest (w :: ws) = if b.length < w.length then some b else some w := by
        simp only [fewest, hfb]
      by_cases hlt : b.length < w.length
      · refine ⟨b, List.mem_cons_of_mem _ hb, by rw [hf, if_pos hlt], fun v hv => ?_⟩
        rcases List.mem_cons.mp hv with rfl | hv
        · exact Nat.le_of_lt hlt
        · exact hmin v hv
      · refine ⟨w, List.mem_cons_self, by rw [hf, if_neg hlt], fun v hv => ?_⟩
        rcases List.mem_cons.mp hv with rfl | hv
        · exact Nat.le_refl _
        · exact Nat.le_trans (Nat.le_of_not_lt hlt) (hmin v hv)

/-- Widening refuses first and names its dimension: when some child grant lies within no
parent grant on the widening dimensions, the decision is `AttenuationWidens` whatever the
expiry and tenant scope, naming, for the first such child grant, a dimension on which it is
broader than a nearest parent grant, one it widens on the fewest dimensions. -/
-- spec: authority.attenuate.widens@6643708e
theorem narrow_widens_nearest :
    ∀ (parent : List Grant) (pe : Nat) (child : List Grant) (ce : Option Nat),
      (∃ c ∈ child, ∀ p ∈ parent, (c.widened p).isEmpty = false) →
        ∃ c ∈ child, narrow parent pe child ce =
            .refused "AttenuationWidens" (some (dimensionName (nearestDimension c parent))) ∧
          (parent = [] → nearestDimension c parent = .actions) ∧
            ∀ q ∈ parent, ∃ p ∈ parent, nearestDimension c parent ∈ c.widened p ∧
              (c.widened p).length ≤ (c.widened q).length := by
  intro parent pe child ce ⟨c₀, hc₀, hb₀⟩
  have hpred : broaderThanAll parent c₀ = true := by
    simp only [broaderThanAll, Bool.not_eq_true', List.any_eq_false]
    intro p hp h
    rw [hb₀ p hp] at h
    cases h
  cases hf : child.find? (broaderThanAll parent) with
  | none => exact absurd hpred (by simpa using List.find?_eq_none.mp hf c₀ hc₀)
  | some c =>
    have hc := List.find?_some hf
    have hm := List.mem_of_find?_eq_some hf
    simp only [broaderThanAll, Bool.not_eq_true', List.any_eq_false] at hc
    refine ⟨c, hm, by simp [narrow, hf], fun h => by simp [nearestDimension, h, fewest], fun q hq => ?_⟩
    obtain ⟨w, hw, hfw, hmin⟩ := fewest_spec (parent.map c.widened) (by simp; exact List.ne_nil_of_mem hq)
    obtain ⟨p, hp, rfl⟩ := List.mem_map.mp hw
    have hne : c.widened p ≠ [] := by
      intro h
      exact hc p hp (by simp [h])
    obtain ⟨d, ds, hds⟩ := List.exists_cons_of_ne_nil hne
    refine ⟨p, hp, ?_, hmin _ (List.mem_map_of_mem hq)⟩
    simp only [nearestDimension, hfw, hds, List.mem_cons_self]

/-- Narrowing on every dimension, at derivation and along the chain: a child is admitted
exactly when each of its grants lies within one parent grant on every dimension and its expiry
within its parent's, and a chain admission checks hop by hop permits no request its root
grants refuse. -/
-- spec: authority.attenuate.narrowing@d61d3afa
theorem narrowing_every_dimension_every_hop :
    (∀ (parent : List Grant) (pe : Nat) (child : List Grant) (ce : Option Nat),
        (narrow parent pe child ce).verdict = "admitted" ↔
          (∀ c ∈ child, ∃ p ∈ parent, c.includedIn p = true) ∧ ∀ e, ce = some e → e ≤ pe) ∧
      ∀ (hops : List (List Grant)) (root gs : List Grant), Authority.checkChain root hops = some gs →
        ∀ r : Authority.Request, gs.any (fun g => g.permits r) = true → root.any (fun g => g.permits r) = true :=
  ⟨narrow_ok_iff_includedIn, Authority.checkChain_permits_root⟩

end Reference
