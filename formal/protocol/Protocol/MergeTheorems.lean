import Protocol.Merge

/-!
# The merge theorems

Each theorem ranges over every input of one key, every writer and every instant; the merge
differential samples the model against the Rust merge.
-/

namespace Protocol.Merge

theorem applyTomb_tomb {me now : Nat} {v : Bool} {b : Option Entry × Option Tombstone}
    {rt : Option Tombstone} {t : Tombstone} (ht : (applyTomb me now v b rt).tomb = some t) :
    b.2 = some t ∨ (rt = some t ∧ now - t.deletedAt < ttl) := by
  rcases b with ⟨e, tb⟩
  cases tb with
  | some t' => simp only [applyTomb] at ht; exact .inl ht
  | none =>
    cases rt with
    | none => simp [applyTomb] at ht
    | some r =>
      simp only [applyTomb] at ht
      by_cases hx : ttl ≤ now - r.deletedAt
      · simp [hx] at ht
      · simp only [hx, if_false] at ht
        have hlive : now - r.deletedAt < ttl := by omega
        cases e with
        | none => simp at ht; subst ht; exact .inr ⟨rfl, hlive⟩
        | some x =>
          simp only at ht
          by_cases h1 : (x.owner != r.owner) = true
          · simp [h1] at ht
          · simp only [h1] at ht
            by_cases h2 : (x.owner == me) = true
            · simp [h2] at ht
            · simp only [h2] at ht
              cases v <;> simp at ht
              subst ht; exact .inr ⟨rfl, hlive⟩

theorem base_tomb {me now : Nat} {i : Input} {t : Tombstone} (ht : (base me now i).2 = some t) :
    t = ⟨me, now⟩ ∧ i.held = true ∧ (∃ r, i.remoteEntry = some r ∧ r.owner = me) ∧
      ∀ e, i.localEntry = some e → e.owner ≠ 0 ∧ e.owner ≠ me := by
  simp only [base] at ht
  (repeat' split at ht) <;> simp_all

/-- A local entry this writer owns stays listed; a local entry another writer owns counts for
nothing; and a merge writes a fresh tombstone only for a remote entry this writer owns, held
at its last sync, and no longer holds. -/
-- spec: store.merge.scoped-union@fc9577a4
theorem merge_scoped_union (me now : Nat) (i : Input) :
    (∀ e, i.localEntry = some e → e.owner = me → (mergeKey me now i).entry = some e) ∧
    (∀ e, i.localEntry = some e → e.owner ≠ 0 → e.owner ≠ me →
      mergeKey me now i = mergeKey me now { i with localEntry := none }) ∧
    (∀ t, (mergeKey me now i).tomb = some t → i.remoteTomb ≠ some t →
      t = ⟨me, now⟩ ∧ i.held = true ∧ (∃ r, i.remoteEntry = some r ∧ r.owner = me) ∧
      ∀ e, i.localEntry = some e → e.owner ≠ 0 ∧ e.owner ≠ me) := by
  refine ⟨fun e hl ho => ?_, fun e hl h0 hme => ?_, fun t ht hne => ?_⟩
  · have hk : (e.owner == 0 || e.owner == me) = true := by
      have h : (me == me) = true := by show decide (me = me) = true; exact decide_eq_true rfl
      rw [ho, h, Bool.or_true]
    have hb : base me now i = (some e, none) := by
      unfold base; rw [hl]
      show (match (if (e.owner == 0 || e.owner == me) = true then some e else none) with
        | some e => (some e, none) | none => _) = _
      rw [hk]; rfl
    simp only [mergeKey, hb]
    cases hr : i.remoteTomb with
    | none => rfl
    | some r =>
      simp only [applyTomb]
      split
      · rfl
      · have hm : (e.owner == me) = true := by
          rw [ho]; show decide (me = me) = true; exact decide_eq_true rfl
        by_cases h1 : (e.owner != r.owner) = true
        · rw [if_pos h1]
        · rw [if_neg h1, if_pos hm]
  · simp only [mergeKey, base, hl, Option.filter]
    have : (e.owner == 0 || e.owner == me) = false := by simp [h0, hme]
    simp only [this]
    rfl
  · rcases applyTomb_tomb ht with hb | ⟨hr, _⟩
    · exact base_tomb hb
    · exact absurd hr hne

/-- A live remote tombstone whose owner differs from the owner of the entry it names keeps
the entry and answers `foreign`. -/
-- spec: store.merge.tombstone-owner@df322a2c
theorem foreign_tombstone_keeps_entry (me now : Nat) (i : Input) (x : Entry) (rt : Tombstone)
    (hb : base me now i = (some x, none)) (hr : i.remoteTomb = some rt)
    (hlive : now - rt.deletedAt < ttl) (ho : x.owner ≠ rt.owner) :
    mergeKey me now i = ⟨some x, none, some .foreign⟩ := by
  have hx : ¬ ttl ≤ now - rt.deletedAt := by omega
  simp [mergeKey, hb, hr, applyTomb, hx, ho]

/-- A live remote tombstone naming another writer's entry of its own owner that fails
verification keeps the entry and answers `unverified`. -/
-- spec: store.merge.tombstone-unverified@fab14869
theorem unverified_tombstone_keeps_entry (me now : Nat) (i : Input) (x : Entry) (rt : Tombstone)
    (hb : base me now i = (some x, none)) (hr : i.remoteTomb = some rt)
    (hlive : now - rt.deletedAt < ttl) (ho : x.owner = rt.owner) (hme : x.owner ≠ me)
    (hv : i.verified = false) :
    mergeKey me now i = ⟨some x, none, some .unverified⟩ := by
  have hx : ¬ ttl ≤ now - rt.deletedAt := by omega
  have hme' : ¬ rt.owner = me := ho ▸ hme
  simp [mergeKey, hb, hr, applyTomb, hx, ho, hme', hv]

/-- Every tombstone a merge lists is younger than 30 d. -/
-- spec: store.merge.tombstone-ttl@30121692
theorem merged_tombstones_live (me now : Nat) (i : Input) (t : Tombstone)
    (ht : (mergeKey me now i).tomb = some t) : now - t.deletedAt < ttl := by
  rcases applyTomb_tomb ht with hb | ⟨_, hl⟩
  · rw [(base_tomb hb).1]; simp [ttl]
  · exact hl

/-- A commit-log key whose local and remote copies differ refuses the whole merge. -/
-- spec: store.merge.cursor-recency@a551b135
theorem merge_refuses_on_conflicts (me now : Nat) (is : List Input) (i : Input) (hi : i ∈ is)
    (hc : conflicts me i = true) : merge me now is = none := by
  have : is.any (conflicts me) = true := List.any_eq_true.mpr ⟨i, hi, hc⟩
  simp [merge, this]

/-- An entry `base` kept survives a tombstone or leaves under a live one. -/
theorem applyTomb_entry {me now : Nat} {v : Bool} {x : Entry} {rt : Option Tombstone} :
    (applyTomb me now v (some x, none) rt).entry = some x ∨
      ((applyTomb me now v (some x, none) rt).entry = none ∧
       ∃ t, rt = some t ∧ (applyTomb me now v (some x, none) rt).tomb = some t ∧ now - t.deletedAt < ttl) := by
  cases rt with
  | none => exact .inl rfl
  | some r =>
    simp only [applyTomb]
    by_cases hx : ttl ≤ now - r.deletedAt
    · simp [hx]
    · simp only [hx, if_false]
      by_cases h1 : (x.owner != r.owner) = true
      · simp [h1]
      · simp only [h1]
        by_cases h2 : (x.owner == me) = true
        · simp [h2]
        · simp only [h2]
          cases v
          · simp
          · right; simp only [if_true]; exact ⟨rfl, r, rfl, rfl, by omega⟩

theorem applyTomb_some_converges (me now : Nat) (v : Bool) (x : Entry) (rt : Option Tombstone) :
    (applyTomb me now v (some x, none) (applyTomb me now v (some x, none) rt).tomb).entry
        = (applyTomb me now v (some x, none) rt).entry ∧
    (applyTomb me now v (some x, none) (applyTomb me now v (some x, none) rt).tomb).tomb
        = (applyTomb me now v (some x, none) rt).tomb := by
  rcases applyTomb_entry (me := me) (now := now) (v := v) (x := x) (rt := rt) with he | ⟨he, t, hr, ht, hl⟩
  · -- The entry stays, so the merge lists no tombstone for the key.
    have hnone : (applyTomb me now v (some x, none) rt).tomb = none := by
      cases h : (applyTomb me now v (some x, none) rt).tomb with
      | none => rfl
      | some t =>
        exfalso
        rcases applyTomb_tomb h with hb | ⟨hr, _⟩
        · simp at hb
        · subst hr
          rcases applyTomb_entry (me := me) (now := now) (v := v) (x := x) (rt := some t) with h' | ⟨h', _⟩
          · -- A live tombstone that keeps the entry lists no tombstone.
            revert h; revert h'
            simp only [applyTomb]
            split
            · simp
            · split <;> (try split) <;> (try split) <;> simp_all
          · rw [he] at h'; cases h'
    rw [hnone, he]
    exact ⟨rfl, rfl⟩
  · rw [ht, he]
    have hx : ¬ ttl ≤ now - t.deletedAt := by omega
    rw [hr] at ht he
    revert ht he
    simp only [applyTomb, hx, if_false]
    split <;> (try split) <;> (try split) <;> simp_all

theorem applyTomb_none_converges (me now : Nat) (v : Bool) (rt : Option Tombstone) :
    applyTomb me now v (none, none) (applyTomb me now v (none, none) rt).tomb
      = applyTomb me now v (none, none) rt := by
  cases rt with
  | none => rfl
  | some r =>
    by_cases hx : ttl ≤ now - r.deletedAt
    · simp [applyTomb, hx]
    · simp [applyTomb, hx]

/-- At one `now`, merging the same store into a merge's own output lists the same entry and
tombstone: one writer's merges converge in one step. -/
theorem merge_converges (me now : Nat) (i : Input) :
    (mergeKey me now (i.after (mergeKey me now i))).entry = (mergeKey me now i).entry ∧
    (mergeKey me now (i.after (mergeKey me now i))).tomb = (mergeKey me now i).tomb := by
  simp only [mergeKey]
  have hloc : (i.after (applyTomb me now i.verified (base me now i) i.remoteTomb)).localEntry = i.localEntry := rfl
  cases hf : i.localEntry.filter (fun e => e.owner == 0 || e.owner == me) with
  | some x =>
    have hb : ∀ j : Input, j.localEntry = i.localEntry → base me now j = (some x, none) := by
      intro j hj; simp only [base, hj, hf]
    rw [hb _ hloc, hb i rfl]
    exact applyTomb_some_converges me now i.verified x i.remoteTomb
  | none =>
    have hb : ∀ j : Input, j.localEntry = i.localEntry → j.held = i.held →
        base me now j = match j.remoteEntry with
          | some r => if (r.owner == me && i.held) = true then (none, some ⟨me, now⟩) else (some r, none)
          | none => (none, none) := by
      intro j hj hh; simp only [base, hj, hf, hh]; rfl
    rw [hb _ hloc rfl, hb i rfl rfl]
    simp only [Input.after]
    cases hr : i.remoteEntry with
    | none =>
      simp only
      have he : (applyTomb me now i.verified (none, none) i.remoteTomb).entry = none := by
        cases i.remoteTomb with
        | none => rfl
        | some r => simp only [applyTomb]; split <;> rfl
      rw [he]
      simp only
      rw [applyTomb_none_converges]
      exact ⟨he, rfl⟩
    | some r =>
      simp only
      by_cases hm : (r.owner == me && i.held) = true
      · simp only [hm, if_true, applyTomb]
        simp [ttl]
      · simp only [hm, Bool.false_eq_true, if_false]
        rcases applyTomb_entry (me := me) (now := now) (v := i.verified) (x := r) (rt := i.remoteTomb) with he | ⟨he, t, _, ht, hl⟩
        · have key := applyTomb_some_converges me now i.verified r i.remoteTomb
          rw [he] at key ⊢
          simp only [hm, Bool.false_eq_true, if_false]
          exact key
        · rw [he, ht]
          have hx : ¬ ttl ≤ now - t.deletedAt := by omega
          simp [applyTomb, hx]

end Protocol.Merge
