import Protocol.Step

/-!
# The five safety invariants

Each theorem ranges over both lease paths, every node and every lease generation. The
bounded check in `Main.lean` evaluates the same five properties on every state three nodes
and four lease generations reach on each path.
-/

namespace Protocol

/-- The inductive invariant: every belief and every in-flight fence is at most the highest
fence granted, no two nodes believe the same fence, the guarded object's fence is at most the
highest granted and at least every completed acquisition's, and its head is at most its
fence. -/
structure Inv (s : State) : Prop where
  belief_le : ∀ n f, (s.node n).belief = some f → f ≤ s.granted
  unique : ∀ n m f, (s.node n).belief = some f → (s.node m).belief = some f → n = m
  pending_le : ∀ n f, (s.node n).pending = some f → f ≤ s.granted
  fence_le : s.fence ≤ s.granted
  raised_le : ∀ n f, (s.node n).raised = true → (s.node n).belief = some f → f ≤ s.fence
  head_le : s.head ≤ s.fence

theorem inv_init : Inv State.init where
  belief_le := by intros; contradiction
  unique := by intros; contradiction
  pending_le := by intros; contradiction
  fence_le := Nat.le_refl _
  raised_le := by intros; contradiction
  head_le := Nat.le_refl _

theorem setNode_node (s : State) (n m : Node) (v : NodeState) :
    (s.setNode n v).node m = if m = n then v else s.node m := rfl

@[simp] theorem setNode_lease (s : State) (n : Node) (v : NodeState) : (s.setNode n v).lease = s.lease := rfl
@[simp] theorem setNode_fence (s : State) (n : Node) (v : NodeState) : (s.setNode n v).fence = s.fence := rfl
@[simp] theorem setNode_head (s : State) (n : Node) (v : NodeState) : (s.setNode n v).head = s.head := rfl
@[simp] theorem setNode_granted (s : State) (n : Node) (v : NodeState) : (s.setNode n v).granted = s.granted := rfl

theorem granted_some {s : State} {l : Lease} (h : s.lease = some l) : s.granted = l.fence := by
  simp [State.granted, h]

theorem granted_none {s : State} (h : s.lease = none) : s.granted = 0 := by
  simp [State.granted, h]

/-- Replacing one node's view keeps the invariant when the new view keeps or drops its
belief, puts in flight only a fence within the grant, and marks complete only an acquisition
within the guarded object's fence. -/
theorem inv_setNode {s : State} {n : Node} {v : NodeState} (h : Inv s)
    (hb : ∀ f, v.belief = some f → (s.node n).belief = some f)
    (hp : ∀ f, v.pending = some f → f ≤ s.granted)
    (hr : ∀ f, v.raised = true → v.belief = some f → f ≤ s.fence) : Inv (s.setNode n v) where
  belief_le := by
    intro m f hm
    rw [setNode_node] at hm
    by_cases e : m = n
    · rw [if_pos e] at hm; exact h.belief_le _ f (hb f hm)
    · rw [if_neg e] at hm; exact h.belief_le m f hm
  unique := by
    intro a b f ha hb'
    rw [setNode_node] at ha hb'
    by_cases ea : a = n <;> by_cases eb : b = n
    · rw [ea, eb]
    · rw [if_pos ea] at ha; rw [if_neg eb] at hb'
      rw [ea]; exact h.unique _ _ f (hb f ha) hb'
    · rw [if_neg ea] at ha; rw [if_pos eb] at hb'
      rw [eb]; exact h.unique _ _ f ha (hb f hb')
    · rw [if_neg ea] at ha; rw [if_neg eb] at hb'; exact h.unique _ _ f ha hb'
  pending_le := by
    intro m f hm
    rw [setNode_node] at hm
    by_cases e : m = n
    · rw [if_pos e] at hm; exact hp f hm
    · rw [if_neg e] at hm; exact h.pending_le m f hm
  fence_le := h.fence_le
  raised_le := by
    intro m f hr' hm
    rw [setNode_node] at hr' hm
    by_cases e : m = n
    · rw [if_pos e] at hr' hm; exact hr f hr' hm
    · rw [if_neg e] at hr' hm; exact h.raised_le m f hr' hm
  head_le := h.head_le

/-- Dropping a node's belief keeps the invariant. -/
theorem inv_abandon {s : State} (h : Inv s) (n : Node) : Inv (s.abandon n) :=
  inv_setNode h (by intro f hf; simp at hf) (fun f hf => h.pending_le n f hf) (by intro f _ hf; simp at hf)

/-- A change to the lease object alone that keeps its fence keeps the invariant. -/
theorem inv_lease {s : State} (l : Lease) (h : Inv s) (hl : s.lease = some l) (l' : Lease) (hf : l'.fence = l.fence) :
    Inv { s with lease := some l' } := by
  have hg : ({ s with lease := some l' } : State).granted = s.granted := by
    simp [State.granted, hl, hf]
  exact {
    belief_le := fun n f hn => hg ▸ h.belief_le n f hn
    unique := h.unique
    pending_le := fun n f hn => hg ▸ h.pending_le n f hn
    fence_le := hg ▸ h.fence_le
    raised_le := h.raised_le
    head_le := h.head_le }

/-- A grant one past the highest fence keeps the invariant. -/
theorem inv_grant {s : State} {n : Node} {f : Nat} (h : Inv s) (hf : f = s.granted + 1) :
    Inv (grant s n f) := by
  have below : ∀ m g, (s.node m).belief = some g → g < f := fun m g hm => by
    have := h.belief_le m g hm; omega
  have hg : (grant s n f).granted = f := rfl
  have node_eq : ∀ m, (grant s n f).node m = if m = n then { s.node n with belief := some f, raised := false } else s.node m :=
    fun m => rfl
  refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩
  · intro m g hm
    rw [hg]
    rw [node_eq] at hm
    split at hm
    · simp at hm; omega
    · have := below m g hm; omega
  · intro a b g ha hb
    rw [node_eq] at ha hb
    split at ha <;> split at hb
    · subst_vars; rfl
    · simp at ha; have := below b g hb; omega
    · simp at hb; have := below a g ha; omega
    · exact h.unique a b g ha hb
  · intro m g hm
    rw [hg]
    rw [node_eq] at hm
    split at hm
    · have := h.pending_le n g hm; omega
    · have := h.pending_le m g hm; omega
  · show s.fence ≤ f
    have := h.fence_le; omega
  · intro m g hr hm
    rw [node_eq] at hr hm
    split at hr
    · simp at hr
    · rw [if_neg (by assumption)] at hm; exact h.raised_le m g hr hm
  · exact h.head_le

/-- A raise of `n`'s belief `f`, at least the guarded object's fence, writes `f` into the
object and keeps the invariant. -/
theorem inv_raise {s : State} {n : Node} {f : Nat} (h : Inv s) (hb : (s.node n).belief = some f)
    (hge : ¬ f < s.fence) : Inv { s.setNode n { s.node n with raised := true } with fence := f } := by
  have h0 : Inv { s with fence := f } :=
    { belief_le := h.belief_le, unique := h.unique, pending_le := h.pending_le
      fence_le := h.belief_le n f hb
      raised_le := fun m g hr hm => by have := h.raised_le m g hr hm; show g ≤ f; omega
      head_le := by have := h.head_le; show s.head ≤ f; omega }
  exact inv_setNode (s := { s with fence := f }) h0 (fun g hg => hg) (fun g hg => h.pending_le n g hg)
    (fun g _ hg => by rw [hb] at hg; cases hg; exact Nat.le_refl _)

theorem inv_step {p : Path} {s : State} (h : Inv s) (st : Step) : Inv (step p s st) := by
  cases st with
  | grant n =>
    simp only [step, stepWith]
    split
    · exact h
    · split
      · rename_i hl; exact inv_grant h (by rw [granted_none hl])
      · rename_i l hl
        split
        · exact inv_grant h (by rw [granted_some hl])
        · exact h
  | raise n =>
    simp only [step, stepWith]
    split
    · rename_i f hb
      split
      · exact h
      · split
        · rename_i hlt
          cases p with
          | compaction =>
            exact inv_setNode h (fun g hg => hg) (fun g hg => h.pending_le n g hg)
              (fun g _ hg => by rw [hb] at hg; cases hg; omega)
          | run => exact inv_abandon h n
        · rename_i hge
          exact inv_raise h hb hge
    · exact h
  | renew n =>
    simp only [step, stepWith]
    split
    · rename_i l hl
      split
      · exact inv_lease l h hl _ rfl
      · exact h
    · exact h
  | expire =>
    simp only [step, stepWith]
    split
    · rename_i l hl; exact inv_lease l h hl _ rfl
    · exact h
  | release n =>
    simp only [step, stepWith]
    split
    · exact h
    · split
      · rename_i l hl
        split
        · exact inv_lease l (inv_abandon h n) (by simp [State.abandon, hl]) _ rfl
        · exact inv_abandon h n
      · exact inv_abandon h n
  | send n =>
    simp only [step, stepWith]
    split
    · rename_i f hb _
      split
      · exact h
      · have hpend : Inv (s.setNode n { s.node n with pending := some f }) :=
          inv_setNode h (fun g hg => hg) (fun g hg => by cases hg; exact h.belief_le n f hb)
            (fun g hr hg => h.raised_le n g hr hg)
        cases p with
        | compaction => exact hpend
        | run =>
          simp only
          split
          · exact hpend
          · split
            · rename_i l hl
              split
              · exact inv_lease l (inv_abandon h n) (by simp [State.abandon, hl]) _ rfl
              · exact inv_abandon h n
            · exact inv_abandon h n
    · exact h
  | deliver n =>
    simp only [step, stepWith]
    split
    · rename_i f hpend
      have hmid : Inv (s.setNode n { s.node n with pending := none }) :=
        inv_setNode h (fun g hg => hg) (by intro g hg; simp at hg) (fun g hr hg => h.raised_le n g hr hg)
      split
      · rename_i hle
        have hfg := h.pending_le n f hpend
        exact {
          belief_le := hmid.belief_le
          unique := hmid.unique
          pending_le := hmid.pending_le
          fence_le := by simpa using hfg
          raised_le := fun m g hr hm => by
            have := hmid.raised_le m g hr hm
            simp at this ⊢; omega
          head_le := Nat.le_refl _ }
      · exact hmid
    · exact h
  | pause n =>
    exact inv_setNode h (fun g hg => hg) (fun g hg => h.pending_le n g hg) (fun g hr hg => h.raised_le n g hr hg)
  | resume n =>
    exact inv_setNode h (fun g hg => hg) (fun g hg => h.pending_le n g hg) (fun g hr hg => h.raised_le n g hr hg)
  | crash n =>
    exact inv_setNode h (by intro f hf; simp at hf) (fun g hg => h.pending_le n g hg) (by intro f _ hf; simp at hf)

theorem reachable_inv {p : Path} : ∀ {s : State}, Reachable p s → Inv s := by
  intro s h
  induction h with
  | init => exact inv_init
  | step st _ ih => exact inv_step ih st

/-- One lease holder per fence: no two nodes of a reachable state believe they hold the same fence. -/
theorem one_holder_per_fence :
    ∀ {p : Path} {s : State}, Reachable p s → ∀ (n m : Node) (fence : Nat),
      (s.node n).belief = some fence → (s.node m).belief = some fence → n = m :=
  fun h => (reachable_inv h).unique

/-- Fences only increase: no step lowers the highest fence granted or the guarded object's fence. -/
theorem fences_only_increase :
    ∀ (p : Path) (s : State) (st : Step), s.granted ≤ (step p s st).granted ∧ s.fence ≤ (step p s st).fence := by
  intro p s st
  cases st with
  | grant n =>
    simp only [step, stepWith]
    split
    · exact ⟨Nat.le_refl _, Nat.le_refl _⟩
    · split
      · rename_i hl; exact ⟨by rw [granted_none hl]; exact Nat.zero_le _, Nat.le_refl _⟩
      · rename_i l hl
        split
        · exact ⟨by rw [granted_some hl]; exact Nat.le_succ _, Nat.le_refl _⟩
        · exact ⟨Nat.le_refl _, Nat.le_refl _⟩
  | raise n =>
    simp only [step, stepWith]
    split
    · split
      · exact ⟨Nat.le_refl _, Nat.le_refl _⟩
      · split
        · cases p <;> exact ⟨Nat.le_refl _, Nat.le_refl _⟩
        · rename_i hge; exact ⟨Nat.le_refl _, by simp; omega⟩
    · exact ⟨Nat.le_refl _, Nat.le_refl _⟩
  | renew n =>
    simp only [step, stepWith]
    split
    · rename_i l hl
      split
      · exact ⟨by simp [State.granted, hl], Nat.le_refl _⟩
      · exact ⟨Nat.le_refl _, Nat.le_refl _⟩
    · exact ⟨Nat.le_refl _, Nat.le_refl _⟩
  | expire =>
    simp only [step, stepWith]
    split
    · rename_i l hl; exact ⟨by simp [State.granted, hl], Nat.le_refl _⟩
    · exact ⟨Nat.le_refl _, Nat.le_refl _⟩
  | release n =>
    simp only [step, stepWith]
    split
    · exact ⟨Nat.le_refl _, Nat.le_refl _⟩
    · split
      · rename_i l hl
        split
        · exact ⟨by simp [State.granted, hl], Nat.le_refl _⟩
        · exact ⟨Nat.le_refl _, Nat.le_refl _⟩
      · exact ⟨Nat.le_refl _, Nat.le_refl _⟩
  | send n =>
    simp only [step, stepWith]
    split
    · split
      · exact ⟨Nat.le_refl _, Nat.le_refl _⟩
      · cases p with
        | compaction => exact ⟨Nat.le_refl _, Nat.le_refl _⟩
        | run =>
          simp only
          split
          · exact ⟨Nat.le_refl _, Nat.le_refl _⟩
          · split
            · rename_i l hl
              split
              · exact ⟨by simp [State.granted, hl], Nat.le_refl _⟩
              · exact ⟨Nat.le_refl _, Nat.le_refl _⟩
            · exact ⟨Nat.le_refl _, Nat.le_refl _⟩
    · exact ⟨Nat.le_refl _, Nat.le_refl _⟩
  | deliver n =>
    simp only [step, stepWith]
    split
    · split
      · rename_i hle; exact ⟨Nat.le_refl _, hle⟩
      · exact ⟨Nat.le_refl _, Nat.le_refl _⟩
    · exact ⟨Nat.le_refl _, Nat.le_refl _⟩
  | pause n => exact ⟨Nat.le_refl _, Nat.le_refl _⟩
  | resume n => exact ⟨Nat.le_refl _, Nat.le_refl _⟩
  | crash n => exact ⟨Nat.le_refl _, Nat.le_refl _⟩

/-- No commit after a successor's acquisition: once a node's acquisition of fence `g`
completes, raising `g` into the guarded object, every commit a step of a reachable state
lands carries a fence of at least `g`. -/
-- spec: store.lease.stale-fence@ec8153dd
-- spec: store.lease.pointer-fence@037c9e49
theorem no_commit_after_successor_acquire :
    ∀ {p : Path} {s : State}, Reachable p s → ∀ (st : Step) (f : Nat), landed s st = some f →
      ∀ (m : Node) (g : Nat), s.acquire m = some g → g ≤ f := by
  intro p s hr st f hl m g ha
  have h := reachable_inv hr
  have hm : (s.node m).raised = true := by
    unfold State.acquire at ha; split at ha
    · assumption
    · cases ha
  have hb : (s.node m).belief = some g := by simpa [State.acquire, hm] using ha
  have hg := h.raised_le m g hm hb
  cases st with
  | deliver n =>
    simp only [landed] at hl
    split at hl
    · split at hl
      · rename_i hle; cases hl; omega
      · cases hl
    · cases hl
  | _ => simp [landed] at hl

/-- No lost update: the guarded object's head, the last commit it applied, is replaced only
by a commit carrying a fence at least as high; no raise and no stale commit removes it. -/
theorem no_lost_update :
    ∀ {p : Path} {s : State}, Reachable p s → ∀ (st : Step), ¬ lostUpdate p s st := by
  intro p s hr st
  suffices s.head ≤ (step p s st).head by unfold lostUpdate; omega
  have h := reachable_inv hr
  cases st with
  | grant n =>
    simp only [step, stepWith]
    split
    · exact Nat.le_refl _
    · split
      · exact Nat.le_refl _
      · split <;> exact Nat.le_refl _
  | raise n =>
    simp only [step, stepWith]
    split
    · split
      · exact Nat.le_refl _
      · split
        · cases p <;> exact Nat.le_refl _
        · exact Nat.le_refl _
    · exact Nat.le_refl _
  | renew n =>
    simp only [step, stepWith]
    split
    · split <;> exact Nat.le_refl _
    · exact Nat.le_refl _
  | expire =>
    simp only [step, stepWith]
    split <;> exact Nat.le_refl _
  | release n =>
    simp only [step, stepWith]
    split
    · exact Nat.le_refl _
    · split
      · split <;> exact Nat.le_refl _
      · exact Nat.le_refl _
  | send n =>
    simp only [step, stepWith]
    split
    · split
      · exact Nat.le_refl _
      · cases p with
        | compaction => exact Nat.le_refl _
        | run =>
          simp only
          split
          · exact Nat.le_refl _
          · split
            · split <;> exact Nat.le_refl _
            · exact Nat.le_refl _
    · exact Nat.le_refl _
  | deliver n =>
    simp only [step, stepWith]
    split
    · split
      · rename_i hle; have := h.head_le; show s.head ≤ _; simp; omega
      · exact Nat.le_refl _
    · exact Nat.le_refl _
  | pause n => exact Nat.le_refl _
  | resume n => exact Nat.le_refl _
  | crash n => exact Nat.le_refl _

/-- Release keeps the lease object and its fence: after a release, the object exists exactly
when it did before, carrying the same fence. -/
theorem release_keeps_lease :
    ∀ (p : Path) (s : State) (n : Node),
      (step p s (.release n)).lease.map Lease.fence = s.lease.map Lease.fence := by
  intro p s n
  simp only [step, stepWith]
  split
  · rfl
  · split
    · rename_i l hl
      split
      · simp [hl]
      · simp [State.abandon, hl]
    · simp [State.abandon, *]

/-- The five safety invariants over every reachable state of either path: one lease holder
per fence, fences only increase, no commit after a successor's acquisition, no lost update,
and release keeps the lease object and its fence. -/
-- spec: assurance.model.protocol-safety@4d566dd5
theorem protocol_safety :
    ∀ {p : Path} {s : State}, Reachable p s →
      (∀ (n m : Node) (fence : Nat), (s.node n).belief = some fence → (s.node m).belief = some fence → n = m) ∧
      (∀ st : Step, s.granted ≤ (step p s st).granted ∧ s.fence ≤ (step p s st).fence) ∧
      (∀ (st : Step) (f : Nat), landed s st = some f → ∀ (m : Node) (g : Nat), s.acquire m = some g → g ≤ f) ∧
      (∀ st : Step, ¬ lostUpdate p s st) ∧
      (∀ n : Node, (step p s (.release n)).lease.map Lease.fence = s.lease.map Lease.fence) :=
  fun h => ⟨one_holder_per_fence h, fences_only_increase _ _, no_commit_after_successor_acquire h,
    no_lost_update h, release_keeps_lease _ _⟩

end Protocol
