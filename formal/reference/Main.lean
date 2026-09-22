import Reference

/-! The reference binary: one case on standard input, one decision on standard output. -/

partial def readAll (h : IO.FS.Stream) (acc : String) : IO String := do
  let line ← h.getLine
  if line.isEmpty then pure acc else readAll h (acc ++ line)

def main : IO Unit := do
  let input ← readAll (← IO.getStdin) ""
  IO.println (Reference.run input).toJson.compress
