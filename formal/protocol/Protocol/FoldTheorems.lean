import Protocol.Fold

/-!
# The fold theorems

Each theorem ranges over every state the fold step function reaches, or over every state.
The lease invariant enters as the commit guard: a pass whose fence a successor's
acquisition superseded commits nothing. The fold differential replays the same steps
against the store's `prepare`, `commit` and `collect`.
-/

namespace Protocol.Fold

/-- The inductive invariant: every published snapshot's directory exists, every reader
resolved a published snapshot, chain fences never rise from newest to oldest, and the
newest fence is at most the highest granted. -/
def Inv (s : State) : Prop :=
  (∀ e ∈ s.chain, s.final e.1 = true) ∧
  (∀ r id, s.readers r = some id → s.reachable id = true) ∧
  s.chain.Pairwise (fun a b => b.2 ≤ a.2) ∧
  (∀ e ∈ s.chain, e.2 ≤ s.granted) ∧
  (∀ e ∈ s.chain, s.staging e.1 = false ∧ e.1 < s.next) ∧
  (∀ p id v f, s.passes p = .staged id v f → id < s.next)

theorem reachable_of_mem {s : State} {e : Nat × Nat} (h : e ∈ s.chain) : s.reachable e.1 = true := by
  unfold State.reachable
  exact List.any_eq_true.mpr ⟨e, h, show decide (e.1 = e.1) = true from decide_eq_true rfl⟩

theorem current_reachable {s : State} {id : Nat} (h : s.current = some id) : s.reachable id = true := by
  unfold State.current at h
  cases hc : s.chain with
  | nil => simp [hc] at h
  | cons e es =>
    simp [hc] at h
    subst h
    exact reachable_of_mem (by rw [hc]; exact List.mem_cons_self)

theorem collectible_unreachable {s : State} {id : Nat} (h : s.reachable id = true) :
    s.collectible id = false := by
  unfold State.collectible
  cases s.current <;> simp [h]

theorem inv_init : Inv State.init := by
  refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩ <;> simp [State.init, State.reachable]

theorem inv_step {s : State} (h : Inv s) (st : Step) : Inv (step s st) := by
  obtain ⟨h1, h2, h3, h4, h5, h6⟩ := h
  cases st with
  | acquire =>
    exact ⟨h1, h2, h3, fun e he => Nat.le_succ_of_le (h4 e he), h5, h6⟩
  | stage p =>
    simp only [step]
    cases hp : s.passes p with
    | staged _ _ _ => exact ⟨h1, h2, h3, h4, h5, h6⟩
    | idle =>
      simp only
      have hpass : ∀ q id' v' f', (s.setPass p (.staged s.next s.version s.granted)).passes q = .staged id' v' f' →
          id' < s.next + 1 := by
        intro q id' v' f' hq
        simp only [State.setPass] at hq
        split at hq
        · cases hq; exact Nat.lt_succ_self _
        · exact Nat.lt_succ_of_lt (h6 q id' v' f' hq)
      refine ⟨h1, h2, h3, h4, fun e he => ?_, hpass⟩
      have ⟨hs, hn⟩ := h5 e he
      have hne : e.1 ≠ s.next := Nat.ne_of_lt hn
      simp only [hne, if_false, hs]
      exact ⟨trivial, Nat.lt_succ_of_lt hn⟩
  | commit p =>
    simp only [step]
    cases hp : s.passes p with
    | idle => exact ⟨h1, h2, h3, h4, h5, h6⟩
    | staged id v f =>
      simp only
      by_cases hf : f = s.granted
      · simp only [hf, if_true]
        have hfin : ∀ e ∈ s.chain, (if e.1 = id then true else s.final e.1) = true := by
          intro e he; split
          · rfl
          · exact h1 e he
        have hstg : ∀ e ∈ s.chain, ((if e.1 = id then false else s.staging e.1) = false ∧ e.1 < s.next) := by
          intro e he; have := h5 e he; split <;> simp_all
        by_cases hv : v = s.version
        · simp only [hv, if_true]
          have hp6 : ∀ q id' v' f', (s.setPass p .idle).passes q = .staged id' v' f' → id' < s.next := by
            intro q id' v' f' hq; simp only [State.setPass] at hq; split at hq
            · cases hq
            · exact h6 q id' v' f' hq
          refine ⟨?_, ?_, ?_, ?_, ?_, hp6⟩
          · intro e he
            simp only [List.mem_cons] at he
            rcases he with rfl | he
            · simp
            · exact hfin e he
          · intro r i hr
            have := h2 r i hr
            unfold State.reachable at *
            simp only [List.any_cons]
            simp [this]
          · refine List.Pairwise.cons (fun b hb => ?_) h3
            exact h4 b hb
          · intro e he
            simp only [List.mem_cons] at he
            rcases he with rfl | he
            · exact Nat.le_refl _
            · exact h4 e he
          · intro e he
            simp only [List.mem_cons] at he
            rcases he with rfl | he
            · exact ⟨by simp, h6 p id v f hp⟩
            · exact hstg e he
        · simp only [hv, if_false]
          refine ⟨hfin, h2, h3, h4, hstg, ?_⟩
          intro q id' v' f' hq; simp only [State.setPass] at hq; split at hq
          · cases hq
          · exact h6 q id' v' f' hq
      · simp only [hf, if_false]
        exact ⟨h1, h2, h3, h4, h5, h6⟩
  | abandon p =>
    refine ⟨h1, h2, h3, h4, h5, fun q id' v' f' hq => ?_⟩
    simp only [step, State.setPass] at hq; split at hq
    · cases hq
    · exact h6 q id' v' f' hq
  | collect =>
    refine ⟨fun e he => ?_, h2, h3, h4, fun e he => ?_, h6⟩
    · have he' : e ∈ s.chain := he
      have hc := collectible_unreachable (reachable_of_mem he')
      show (s.final e.1 && !s.collectible e.1) = true
      rw [h1 e he', hc]; rfl
    · have ⟨hs, hn⟩ := h5 e (show e ∈ s.chain from he)
      show (s.staging e.1 && !(s.collectible e.1 && !s.inFlight e.1)) = false ∧ e.1 < s.next
      rw [hs]; exact ⟨rfl, hn⟩
  | read r =>
    refine ⟨h1, fun q i hq => ?_, h3, h4, h5, h6⟩
    simp only [step] at hq
    split at hq
    · exact current_reachable hq
    · exact h2 q i hq
  | release r =>
    refine ⟨h1, fun q i hq => ?_, h3, h4, h5, h6⟩
    simp only [step] at hq
    split at hq
    · cases hq
    · exact h2 q i hq

theorem inv_reachable {s : State} (hs : Reachable s) : Inv s := by
  induction hs with
  | init => exact inv_init
  | step st _ ih => exact inv_step ih st

/-- A commit that moves the pointer read the version the pointer still holds. -/
-- spec: store.fold.pointer-commit@404387b7
theorem commit_conditioned_on_version (s : State) (p : Pass) (id v f : Nat)
    (hp : s.passes p = .staged id v f) (hm : (step s (.commit p)).version ≠ s.version) :
    v = s.version := by
  simp only [step, hp] at hm
  by_cases hf : f = s.granted
  · simp only [hf, if_true] at hm
    by_cases hv : v = s.version
    · exact hv
    · simp only [hv, if_false] at hm; exact absurd rfl hm
  · simp [hf] at hm

/-- A commit whose pass read an older version publishes nothing: the pointer, its version
and the chain stay. -/
-- spec: store.fold.lost-pointer@e9fa3f89
theorem lost_commit_publishes_nothing (s : State) (p : Pass) (id v f : Nat)
    (hp : s.passes p = .staged id v f) (hv : v ≠ s.version) :
    (step s (.commit p)).chain = s.chain ∧ (step s (.commit p)).version = s.version := by
  simp only [step, hp]
  by_cases hf : f = s.granted
  · simp only [hf, hv, if_true, if_false]; exact ⟨rfl, rfl⟩
  · simp only [hf, if_false]; exact ⟨trivial, trivial⟩

/-- After collection, no staging directory below the current snapshot survives unless a
pass holds it, and no snapshot directory below it survives unless a chain reaches it. -/
-- spec: store.fold.staging-collected@8832e10f
theorem collect_leaves_reachable {s : State} (hs : Reachable s) (id cur : Nat)
    (hc : s.current = some cur) (hlt : id < cur) :
    ((step s .collect).staging id = true → s.inFlight id = true) ∧
    ((step s .collect).final id = true → s.reachable id = true) := by
  have ⟨_, _, _, _, h5, _⟩ := inv_reachable hs
  have hcol : s.reachable id = false → s.collectible id = true := by
    intro hr; unfold State.collectible; rw [hc]; simp [hlt, hr]
  constructor
  · intro hst
    simp only [step] at hst
    cases hr : s.reachable id with
    | true =>
      obtain ⟨e, he, heq⟩ := List.any_eq_true.mp (show s.chain.any (fun e => e.1 == id) = true from hr)
      have hid : e.1 = id := by simpa using heq
      have := (h5 e he).1
      rw [hid] at this
      simp [this] at hst
    | false =>
      have := hcol hr
      cases hin : s.inFlight id <;> simp_all
  · intro hf
    cases hr : s.reachable id with
    | true => rfl
    | false =>
      simp only [step] at hf
      simp [hcol hr] at hf

/-- A commit that publishes keeps every earlier snapshot on the chain and every directory
in place. -/
-- spec: store.fold.supersedes@97cbcf3a
theorem commit_supersedes_without_deleting (s : State) (p : Pass) (id v f : Nat)
    (hp : s.passes p = .staged id v f) (hf : f = s.granted) (hv : v = s.version) :
    (step s (.commit p)).chain = (id, f) :: s.chain ∧
    ∀ i, s.final i = true → (step s (.commit p)).final i = true := by
  simp only [step, hp, hf, hv, if_true]
  refine ⟨trivial, fun i hi => ?_⟩
  show (if i = id then true else s.final i) = true
  split
  · rfl
  · exact hi

/-- A reader resolved a snapshot whose directory exists, whatever passes commit and
collection removes after. -/
-- spec: store.fold.non-blocking@05f96f28
theorem reader_snapshot_present {s : State} (hs : Reachable s) (r : Reader) (id : Nat)
    (h : s.readers r = some id) : s.final id = true := by
  have ⟨h1, h2, _, _, _, _⟩ := inv_reachable hs
  have := h2 r id h
  unfold State.reachable at this
  obtain ⟨e, he, heq⟩ := List.any_eq_true.mp this
  have : e.1 = id := by simpa using heq
  exact this ▸ h1 e he

/-- Under the lease invariant, the published chain's fences never rise from newest to
oldest, and none exceeds the highest granted. -/
-- spec: store.fold.compaction-lease@6b473e0b
theorem chain_fences_follow_the_lease {s : State} (hs : Reachable s) :
    s.chain.Pairwise (fun a b => b.2 ≤ a.2) ∧ ∀ e ∈ s.chain, e.2 ≤ s.granted := by
  have ⟨_, _, h3, h4, _, _⟩ := inv_reachable hs
  exact ⟨h3, h4⟩

end Protocol.Fold
