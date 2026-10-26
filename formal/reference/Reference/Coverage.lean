import Contextful.Authority

/-!
# Table-pattern parsing

Decodes a pattern string into `Authority.Pattern`, whose `covers` and `subsumes` decide
coverage. A pattern is `*`; a prefix ending in `*`, held without its `*`; or any other
string, matched exactly. A `*` anywhere but the final position is malformed.
-/

namespace Reference

/-- Parse a pattern; `none` when a `*` stands anywhere but the final position. -/
def parsePattern (s : String) : Option Authority.Pattern :=
  match s.toList.reverse with
  | '*' :: restRev =>
    if restRev.contains '*' then none
    else if restRev.isEmpty then some .star
    else some (.prefixed restRev.reverse)
  | _ =>
    if s.toList.contains '*' then none else some (.literal s.toList)

end Reference
