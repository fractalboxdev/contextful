import Protocol.Step

/-!
# The four safety invariants

Each theorem ranges over every node and every lease generation. The bounded check in
`Main.lean` evaluates the same four properties on every state three nodes and four lease
generations reach.
-/

namespace Protocol

/-- The inductive invariant: every belief and every in-flight fence is at most the highest
fence granted, no two nodes believe the same fence, and both guarded objects hold the
highest fence granted. -/
def Inv (s : State) : Prop :=
  (∀ n f, (s.node n).belief = some f → f ≤ s.granted) ∧
  (∀ n m f, (s.node n).belief = some f → (s.node m).belief = some f → n = m) ∧
  (∀ n t f, (s.node n).pending = some (t, f) → f ≤ s.granted) ∧
  s.catalogFence = s.granted ∧ s.cursorFence = s.granted

theorem inv_init : Inv State.init := by
  refine ⟨?_, ?_, ?_, rfl, rfl⟩ <;> intros <;> contradiction

theorem setNode_node (s : State) (n m : Node) (v : NodeState) :
    (s.setNode n v).node m = if m = n then v else s.node m := rfl

/-- A step that keeps the grant and both guarded fences, only drops beliefs, and puts in
flight only fences within the grant, keeps the invariant. -/
theorem inv_mono {s s' : State} (h : Inv s) (hg : s'.granted = s.granted)
    (hc : s'.catalogFence = s.catalogFence) (hcur : s'.cursorFence = s.cursorFence)
    (hb : ∀ m f, (s'.node m).belief = some f → (s.node m).belief = some f)
    (hp : ∀ m t f, (s'.node m).pending = some (t, f) → f ≤ s.granted) : Inv s' := by
  obtain ⟨h1, h2, _, h4, h5⟩ := h
  refine ⟨fun n f hf => hg ▸ h1 n f (hb n f hf), fun n m f hn hm => h2 n m f (hb n f hn) (hb m f hm),
    fun n t f hf => hg ▸ hp n t f hf, ?_, ?_⟩
  · rw [hc, hg]; exact h4
  · rw [hcur, hg]; exact h5

theorem grant_node_self (r : Bool) (s : State) (n : Node) (f : Nat) :
    (grant r s n f).node n = { s.node n with belief := some f } := by
  simp [grant, State.setNode]

theorem grant_node_other (r : Bool) (s : State) {n m : Node} (f : Nat) (h : m ≠ n) :
    (grant r s n f).node m = s.node m := by
  simp [grant, State.setNode, h]

/-- A grant one past the highest fence keeps the invariant. -/
theorem inv_grant {s : State} {n : Node} {f : Nat} (h : Inv s) (hf : f = s.granted + 1) :
    Inv (grant true s n f) := by
  obtain ⟨h1, h2, h3, h4, h5⟩ := h
  have below : ∀ m g, (s.node m).belief = some g → g < f := fun m g hm => by
    have := h1 m g hm; omega
  refine ⟨?_, ?_, ?_, ?_, ?_⟩
  · intro m g hm
    show g ≤ f
    by_cases e : m = n
    · subst e
      rw [grant_node_self] at hm
      have : f = g := Option.some.inj hm
      omega
    · rw [grant_node_other true s f e] at hm
      have := below m g hm
      omega
  · intro a b g ha hb
    by_cases ea : a = n <;> by_cases eb : b = n
    · rw [ea, eb]
    · subst ea
      rw [grant_node_self] at ha
      rw [grant_node_other true s f eb] at hb
      have e1 : f = g := Option.some.inj ha
      have := below b g hb
      omega
    · subst eb
      rw [grant_node_self] at hb
      rw [grant_node_other true s f ea] at ha
      have e1 : f = g := Option.some.inj hb
      have := below a g ha
      omega
    · rw [grant_node_other true s f ea] at ha
      rw [grant_node_other true s f eb] at hb
      exact h2 a b g ha hb
  · intro m t g hm
    show g ≤ f
    by_cases e : m = n
    · subst e
      rw [grant_node_self] at hm
      have := h3 m t g hm
      omega
    · rw [grant_node_other true s f e] at hm
      have := h3 m t g hm
      omega
  · show max s.catalogFence f = f
    omega
  · show max s.cursorFence f = f
    omega

theorem inv_setFence {s : State} {t : Target} {f : Nat} (h : Inv s) (hf : f = s.granted) :
    Inv (s.setFence t f) := by
  obtain ⟨h1, h2, h3, h4, h5⟩ := h
  cases t with
  | catalog => exact ⟨h1, h2, h3, hf, h5⟩
  | cursor => exact ⟨h1, h2, h3, h4, hf⟩

theorem granted_some {s : State} {l : Lease} (h : s.lease = some l) : s.granted = l.fence := by
  simp [State.granted, h]

theorem granted_none {s : State} (h : s.lease = none) : s.granted = 0 := by
  simp [State.granted, h]

theorem inv_step {s : State} (h : Inv s) (st : Step) : Inv (step s st) := by
  cases st with
  | acquire n =>
    simp only [step, stepWith]
    split
    · exact h
    · split
      · rename_i hl
        exact inv_grant h (by rw [granted_none hl])
      · rename_i l hl
        split
        · exact inv_grant h (by rw [granted_some hl])
        · exact h
  | renew n =>
    simp only [step, stepWith]
    split
    · rename_i l hl
      split
      · exact inv_mono h (by rw [granted_some hl]; rfl) rfl rfl (fun _ _ x => x) (fun m t f hf => h.2.2.1 m t f hf)
      · exact h
    · exact h
  | expire =>
    simp only [step, stepWith]
    split
    · rename_i l hl
      exact inv_mono h (by rw [granted_some hl]; rfl) rfl rfl (fun _ _ x => x) (fun m t f hf => h.2.2.1 m t f hf)
    · exact h
  | release n =>
    simp only [step, stepWith]
    split
    · rename_i l hl
      split
      · refine inv_mono h (by rw [granted_some hl]; rfl) rfl rfl ?_ ?_
        · intro m f hf
          by_cases e : m = n
          · subst e; simp [setNode_node] at hf
          · simpa [setNode_node, e] using hf
        · intro m t f hf
          by_cases e : m = n
          · subst e; simp only [setNode_node] at hf; exact h.2.2.1 m t f hf
          · simp only [setNode_node, if_neg e] at hf; exact h.2.2.1 m t f hf
      · exact h
    · exact h
  | send n t =>
    simp only [step, stepWith]
    split
    · rename_i f hbel _
      split
      · exact h
      · refine inv_mono h rfl rfl rfl ?_ ?_
        · intro m g hg
          by_cases e : m = n
          · subst e; simpa [setNode_node] using hg
          · simpa [setNode_node, e] using hg
        · intro m t' g hg
          by_cases e : m = n
          · subst e
            simp only [setNode_node] at hg
            obtain ⟨_, rfl⟩ := hg
            exact h.1 m f hbel
          · simp only [setNode_node, if_neg e] at hg; exact h.2.2.1 m t' g hg
    · exact h
  | deliver n =>
    simp only [step, stepWith]
    split
    · rename_i t f hpend
      have hmid : Inv (s.setNode n { s.node n with pending := none }) := by
        refine inv_mono h rfl rfl rfl ?_ ?_
        · intro m g hg
          by_cases e : m = n
          · subst e; simpa [setNode_node] using hg
          · simpa [setNode_node, e] using hg
        · intro m t' g hg
          by_cases e : m = n
          · subst e; simp [setNode_node] at hg
          · simp only [setNode_node, if_neg e] at hg; exact h.2.2.1 m t' g hg
      split
      · rename_i hle
        refine inv_setFence hmid ?_
        have hup := h.2.2.1 n t f hpend
        have hfen : s.fenceOf t = s.granted := by
          cases t
          · exact h.2.2.2.1
          · exact h.2.2.2.2
        show f = s.granted
        omega
      · exact hmid
    · exact h
  | pause n =>
    refine inv_mono h rfl rfl rfl ?_ ?_
    · intro m g hg
      by_cases e : m = n
      · subst e; simpa [step, stepWith, setNode_node] using hg
      · simpa [step, stepWith, setNode_node, e] using hg
    · intro m t g hg
      by_cases e : m = n
      · subst e; simp only [step, stepWith, setNode_node] at hg; exact h.2.2.1 m t g hg
      · simp only [step, stepWith, setNode_node, if_neg e] at hg; exact h.2.2.1 m t g hg
  | resume n =>
    refine inv_mono h rfl rfl rfl ?_ ?_
    · intro m g hg
      by_cases e : m = n
      · subst e; simpa [step, stepWith, setNode_node] using hg
      · simpa [step, stepWith, setNode_node, e] using hg
    · intro m t g hg
      by_cases e : m = n
      · subst e; simp only [step, stepWith, setNode_node] at hg; exact h.2.2.1 m t g hg
      · simp only [step, stepWith, setNode_node, if_neg e] at hg; exact h.2.2.1 m t g hg
  | crash n =>
    refine inv_mono h rfl rfl rfl ?_ ?_
    · intro m g hg
      by_cases e : m = n
      · subst e; simp [step, stepWith, setNode_node] at hg
      · simpa [step, stepWith, setNode_node, e] using hg
    · intro m t g hg
      by_cases e : m = n
      · subst e; simp only [step, stepWith, setNode_node] at hg; exact h.2.2.1 m t g hg
      · simp only [step, stepWith, setNode_node, if_neg e] at hg; exact h.2.2.1 m t g hg

theorem reachable_inv : ∀ {s : State}, Reachable s → Inv s := by
  intro s h
  induction h with
  | init => exact inv_init
  | step st _ ih => exact inv_step ih st

/-- One lease holder per fence: no two nodes of a reachable state believe they hold the same fence. -/
theorem one_holder_per_fence :
    ∀ {s : State}, Reachable s → ∀ (n m : Node) (fence : Nat),
      (s.node n).belief = some fence → (s.node m).belief = some fence → n = m :=
  fun h => (reachable_inv h).2.1

/-- Fences only increase: no step lowers the highest fence granted or the fence either
guarded object holds. -/
theorem fences_only_increase :
    ∀ (s : State) (st : Step),
      s.granted ≤ (step s st).granted ∧ s.catalogFence ≤ (step s st).catalogFence ∧
        s.cursorFence ≤ (step s st).cursorFence := by
  intro s st
  cases st with
  | acquire n =>
    simp only [step, stepWith]
    split
    · exact ⟨Nat.le_refl _, Nat.le_refl _, Nat.le_refl _⟩
    · split
      · rename_i hl
        refine ⟨?_, Nat.le_max_left _ _, Nat.le_max_left _ _⟩
        rw [granted_none hl]; exact Nat.zero_le _
      · rename_i l hl
        split
        · refine ⟨?_, Nat.le_max_left _ _, Nat.le_max_left _ _⟩
          rw [granted_some hl]; exact Nat.le_succ _
        · exact ⟨Nat.le_refl _, Nat.le_refl _, Nat.le_refl _⟩
  | renew n =>
    simp only [step, stepWith]
    split
    · rename_i l hl
      split
      · exact ⟨by rw [granted_some hl]; exact Nat.le_refl _, Nat.le_refl _, Nat.le_refl _⟩
      · exact ⟨Nat.le_refl _, Nat.le_refl _, Nat.le_refl _⟩
    · exact ⟨Nat.le_refl _, Nat.le_refl _, Nat.le_refl _⟩
  | expire =>
    simp only [step, stepWith]
    split
    · rename_i l hl
      exact ⟨by rw [granted_some hl]; exact Nat.le_refl _, Nat.le_refl _, Nat.le_refl _⟩
    · exact ⟨Nat.le_refl _, Nat.le_refl _, Nat.le_refl _⟩
  | release n =>
    simp only [step, stepWith]
    split
    · rename_i l hl
      split
      · exact ⟨by rw [granted_some hl]; exact Nat.le_refl _, Nat.le_refl _, Nat.le_refl _⟩
      · exact ⟨Nat.le_refl _, Nat.le_refl _, Nat.le_refl _⟩
    · exact ⟨Nat.le_refl _, Nat.le_refl _, Nat.le_refl _⟩
  | send n t =>
    simp only [step, stepWith]
    split
    · split
      · exact ⟨Nat.le_refl _, Nat.le_refl _, Nat.le_refl _⟩
      · exact ⟨Nat.le_refl _, Nat.le_refl _, Nat.le_refl _⟩
    · exact ⟨Nat.le_refl _, Nat.le_refl _, Nat.le_refl _⟩
  | deliver n =>
    simp only [step, stepWith]
    split
    · rename_i t f _
      split
      · rename_i hle
        cases t with
        | catalog => exact ⟨Nat.le_refl _, hle, Nat.le_refl _⟩
        | cursor => exact ⟨Nat.le_refl _, Nat.le_refl _, hle⟩
      · exact ⟨Nat.le_refl _, Nat.le_refl _, Nat.le_refl _⟩
    · exact ⟨Nat.le_refl _, Nat.le_refl _, Nat.le_refl _⟩
  | pause n => exact ⟨Nat.le_refl _, Nat.le_refl _, Nat.le_refl _⟩
  | resume n => exact ⟨Nat.le_refl _, Nat.le_refl _, Nat.le_refl _⟩
  | crash n => exact ⟨Nat.le_refl _, Nat.le_refl _, Nat.le_refl _⟩

/-- No commit lands carrying a fence below the highest granted: every write a step of a
reachable state lands carries a fence at least the lease object's. -/
-- spec: store.lease.stale-fence@5ca608ca
-- spec: store.lease.pointer-fence@037c9e49
theorem no_commit_below_granted :
    ∀ {s : State}, Reachable s → ∀ (st : Step) (t : Target) (f : Nat),
      landed s st = some (t, f) → s.granted ≤ f := by
  intro s hr st t f hl
  have h := reachable_inv hr
  cases st with
  | deliver n =>
    simp only [landed] at hl
    split at hl
    · rename_i t' f' _
      split at hl
      · rename_i hle
        simp only [Option.some.injEq, Prod.mk.injEq] at hl
        obtain ⟨rfl, rfl⟩ := hl
        have hfen : s.fenceOf t' = s.granted := by
          cases t'
          · exact h.2.2.2.1
          · exact h.2.2.2.2
        omega
      · cases hl
    · cases hl
  | _ => simp [landed] at hl

/-- Release keeps the lease object and its fence: after a release, the object exists exactly
when it did before, carrying the same fence. -/
theorem release_keeps_lease :
    ∀ (s : State) (n : Node), (step s (.release n)).lease.map Lease.fence = s.lease.map Lease.fence := by
  intro s n
  simp only [step, stepWith]
  split
  · split
    · rename_i hl _
      simp [hl]
    · rfl
  · rfl

/-- The four safety invariants over every reachable state: one lease holder per fence, fences
only increase, no commit lands carrying a fence below the highest granted, and release keeps
the lease object and its fence. -/
-- spec: assurance.model.protocol-safety@01e46fbe
theorem protocol_safety :
    ∀ {s : State}, Reachable s →
      (∀ (n m : Node) (fence : Nat), (s.node n).belief = some fence → (s.node m).belief = some fence → n = m) ∧
      (∀ st : Step, s.granted ≤ (step s st).granted ∧ s.catalogFence ≤ (step s st).catalogFence ∧
        s.cursorFence ≤ (step s st).cursorFence) ∧
      (∀ (st : Step) (t : Target) (f : Nat), landed s st = some (t, f) → s.granted ≤ f) ∧
      (∀ n : Node, (step s (.release n)).lease.map Lease.fence = s.lease.map Lease.fence) :=
  fun h => ⟨one_holder_per_fence h, fences_only_increase _, no_commit_below_granted h, release_keeps_lease _⟩

end Protocol
