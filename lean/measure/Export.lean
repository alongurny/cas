import Lean
open Lean

/-!
Writes the declarations of the given modules as text, for cas-lean's
importer (`lean/src/import.rs`). Names, levels and terms are numbered and
shared; each line is one item, its fields separated by spaces:

* `N i p s <hex>` / `N i p n <nat>`: name i is name p (0 = anonymous) with a
  string (hex of its UTF-8 bytes) or number appended;
* `L i z` / `L i s a` / `L i m a b` / `L i i a b` / `L i p <name>`: levels;
* `E i v n` (bound variable), `E i s l`, `E i c <name> <level>...`,
  `E i a f x`, `E i l t b` (lambda), `E i f t b` (pi), `E i z t v b` (let),
  `E i n <nat>`, `E i t <hex>` (string literal), `E i j <struct> idx e`;
  metadata is dropped, as the kernel ignores it;
* `D <name> <type> <nlevels> <level names>... [u] | <rest>` (u marks an
  unsafe declaration, which the kernel does not fully check), rest by
  kind: `axiom`; `def <value> <hint>` (hint `o`, `a` or a height);
  `thm <value>`; `opaque <value>`; `quot <type|ctor|lift|ind>`;
  `induct <params> <indices> <rec> <nnested> all <names> ctors <names>`;
  `ctor <induct> <cidx> <params> <fields>`;
  `rec <params> <indices> <motives> <minors> <k> all <names> rules
  (<ctor> <fields> <rhs>)...`.

Usage: `lean --run Export.lean <out-file> <Module>...`
-/

structure St where
  names : Std.HashMap Name Nat := {}
  levels : Std.HashMap Level Nat := {}
  exprs : Std.HashMap Expr Nat := {}
  next : Nat := 1

abbrev M := StateT St (ReaderT IO.FS.Handle IO)

def emit (s : String) : M Unit := do (← read).putStrLn s

def hex (s : String) : String :=
  String.join (s.toUTF8.toList.map fun b =>
    let d := "0123456789abcdef".toList
    s!"{d[(b.toNat / 16)]!}{d[(b.toNat % 16)]!}")

def fresh : M Nat := do
  let s ← get
  set { s with next := s.next + 1 }
  return s.next

partial def name (n : Name) : M Nat := do
  if n.isAnonymous then return 0
  if let some i := (← get).names.get? n then return i
  let line ← match n with
    | .str p s => do pure s!"{← name p} s {hex s}"
    | .num p k => do pure s!"{← name p} n {k}"
    | .anonymous => pure ""
  let i ← fresh
  emit s!"N {i} {line}"
  modify fun s => { s with names := s.names.insert n i }
  return i

partial def level (l : Level) : M Nat := do
  if let some i := (← get).levels.get? l then return i
  let line ← match l with
    | .zero => pure "z"
    | .succ a => do pure s!"s {← level a}"
    | .max a b => do pure s!"m {← level a} {← level b}"
    | .imax a b => do pure s!"i {← level a} {← level b}"
    | .param n => do pure s!"p {← name n}"
    | .mvar _ => throw (IO.userError "a level metavariable")
  let i ← fresh
  emit s!"L {i} {line}"
  modify fun s => { s with levels := s.levels.insert l i }
  return i

partial def expr (e : Expr) : M Nat := do
  if let .mdata _ b := e then return ← expr b
  if let some i := (← get).exprs.get? e then return i
  let line ← match e with
    | .bvar n => pure s!"v {n}"
    | .sort l => do pure s!"s {← level l}"
    | .const n ls => do
      let ls ← ls.mapM level
      pure s!"c {← name n}{String.join (ls.map (s!" {·}"))}"
    | .app f x => do pure s!"a {← expr f} {← expr x}"
    | .lam _ t b _ => do pure s!"l {← expr t} {← expr b}"
    | .forallE _ t b _ => do pure s!"f {← expr t} {← expr b}"
    | .letE _ t v b _ => do pure s!"z {← expr t} {← expr v} {← expr b}"
    | .lit (.natVal n) => pure s!"n {n}"
    | .lit (.strVal s) => pure s!"t {hex s}"
    | .proj s i x => do pure s!"j {← name s} {i} {← expr x}"
    | .fvar _ | .mvar _ | .mdata .. => throw (IO.userError "a free variable or metavariable")
  let i ← fresh
  emit s!"E {i} {line}"
  modify fun s => { s with exprs := s.exprs.insert e i }
  return i

def names (ns : List Name) : M String := do
  return String.join (← ns.mapM fun n => do pure s!" {← name n}")

def decl (ci : ConstantInfo) : M Unit := do
  let n ← name ci.name
  let ty ← expr ci.type
  let lps ← names ci.levelParams
  let rest ← match ci with
    | .axiomInfo _ => pure "axiom"
    | .defnInfo v => do
      let h := match v.hints with
        | .opaque => "o" | .abbrev => "a" | .regular k => s!"{k}"
      pure s!"def {← expr v.value} {h}"
    | .thmInfo v => do pure s!"thm {← expr v.value}"
    | .opaqueInfo v => do pure s!"opaque {← expr v.value}"
    | .quotInfo v =>
      pure s!"quot {match v.kind with | .type => "type" | .ctor => "ctor" | .lift => "lift" | .ind => "ind"}"
    | .inductInfo v => do
      pure s!"induct {v.numParams} {v.numIndices} {if v.isRec then 1 else 0} {v.numNested} all{← names v.all} ctors{← names v.ctors}"
    | .ctorInfo v => do pure s!"ctor {← name v.induct} {v.cidx} {v.numParams} {v.numFields}"
    | .recInfo v => do
      let rules ← v.rules.mapM fun r => do pure s!" {← name r.ctor} {r.nfields} {← expr r.rhs}"
      pure s!"rec {v.numParams} {v.numIndices} {v.numMotives} {v.numMinors} {if v.k then 1 else 0} all{← names v.all} rules{String.join rules}"
  emit s!"D {n} {ty} {ci.levelParams.length}{lps}{if ci.isUnsafe then " u" else ""} | {rest}"

def main (args : List String) : IO Unit := do
  let out :: mods := args | throw (IO.userError "usage: <out-file> <Module>...")
  initSearchPath (← findSysroot)
  let env ← importModules (mods.toArray.map fun m => { module := m.toName }) {}
  let h ← IO.FS.Handle.mk out .write
  let consts := env.constants.map₁.toList
  let (_, st) ← ((consts.forM fun (_, ci) => decl ci) : M Unit).run {} |>.run h
  IO.println s!"{consts.length} declarations, {st.next - 1} names, levels and terms"
