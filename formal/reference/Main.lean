import Reference

/-! The reference binary: one case on standard input, one decision on standard output. -/

partial def readAll (h : IO.FS.Stream) (acc : ByteArray) : IO ByteArray := do
  let chunk ← h.read 65536
  if chunk.isEmpty then pure acc else readAll h (acc ++ chunk)

def main : IO Unit := do
  let input ← readAll (← IO.getStdin) ByteArray.empty
  IO.println (Reference.runBytes input).toJson.compress
