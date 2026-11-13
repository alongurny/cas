import Lean
open Lean

/-!
Times Lean's own kernel re-checking every declaration of the given modules
and their imports, as lean4checker does (`Lean.Kernel.Environment.replay`:
each declaration sent to the kernel; unsafe and partial ones skipped), for
comparison with cas-lean's prover and checker.

Usage: `lean --run Replay.lean <Module>...`
-/

def main (mods : List String) : IO Unit := do
  initSearchPath (← findSysroot)
  let env ← importModules (mods.toArray.map fun m => { module := m.toName }) {}
  let consts : Std.HashMap Name ConstantInfo := env.constants.map₁.fold (fun m n ci => m.insert n ci) {}
  let empty ← mkEmptyEnvironment
  let t0 ← IO.monoMsNow
  let _ ← empty.toKernelEnv.replay consts
  let t1 ← IO.monoMsNow
  IO.println s!"{consts.size} declarations replayed by Lean's kernel in {t1 - t0} ms"
