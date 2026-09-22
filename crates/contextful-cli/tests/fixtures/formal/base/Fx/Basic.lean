namespace Fx

def Layer := Nat → Bool

def composed (ls : List Layer) (r : Nat) : Bool := ls.all (fun l => l r)

theorem narrows (n : Nat) : n + 0 = n := rfl

theorem extensional (f g : Nat → Nat) (h : ∀ n, f n = g n) (a b : Prop) (e : a ↔ b) :
    f = g ∧ a = b :=
  ⟨funext h, propext e⟩

end Fx
