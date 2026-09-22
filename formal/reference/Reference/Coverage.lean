/-!
# Table-pattern coverage

Rendered from `authority.grant.pattern-forms` and `authority.grant.malformed-pattern`
and the coverage table in the authority contract's Shapes section. Written from the
specification text alone.

A pattern is `*`, covering every table; a prefix ending in `*`, covering every name
beginning with that prefix; or any other string, matched exactly. A `*` anywhere but the
final position is malformed.
-/

namespace Reference

/-- A parsed table pattern. -/
inductive Pattern where
  /-- `*`: every table. -/
  | all
  /-- `<prefix>*` with a non-empty prefix: every name beginning with the prefix. -/
  | pre (p : String)
  /-- Any other string, matched exactly. -/
  | exact (s : String)
  deriving Repr, DecidableEq

/-- Parse a pattern; `none` when a `*` stands anywhere but the final position. -/
def Pattern.parse (s : String) : Option Pattern :=
  match s.toList.reverse with
  | '*' :: restRev =>
    if restRev.contains '*' then none
    else if restRev.isEmpty then some .all
    else some (.pre (String.ofList restRev.reverse))
  | _ =>
    if s.toList.contains '*' then none else some (.exact s)

/-- Whether a pattern covers a table name. -/
def Pattern.coversName : Pattern → String → Bool
  | .all, _ => true
  | .pre p, n => p.toList.isPrefixOf n.toList
  | .exact e, n => e == n

/-- Whether `self` covers every name `other` covers.

* `other = *` covers every name; only `*` covers every name, so a concrete pattern never
  covers `*`.
* `other = q*` covers `q` extended by any string. `*` covers all of them; `p*` covers all
  of them exactly when `p` is a prefix of `q` (the name `q` itself forces it, and any
  extension of `q` then starts with `p`); an exact pattern covers one name, never the
  infinitely many `q*` covers.
* `other = n` covers the one name `n`, so `self` covers it when it covers `n`. -/
def Pattern.covers (self other : Pattern) : Bool :=
  match other with
  | .all => self == .all
  | .pre q =>
    match self with
    | .all => true
    | .pre p => p.toList.isPrefixOf q.toList
    | .exact _ => false
  | .exact n => self.coversName n

end Reference
