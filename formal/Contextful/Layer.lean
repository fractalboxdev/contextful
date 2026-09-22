/-!
# The layer algebra

A layer admits or removes one row. A registered relation applies its layers as one list,
in the single order the relation fixes, and a row survives the relation when every layer
in the list admits it.
-/

/-- A row identifier. The model fixes no structure on it beyond equality. -/
abbrev RowId := Nat

/-- A layer: a total function from a row identifier to `Bool`, `true` admitting the row. -/
abbrev Layer := RowId → Bool

/-- The fold of a list of layers by conjunction, in list order: a row is admitted when
every member admits it, and the empty list admits every row. -/
-- spec: assurance.model.layer@1d6c2117
def composed (ls : List Layer) : Layer :=
  fun r => ls.all (fun l => l r)

/-- A row the fold over a list admits is admitted by every member of that list. -/
-- spec: assurance.prove.composition-sound@768c4c71
theorem composed_sound :
    ∀ {ls : List Layer} {r : RowId}, composed ls r = true → ∀ l ∈ ls, l r = true :=
  fun h => List.all_eq_true.mp h

/-- A row admitted by `l :: ls` is admitted by `ls`: appending a layer removes rows and adds none. -/
-- spec: assurance.prove.narrowing@bf923b9d
theorem composed_narrows :
    ∀ {l : Layer} {ls : List Layer} {r : RowId}, composed (l :: ls) r = true → composed ls r = true :=
  fun h => (Bool.and_eq_true _ _).mp h |>.2
