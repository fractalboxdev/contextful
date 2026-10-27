/-!
# The decision a case prints

A verdict, an error identifier, the dimension a widening names, and the zone a placement
case resolves.
-/

namespace Reference

/-- A decision the reference prints. -/
structure Decision where
  verdict : String
  error : Option String := none
  dimension : Option String := none
  zone : Option String := none

def Decision.refused (error : String) (dimension : Option String := none) : Decision :=
  { verdict := "refused", error := some error, dimension }

def Decision.admitted : Decision := { verdict := "admitted" }

end Reference
