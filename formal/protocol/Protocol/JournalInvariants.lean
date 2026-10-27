import Protocol.Journal

/-!
# The journal invariants

Each theorem ranges over every state the journal step function reaches under the grace
hypothesis. The bounded check in `JournalCheck.lean` evaluates the same properties on every
state three callers reach on one key, and without the grace hypothesis prints the shortest
sequence to `BlobMissing`.
-/

namespace Protocol.Journal

/-- A caller running its effect holds the key's claim, or its lease lapsed, or a lapsed
holder already recorded the key. -/
def ClaimHeld (s : State) (c : Caller) (k : Key) : Prop :=
  (∃ a, s.rows k = some (.pending c a)) ∨ (s.callers c).lapsed = true ∨ ∃ v, s.rows k = some (.recorded v)

/-- What each phase guarantees about the rest of the state. -/
def CallerOk (s : State) (c : Caller) : Prop :=
  match (s.callers c).phase with
  | .takingOver _ h => (s.callers h).lapsed = true
  | .claimed k => ClaimHeld s c k
  | .checking _ v => v.origin = c
  | .storing _ v => v.origin = c
  | .recording _ v => v.origin = c ∧ (v.blob = true → s.blobs v.origin = true)
  | .loading k v => v.blob = true ∧ s.rows k = some (.recorded v)
  | .done k (.replayed v) => s.rows k = some (.recorded v)
  | .done k (.recorded v) => s.rows k = some (.recorded v)
  | .done _ .missing => False
  | _ => True

/-- What each row guarantees: the ghost record count, and a recorded blob's presence. -/
def RowOk (s : State) (k : Key) : Prop :=
  match s.rows k with
  | some (.recorded v) => s.records k = 1 ∧ (v.blob = true → s.blobs v.origin = true)
  | _ => s.records k = 0

/-- The inductive invariant. -/
def Inv (s : State) : Prop := (∀ c, CallerOk s c) ∧ ∀ k, RowOk s k

@[simp] theorem setPhase_rows (s : State) (c : Caller) (p : Phase) : (s.setPhase c p).rows = s.rows := rfl
@[simp] theorem setPhase_blobs (s : State) (c : Caller) (p : Phase) : (s.setPhase c p).blobs = s.blobs := rfl
@[simp] theorem setPhase_records (s : State) (c : Caller) (p : Phase) : (s.setPhase c p).records = s.records := rfl
@[simp] theorem setPhase_phase (s : State) (c d : Caller) (p : Phase) :
    ((s.setPhase c p).callers d).phase = if d = c then p else (s.callers d).phase := by
  simp only [State.setPhase]; split <;> simp_all
@[simp] theorem setPhase_lapsed (s : State) (c d : Caller) (p : Phase) :
    ((s.setPhase c p).callers d).lapsed = (s.callers d).lapsed := by
  simp only [State.setPhase]; split <;> simp_all
@[simp] theorem setRow_rows (s : State) (k j : Key) (r : Option Row) :
    (s.setRow k r).rows j = if j = k then r else s.rows j := rfl
@[simp] theorem setRow_blobs (s : State) (k : Key) (r : Option Row) : (s.setRow k r).blobs = s.blobs := rfl
@[simp] theorem setRow_records (s : State) (k : Key) (r : Option Row) : (s.setRow k r).records = s.records := rfl
@[simp] theorem setRow_callers (s : State) (k : Key) (r : Option Row) : (s.setRow k r).callers = s.callers := rfl

theorem inv_init : Inv State.init := by
  refine ⟨fun c => ?_, fun k => ?_⟩ <;> simp [CallerOk, RowOk, State.init]

/-- Another caller's guarantees survive a step that only moves `c`'s phase. -/
theorem callerOk_setPhase {s : State} {c d : Caller} (p : Phase) (hd : d ≠ c) (h : CallerOk s d) :
    CallerOk (s.setPhase c p) d := by
  unfold CallerOk ClaimHeld at *
  simp only [setPhase_phase, hd, if_false, setPhase_rows, setPhase_blobs, setPhase_lapsed]
  exact h

theorem rowOk_setPhase {s : State} {c : Caller} (p : Phase) {k : Key} (h : RowOk s k) :
    RowOk (s.setPhase c p) k := by
  unfold RowOk at *; simpa using h

/-- Another caller's guarantees survive a write of key `k`'s row that overwrites no recorded
value, and leaves a pending claim of `d` alone unless `d` lapsed or the write records. -/
theorem callerOk_setRow {s : State} {c d : Caller} {k : Key} {r : Option Row} (p : Phase)
    (hd : d ≠ c) (h : CallerOk s d) (hrec : ∀ v, s.rows k ≠ some (.recorded v))
    (hpend : ∀ a, s.rows k = some (.pending d a) →
      r = s.rows k ∨ (s.callers d).lapsed = true ∨ ∃ v, r = some (.recorded v)) :
    CallerOk ((s.setRow k r).setPhase c p) d := by
  unfold CallerOk ClaimHeld at *
  simp only [setPhase_phase, hd, if_false, setPhase_rows, setPhase_blobs, setPhase_lapsed,
    setRow_rows, setRow_blobs, setRow_callers]
  cases hp : (s.callers d).phase <;> simp only [hp] at h ⊢
  case claimed j =>
    by_cases e : j = k
    · subst e
      simp only [if_true]
      rcases h with ⟨a, ha⟩ | hl | ⟨v, hv⟩
      · rcases hpend a ha with hr | hl | ⟨v, hv⟩
        · exact .inl ⟨a, hr ▸ ha⟩
        · exact .inr (.inl hl)
        · exact .inr (.inr ⟨v, hv⟩)
      · exact .inr (.inl hl)
      · exact absurd hv (hrec v)
    · simp only [e, if_false]; exact h
  case loading j v =>
    by_cases e : j = k
    · subst e; exact absurd h.2 (hrec v)
    · simp only [e, if_false]; exact h
  case done j res =>
    cases res <;> simp only at h ⊢
    all_goals first
      | exact h
      | (rename_i v; by_cases e : j = k
         · subst e; exact absurd h (hrec v)
         · simp only [e, if_false]; exact h)
  all_goals exact h

theorem inv_setPhase {s : State} {c : Caller} {p : Phase} (h : Inv s)
    (hc : CallerOk (s.setPhase c p) c) : Inv (s.setPhase c p) :=
  ⟨fun d => if e : d = c then e ▸ hc else callerOk_setPhase p e (h.1 d),
   fun k => rowOk_setPhase p (h.2 k)⟩

theorem inv_setRow {s : State} {c : Caller} {k : Key} {r : Option Row} {p : Phase} (h : Inv s)
    (hrec : ∀ v, s.rows k ≠ some (.recorded v))
    (hpend : ∀ d, d ≠ c → ∀ a, s.rows k = some (.pending d a) →
      r = s.rows k ∨ (s.callers d).lapsed = true ∨ ∃ v, r = some (.recorded v))
    (hc : CallerOk ((s.setRow k r).setPhase c p) c) (hk : RowOk ((s.setRow k r).setPhase c p) k) :
    Inv ((s.setRow k r).setPhase c p) := by
  refine ⟨fun d => if e : d = c then e ▸ hc else callerOk_setRow p e (h.1 d) hrec (hpend d e), fun j => ?_⟩
  by_cases e : j = k
  · exact e ▸ hk
  · have := h.2 j
    unfold RowOk at *
    simpa [e] using this

/-- The phase a caller reaches reading recorded value `v` keeps its guarantees. -/
theorem callerOk_replay {s : State} {c : Caller} {k : Key} {v : Value}
    (hr : s.rows k = some (.recorded v)) : CallerOk (s.setPhase c (replay k v)) c := by
  unfold CallerOk replay
  cases hb : v.blob <;> simp [hb, hr]

theorem not_recorded_of_pending {s : State} {k : Key} {h : Caller} {a : Nat}
    (hr : s.rows k = some (.pending h a)) : ∀ v, s.rows k ≠ some (.recorded v) := by
  intro v e; rw [hr] at e; cases e

theorem not_recorded_of_none {s : State} {k : Key} (hr : s.rows k = none) :
    ∀ v, s.rows k ≠ some (.recorded v) := by
  intro v e; rw [hr] at e; cases e

theorem rowOk_pending {s : State} {k : Key} {c : Caller} {a : Nat} {p : Phase} {old : Option Row}
    (hold : s.rows k = old) (hno : ∀ v, old ≠ some (.recorded v)) (h : RowOk s k) :
    RowOk ((s.setRow k (some (.pending c a))).setPhase c p) k := by
  unfold RowOk at *
  rw [hold] at h
  cases old with
  | none => simpa using h
  | some r => cases r with
    | pending _ _ => simpa using h
    | recorded v => exact absurd rfl (hno v)

theorem rowOk_none {s : State} {k : Key} {c : Caller} {p : Phase} {old : Option Row}
    (hold : s.rows k = old) (hno : ∀ v, old ≠ some (.recorded v)) (h : RowOk s k) :
    RowOk ((s.setRow k none).setPhase c p) k := by
  unfold RowOk at *
  rw [hold] at h
  cases old with
  | none => simpa using h
  | some r => cases r with
    | pending _ _ => simpa using h
    | recorded v => exact absurd rfl (hno v)

/-- Storing more blobs keeps every guarantee. -/
theorem inv_blobs_grow {s : State} {f : Caller → Bool} (h : Inv s)
    (hg : ∀ b, s.blobs b = true → f b = true) : Inv { s with blobs := f } := by
  refine ⟨fun d => ?_, fun k => ?_⟩
  · have hd := h.1 d
    unfold CallerOk ClaimHeld at *
    cases hp : (s.callers d).phase <;> simp only [hp] at hd ⊢
    case recording j v => exact ⟨hd.1, fun hb => hg _ (hd.2 hb)⟩
    case done j r => cases r <;> simp only at hd ⊢ <;> exact hd
    all_goals exact hd
  · have hk := h.2 k
    unfold RowOk at *
    cases hr : s.rows k <;> simp only [hr] at hk ⊢
    case none => exact hk
    case some r =>
      cases r <;> simp only at hk ⊢
      case pending => exact hk
      case recorded v => exact ⟨hk.1, fun hb => hg _ (hk.2 hb)⟩

theorem inv_record {s : State} {c : Caller} {k : Key} {v : Value} {old : Option Row} (h : Inv s)
    (hc : v.origin = c ∧ (v.blob = true → s.blobs v.origin = true)) (hr : s.rows k = old)
    (hno : ∀ w, old ≠ some (.recorded w)) :
    Inv { (s.setRow k (some (.recorded v))).setPhase c (.done k (.recorded v)) with
      records := fun j => if j = k then s.records k + 1 else s.records j } := by
  refine ⟨fun d => ?_, fun j => ?_⟩
  · by_cases e : d = c
    · subst e; unfold CallerOk; simp
    · exact callerOk_setRow (.done k (.recorded v)) e (h.1 d) (hr ▸ hno) (fun _ _ => .inr (.inr ⟨v, rfl⟩))
  · have hj := h.2 j
    unfold RowOk at *
    by_cases e : j = k
    · subst e
      rw [hr] at hj
      have h0 : s.records j = 0 := by
        cases old with
        | none => exact hj
        | some r => cases r with
          | pending _ _ => exact hj
          | recorded w => exact absurd rfl (hno w)
      simp only [if_true, setPhase_rows, setRow_rows, setPhase_blobs, setRow_blobs, h0]
      exact ⟨trivial, hc.2⟩
    · simpa [e] using hj

theorem inv_advance {s : State} (h : Inv s) (c : Caller) : Inv (advance s c) := by
  have hc := h.1 c
  unfold CallerOk at hc
  unfold advance
  cases hp : (s.callers c).phase <;> simp only [hp] at hc ⊢
  case idle => exact h
  case claimed k => exact h
  case done k r => exact h
  case claiming k =>
    cases hr : s.rows k with
    | none =>
      refine inv_setRow h (not_recorded_of_none hr) (fun d _ a e => by rw [hr] at e; cases e) ?_
        (rowOk_pending hr (by intro v e; cases e) (h.2 k))
      unfold CallerOk ClaimHeld; simp
    | some r => exact inv_setPhase h (by unfold CallerOk; simp)
  case reading k =>
    cases hr : s.rows k with
    | none => exact inv_setPhase h (by unfold CallerOk; simp)
    | some r =>
      cases r with
      | recorded v => exact inv_setPhase h (callerOk_replay hr)
      | pending hd a =>
        simp only
        split
        · rename_i e; subst e
          exact inv_setPhase h (by unfold CallerOk ClaimHeld; simp [hr])
        · split
          · rename_i hl
            exact inv_setPhase h (by unfold CallerOk; simp [hl])
          · exact inv_setPhase h (by unfold CallerOk; simp)
  case takingOver k hh =>
    cases hr : s.rows k with
    | none => exact inv_setPhase h (by unfold CallerOk; simp)
    | some r =>
      cases r with
      | recorded v => exact inv_setPhase h (by unfold CallerOk; simp)
      | pending h' a =>
        simp only
        split
        · rename_i e; subst e
          refine inv_setRow h (not_recorded_of_pending hr) ?_ ?_
            (rowOk_pending hr (by intro v e; cases e) (h.2 k))
          · intro d _ a' e
            rw [hr] at e
            cases e
            exact .inr (.inl hc)
          · unfold CallerOk ClaimHeld; simp
        · exact inv_setPhase h (by unfold CallerOk; simp)
  case checking k v =>
    cases hr : s.rows k with
    | none =>
      simp only
      exact inv_setPhase h (by unfold CallerOk; cases hb : v.blob <;> simp [hb, hc])
    | some r =>
      cases r with
      | recorded w => exact inv_setPhase h (callerOk_replay hr)
      | pending _ _ =>
        simp only
        exact inv_setPhase h (by unfold CallerOk; cases hb : v.blob <;> simp [hb, hc])
  case storing k v =>
    have hg : Inv { s with blobs := fun b => if b = v.origin then true else s.blobs b } :=
      inv_blobs_grow h (fun b hb => by split <;> simp_all)
    have e : { s.setPhase c (.recording k v) with blobs := fun b => if b = v.origin then true else s.blobs b }
        = ({ s with blobs := fun b => if b = v.origin then true else s.blobs b }).setPhase c (.recording k v) := rfl
    rw [e]
    exact inv_setPhase hg (by unfold CallerOk; simp [hc])
  case recording k v =>
    cases hr : s.rows k with
    | some r =>
      cases r with
      | recorded w => exact inv_setPhase h (callerOk_replay hr)
      | pending hd a => exact inv_record h hc hr (by intro w e; cases e)
    | none => exact inv_record h hc hr (by intro w e; cases e)
  case releasing k r =>
    cases r <;> simp only <;>
    · cases hr : s.rows k with
      | none => exact inv_setPhase h (by unfold CallerOk; simp)
      | some row =>
        cases row with
        | recorded w => exact inv_setPhase h (by unfold CallerOk; simp)
        | pending hd a =>
          simp only
          split
          · rename_i e; subst e
            exact inv_setRow h (not_recorded_of_pending hr)
              (fun d hd' a' e => by rw [hr] at e; cases e; exact absurd rfl hd')
              (by unfold CallerOk; simp) (rowOk_none hr (by intro v e; cases e) (h.2 k))
          · exact inv_setPhase h (by unfold CallerOk; simp)
  case loading k v =>
    have hk := h.2 k
    unfold RowOk at hk
    rw [hc.2] at hk
    simp only at hk
    have hb : s.blobs v.origin = true := hk.2 hc.1
    exact inv_setPhase h (by unfold CallerOk; simp [hb, hc.2])

/-- A row write that overwrites no recorded value and leaves another caller's pending claim
alone keeps that caller's guarantees. -/
theorem callerOk_setRowOnly {s : State} {d : Caller} {k : Key} {r : Option Row}
    (h : CallerOk s d) (hrec : ∀ v, s.rows k ≠ some (.recorded v))
    (hpend : ∀ a, s.rows k ≠ some (.pending d a)) : CallerOk (s.setRow k r) d := by
  unfold CallerOk ClaimHeld at *
  simp only [setRow_rows, setRow_blobs, setRow_callers]
  cases hp : (s.callers d).phase <;> simp only [hp] at h ⊢
  case claimed j =>
    by_cases e : j = k
    · subst e
      rcases h with ⟨a, ha⟩ | hl | ⟨v, hv⟩
      · exact absurd ha (hpend a)
      · exact .inr (.inl hl)
      · exact absurd hv (hrec v)
    · simp only [e, if_false]; exact h
  case loading j v =>
    by_cases e : j = k
    · subst e; exact absurd h.2 (hrec v)
    · simp only [e, if_false]; exact h
  case done j res =>
    cases res <;> simp only at h ⊢
    all_goals first
      | exact h
      | (rename_i v; by_cases e : j = k
         · subst e; exact absurd h (hrec v)
         · simp only [e, if_false]; exact h)
  all_goals exact h

theorem inv_note {s : State} (h : Inv s) (c : Caller) : Inv (step s (.note c)) := by
  have hc := h.1 c
  unfold CallerOk at hc
  simp only [step, stepWith]
  cases hp : (s.callers c).phase <;> simp only [hp] at hc ⊢ <;> try exact h
  case claimed k =>
    have pend : ∀ {n : Nat}, ((∃ a, s.rows k = some (.pending c a)) ∨ s.rows k = none) →
        Inv (s.setRow k (some (.pending c n))) := by
      intro n hold
      refine ⟨fun d => ?_, fun j => ?_⟩
      · by_cases e : d = c
        · subst e; unfold CallerOk ClaimHeld; simp [hp]
        · refine callerOk_setRowOnly (h.1 d) (fun v e' => ?_) (fun a e' => ?_)
          · rcases hold with ⟨_, h1⟩ | h1 <;> rw [h1] at e' <;> cases e'
          · rcases hold with ⟨_, h1⟩ | h1 <;> rw [h1] at e' <;> cases e'; exact e rfl
      · have hj := h.2 j
        unfold RowOk at *
        by_cases e : j = k
        · subst e
          rcases hold with ⟨_, h1⟩ | h1 <;> rw [h1] at hj <;> simpa using hj
        · simpa [e] using hj
    cases hr : s.rows k with
    | none =>
      simp only
      split
      · exact pend (.inr hr)
      · exact h
    | some row =>
      cases row with
      | recorded w => simp only; split <;> exact h
      | pending hd a =>
        simp only
        by_cases e : hd = c
        · subst e
          simp only [if_true]
          split
          · exact pend (.inl ⟨a, hr⟩)
          · exact h
        · simp only [e, if_false]
          split <;> exact h

theorem inv_lapse {s : State} (h : Inv s) (c : Caller) : Inv (step s (.lapse c)) := by
  simp only [step, stepWith]
  have hph : ∀ d, ((if d = c then { s.callers c with lapsed := true } else s.callers d) : CallerState).phase
      = (s.callers d).phase := fun d => by split <;> simp_all
  have hl : ∀ d, (s.callers d).lapsed = true →
      ((if d = c then { s.callers c with lapsed := true } else s.callers d) : CallerState).lapsed = true :=
    fun d hd => by split <;> simp_all
  refine ⟨fun d => ?_, fun k => h.2 k⟩
  have hd := h.1 d
  unfold CallerOk ClaimHeld at *
  simp only [hph]
  cases hp : (s.callers d).phase <;> simp only [hp] at hd ⊢
  case takingOver j hh => exact hl hh hd
  case claimed j =>
    rcases hd with ha | hla | hv
    · exact .inl ha
    · exact .inr (.inl (hl d hla))
    · exact .inr (.inr hv)
  case done j res => cases res <;> simp only at hd ⊢ <;> exact hd
  all_goals exact hd

theorem referenced_of_row {s : State} {k : Key} {b : Caller}
    (hr : s.rows k = some (.recorded ⟨b, true⟩)) : s.referenced b = true := by
  unfold State.referenced
  rw [List.any_eq_true]
  exact ⟨k, List.mem_finRange k, by simp [hr]⟩

theorem inv_sweep {s : State} (h : Inv s) (aged : Bool) : Inv (step s (.sweep aged)) := by
  simp only [step, stepWith]
  split
  · refine ⟨fun d => ?_, fun k => ?_⟩
    · have hd := h.1 d
      unfold CallerOk ClaimHeld at *
      cases hp : (s.callers d).phase <;> simp only [hp] at hd ⊢
      case recording j v =>
        refine ⟨hd.1, fun hb => ?_⟩
        have hin : s.inFlight v.origin = true := by
          unfold State.inFlight; rw [hd.1, hp]
          show (v.blob && decide (v.origin = d)) = true
          rw [hb, hd.1]; exact decide_eq_true rfl
        rw [hd.2 hb, hin]
        cases s.referenced v.origin <;> rfl
      case done j res => cases res <;> simp only at hd ⊢ <;> exact hd
      all_goals exact hd
    · have hk := h.2 k
      unfold RowOk at *
      cases hr : s.rows k <;> simp only [hr] at hk ⊢
      case none => exact hk
      case some r =>
        cases r <;> simp only at hk ⊢
        case pending => exact hk
        case recorded v =>
          refine ⟨hk.1, fun hb => ?_⟩
          have : s.rows k = some (.recorded ⟨v.origin, true⟩) := by rw [hr, ← hb]
          rw [hk.2 hb, referenced_of_row this]
          rfl
  · exact h

theorem key_free {s : State} {k : Key}
    (hg : ((List.finRange 3).all fun c => (s.callers c).phase.key != some k) = true) (d : Caller) :
    (s.callers d).phase.key ≠ some k := by
  rw [List.all_eq_true] at hg
  have := hg d (List.mem_finRange d)
  simpa using this

theorem inv_retire {s : State} (h : Inv s) (k : Key) : Inv (step s (.retire k)) := by
  simp only [step, stepWith]
  split
  · rename_i hg
    refine ⟨fun d => ?_, fun j => ?_⟩
    · have hd := h.1 d
      have hfree := key_free hg d
      unfold CallerOk ClaimHeld at *
      simp only [setRow_rows, setRow_blobs, setRow_callers]
      cases hp : (s.callers d).phase <;> simp only [hp, Phase.key] at hd hfree ⊢
      case claimed j =>
        have e : j ≠ k := fun e => hfree (e ▸ rfl)
        simp only [e, if_false]; exact hd
      case loading j v =>
        have e : j ≠ k := fun e => hfree (e ▸ rfl)
        simp only [e, if_false]; exact hd
      case done j res =>
        have e : j ≠ k := fun e => hfree (e ▸ rfl)
        cases res <;> simp only [e, if_false] at hd ⊢ <;> exact hd
      all_goals exact hd
    · have hj := h.2 j
      unfold RowOk at *
      by_cases e : j = k
      · subst e; simp
      · simpa [e] using hj
  · exact h

theorem inv_step {s : State} (h : Inv s) (st : Step) : Inv (step s st) := by
  cases st with
  | begin c k =>
    simp only [step, stepWith]
    split
    · exact inv_setPhase h (by unfold CallerOk; simp)
    · exact h
  | advance c => exact inv_advance h c
  | effect c o =>
    simp only [step, stepWith]
    cases hp : (s.callers c).phase <;> simp only <;> try exact h
    case claimed k => exact inv_setPhase h (by unfold CallerOk; cases o <;> simp)
  | note c => exact inv_note h c
  | lapse c => exact inv_lapse h c
  | crash c => exact inv_setPhase h (by unfold CallerOk; simp)
  | finish c =>
    simp only [step, stepWith]
    cases hp : (s.callers c).phase <;> simp only <;> try exact h
    case done k r => exact inv_setPhase h (by unfold CallerOk; simp)
  | sweep aged => exact inv_sweep h aged
  | retire k => exact inv_retire h k

theorem inv_reachable {s : State} (hs : Reachable s) : Inv s := by
  induction hs with
  | init => exact inv_init
  | step st _ ih => exact inv_step ih st

/-- A recorded value stays recorded under every step but the retirement of its execution. -/
theorem recorded_stable (s : State) (st : Step) (k : Key) (v : Value)
    (h : s.rows k = some (.recorded v)) :
    (step s st).rows k = some (.recorded v) ∨ st = .retire k := by
  cases st
  case retire j =>
    by_cases e : j = k
    · exact .inr (e ▸ rfl)
    · left
      simp only [step, stepWith]
      split
      · simp [State.setRow, Ne.symm e, h]
      · exact h
  all_goals
    left
    simp only [step, stepWith, advance]
    (repeat' split) <;> simp_all [State.setRow, State.setPhase] <;> intro e <;> subst e <;> simp_all

/-- Two callers resolving one key from the journal hold the same value. -/
-- spec: run.journal.entry-key@62b1a12d
theorem resolutions_agree {s : State} (hs : Reachable s) (c d : Caller) (k : Key) (v w : Value)
    (hc : (s.callers c).phase = .done k (.replayed v) ∨ (s.callers c).phase = .done k (.recorded v))
    (hd : (s.callers d).phase = .done k (.replayed w) ∨ (s.callers d).phase = .done k (.recorded w)) :
    v = w := by
  have h := inv_reachable hs
  have rc : s.rows k = some (.recorded v) := by
    have := h.1 c; unfold CallerOk at this; rcases hc with e | e <;> rw [e] at this <;> exact this
  have rd : s.rows k = some (.recorded w) := by
    have := h.1 d; unfold CallerOk at this; rcases hd with e | e <;> rw [e] at this <;> exact this
  rw [rc] at rd
  cases rd
  rfl

/-- At most one record lands on a key between two retirements of its execution. -/
-- spec: run.journal.step-output@78bc23e4
theorem one_record_per_key {s : State} (hs : Reachable s) (k : Key) : s.records k ≤ 1 := by
  have := (inv_reachable hs).2 k
  unfold RowOk at this
  split at this
  · omega
  · omega

/-- A caller runs its effect holding the key's claim, after its own lease lapsed, or after
a lapsed holder recorded the key. -/
theorem effect_only_when_claimed {s : State} (hs : Reachable s) (c : Caller) (k : Key)
    (h : (s.callers c).phase = .claimed k) :
    (∃ a, s.rows k = some (.pending c a)) ∨ (s.callers c).lapsed = true ∨
      ∃ v, s.rows k = some (.recorded v) := by
  have := (inv_reachable hs).1 c
  unfold CallerOk at this
  rw [h] at this
  exact this

/-- A recorded value past the inline cutoff names a stored blob. -/
-- spec: run.journal.blob-sweep@a62b9f09
theorem no_dangling_blob {s : State} (hs : Reachable s) (k : Key) (v : Value)
    (h : s.rows k = some (.recorded v)) (hb : v.blob = true) : s.blobs v.origin = true := by
  have := (inv_reachable hs).2 k
  unfold RowOk at this
  rw [h] at this
  exact this.2 hb

/-- A caller takes a claim over only from a holder whose lease lapsed. -/
-- spec: run.journal.claim-takeover@9f58eef0
theorem takeover_only_from_lapsed {s : State} (hs : Reachable s) (c h : Caller) (k : Key)
    (e : (s.callers c).phase = .takingOver k h) : (s.callers h).lapsed = true := by
  have := (inv_reachable hs).1 c
  unfold CallerOk at this
  rw [e] at this
  exact this

/-- No caller reads `BlobMissing`. -/
theorem no_blob_missing {s : State} (hs : Reachable s) (c : Caller) (k : Key) :
    (s.callers c).phase ≠ .done k .missing := by
  intro e
  have := (inv_reachable hs).1 c
  unfold CallerOk at this
  rw [e] at this
  exact this

end Protocol.Journal
