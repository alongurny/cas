import Lean
open Lean

/-!
Measures the work a table checker for Lean (zkPi extended) would have to
check, on the declarations of the given modules:

* `terms.tsv`: per declaration, the distinct term nodes of its type and value
  (what a terms table holds for it) and their size as a tree (what a checker
  without sharing would walk);
* the kernel counters: every declaration is replayed through the kernel,
  which (instrumented) appends its cache misses, hits and equality checks to
  the file named by `LEAN_KERNEL_STATS`.

Usage: `lean --run Measure.lean <out-dir> <Module>...`
-/

structure Counts where
  seen : Std.HashSet Expr := {}
  sizes : Std.HashMap Expr Nat := {}

/-- Adds the distinct nodes of `e` to `seen`; returns how many were new. -/
partial def visit (e : Expr) : StateM Counts Nat := do
  if (← get).seen.contains e then return 0
  modify fun s => { s with seen := s.seen.insert e }
  let sub ← match e with
    | .app f a => pure ((← visit f) + (← visit a))
    | .lam _ t b _ | .forallE _ t b _ => pure ((← visit t) + (← visit b))
    | .letE _ t v b _ => pure ((← visit t) + (← visit v) + (← visit b))
    | .mdata _ b | .proj _ _ b => visit b
    | _ => pure (0)
  return sub + 1

/-- The size of `e` as a tree, with sharing expanded. -/
partial def treeSize (e : Expr) : StateM Counts Nat := do
  if let some n := (← get).sizes.get? e then return n
  let n ← match e with
    | .app f a => pure (1 + (← treeSize f) + (← treeSize a))
    | .lam _ t b _ | .forallE _ t b _ => pure (1 + (← treeSize t) + (← treeSize b))
    | .letE _ t v b _ => pure (1 + (← treeSize t) + (← treeSize v) + (← treeSize b))
    | .mdata _ b | .proj _ _ b => pure (1 + (← treeSize b))
    | _ => pure (1)
  modify fun s => { s with sizes := s.sizes.insert e n }
  return n

def kind : ConstantInfo → String
  | .axiomInfo _ => "axiom" | .defnInfo _ => "def" | .thmInfo _ => "thm"
  | .opaqueInfo _ => "opaque" | .quotInfo _ => "quot" | .inductInfo _ => "inductive"
  | .ctorInfo _ => "ctor" | .recInfo _ => "rec"

def main (args : List String) : IO Unit := do
  let out :: mods := args | throw (IO.userError "usage: <out-dir> <Module>...")
  initSearchPath (← findSysroot)
  let env ← importModules (mods.toArray.map fun m => { module := m.toName }) {}
  let consts := env.constants.map₁.toList
  IO.println s!"{consts.length} constants"
  -- Terms: per declaration on its own, and all declarations together.
  let h ← IO.FS.Handle.mk s!"{out}/terms.tsv" .write
  h.putStrLn "kind\tname\tdistinct_nodes\ttree_size"
  let mut global : Counts := {}
  for (n, ci) in consts do
    let es := [ci.type] ++ (ci.value?.toList)
    let ((d, t), _) := (do
        let mut d := 0
        let mut t := 0
        for e in es do
          d := d + (← visit e)
          t := t + (← treeSize e)
        return (d, t) : StateM Counts (Nat × Nat)).run {}
    let (_, g) := (es.forM fun e => do let _ ← visit e; pure () : StateM Counts Unit).run global
    global := { g with sizes := {} }
    h.putStrLn s!"{kind ci}\t{n}\t{d}\t{t}"
  IO.println s!"distinct nodes over all declarations: {global.seen.size}"
  -- Kernel: replay everything into an empty environment.
  let env0 ← mkEmptyEnvironment
  let start ← IO.monoMsNow
  let _ ← env0.replay (Std.HashMap.ofList consts)
  IO.println s!"replayed in {(← IO.monoMsNow) - start} ms"
