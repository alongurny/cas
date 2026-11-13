# Measuring Lean declarations for table proofs

How much a table proof of a Lean declaration has to hold (spec/lean/tables.md):
the terms of each declaration, and the steps Lean's kernel takes to check it,
each of which becomes about one judgement row.

1. Apply `kernel-counters.patch` to Lean v4.25.0 (`leanprover/lean4`, tag
   `v4.25.0`) and build it (`cmake --preset release && make -C build/release`).
   The patch counts, per declaration added, the kernel's cache misses and hits
   for `infer`, `whnf_core`, `whnf` and `is_def_eq`, its unfoldings and its
   `Nat` reductions, and appends them to the file named by `LEAN_KERNEL_STATS`.
2. With that build's `lean` first on `PATH`:
   `LEAN_KERNEL_STATS=$PWD/out/kstats.tsv lean --run Measure.lean out Init`.
   It writes `out/terms.tsv` (distinct term nodes and tree size per constant)
   and replays every declaration of the modules through the kernel. The replay
   ends with an error about the recursors of nested inductive types
   (`Lean.Syntax.rec_2`), after every declaration was added; the duplicate-name
   panics come from the replay and do not stop it. The first lines of
   `kstats.tsv`, up to `main`, are `Measure.lean`'s own declarations: drop them.
3. `cargo run --release -p cas-lean --example summary out`.

Results for `Init` (2026-11-13): 53,804 constants, 5,226,064 distinct term
nodes over all of them. Kernel steps per declaration added (51,910): median
183, mean 1,045, p90 1,484, p99 15,633, max 388,197; 54.3 million in all.
Only 2,712 of 9.7 million `is_def_eq` calls failed.
