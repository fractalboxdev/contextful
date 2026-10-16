import Contextful.Layer

/-!
# Statement execution

The execution relation the mediation induction ranges over: the read path's statement
execution under one session. A state holds the rows the statement has still to scan and
the rows it has released. One step takes the next pending row, evaluates the session's
fold over it, and releases the row when the fold admits it or drops it otherwise. No
step releases a row by any other route, so every function releasing a stored row takes
the session's admission value.

Engine run steps lie outside this relation: a run step that reads stored rows does so
through a read-path statement, and its effects answer to the protocol model's fences
and the allowlist inclusion theorem.
-/

namespace Mediation

set_option genSizeOfSpec false
set_option genInjectivity false

set_option genCtorIdx false in
/-- A session: the layers its admission value carries, in the single order the relation fixes. -/
structure Session where
  layers : List Layer

set_option genCtorIdx false in
/-- A statement execution's state: the rows still to scan and the rows released so far. -/
structure State where
  pending : List RowId
  released : List RowId

/-- The state a statement starts in: every candidate row pending, none released. -/
def start (rows : List RowId) : State :=
  ⟨rows, []⟩

/-- One step of statement execution under a session: the next pending row is released
when the session's fold admits it, and dropped when the fold refuses it. -/
inductive Step (s : Session) : State → State → Prop where
  | release {r : RowId} {rs out : List RowId} :
      composed s.layers r = true → Step s ⟨r :: rs, out⟩ ⟨rs, out ++ [r]⟩
  | drop {r : RowId} {rs out : List RowId} :
      composed s.layers r = false → Step s ⟨r :: rs, out⟩ ⟨rs, out⟩

/-- The reflexive-transitive closure of `Step`: the states a statement execution reaches. -/
inductive Reach (s : Session) : State → State → Prop where
  | refl (σ : State) : Reach s σ σ
  | tail {σ τ υ : State} : Reach s σ τ → Step s τ υ → Reach s σ υ

/-- Every row a state holds as released passed every layer of the session. -/
def Released (s : Session) (σ : State) : Prop :=
  ∀ r, r ∈ σ.released → ∀ l, l ∈ s.layers → l r = true

theorem step_keeps_released {s : Session} {τ υ : State} :
    Step s τ υ → Released s τ → Released s υ := by
  intro hstep hτ
  cases hstep with
  | release hadm =>
    intro x hx l hl
    rcases List.mem_append.mp hx with hx | hx
    · exact hτ x hx l hl
    · cases List.mem_singleton.mp hx
      exact composed_sound hadm l hl
  | drop _ => exact hτ

theorem reach_keeps_released {s : Session} {σ τ : State} :
    Reach s σ τ → Released s σ → Released s τ := by
  intro h hσ
  induction h with
  | refl => exact hσ
  | tail _ hstep ih => exact step_keeps_released hstep ih

/-- Mediation over statement execution: in every state reachable from a statement's start,
each released row passed every layer of the session. -/
-- spec: assurance.prove.mediation-relation@23fee7b2
theorem statement_reach_released_rows_pass_session_layers :
    ∀ (s : Session) (rows : List RowId) (σ : State),
      Reach s (start rows) σ → ∀ (r : RowId), r ∈ σ.released → ∀ (l : Layer), l ∈ s.layers → l r = true := by
  intro s rows σ h
  exact reach_keeps_released h (fun _ hx => nomatch hx)

end Mediation
