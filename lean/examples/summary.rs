//! Summarizes a measurement of Lean declarations (lean/measure): the
//! terms each declaration's tables hold and the kernel steps its check
//! takes, which is about the judgement rows a table proof would need.
//!
//! Usage: `cargo run --release -p cas-lean --example summary <out-dir>`
//! where the directory holds `terms.tsv` and `kstats.tsv`.

use std::collections::HashMap;

fn quantiles(mut v: Vec<u64>) -> String {
    if v.is_empty() {
        return "none".into();
    }
    v.sort_unstable();
    let q = |p: f64| v[((v.len() - 1) as f64 * p) as usize];
    let sum: u64 = v.iter().sum();
    format!(
        "n={} sum={} mean={:.0} median={} p90={} p99={} max={}",
        v.len(),
        sum,
        sum as f64 / v.len() as f64,
        q(0.5),
        q(0.9),
        q(0.99),
        v[v.len() - 1]
    )
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: summary <out-dir>");
    let terms = std::fs::read_to_string(format!("{dir}/terms.tsv")).expect("terms.tsv");
    let mut by_kind: HashMap<String, (Vec<u64>, Vec<u64>)> = HashMap::new();
    for line in terms.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        let e = by_kind.entry(f[0].to_string()).or_default();
        e.0.push(f[2].parse().unwrap());
        e.1.push(f[3].parse().unwrap());
    }
    println!("# Terms per declaration (distinct nodes; tree size)");
    let mut kinds: Vec<_> = by_kind.into_iter().collect();
    kinds.sort();
    for (k, (d, t)) in kinds {
        println!("{k:10} distinct: {}", quantiles(d));
        println!("{:10} tree:     {}", "", quantiles(t));
    }
    let ks = std::fs::read_to_string(format!("{dir}/kstats.tsv")).expect("kstats.tsv");
    // Columns: kind name infer_check infer_only infer_hit whnf_core
    // whnf_core_hit whnf whnf_hit defeq_core defeq_true defeq_equiv_hit
    // defeq_fail unfold nat. Each cache miss is a step that writes rows;
    // hits reuse a fact already proven.
    let mut steps: HashMap<String, Vec<u64>> = HashMap::new();
    let mut cols = [0u64; 13];
    for line in ks.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        let n: Vec<u64> = f[2..].iter().map(|x| x.parse().unwrap()).collect();
        for (c, x) in cols.iter_mut().zip(&n) {
            *c += x;
        }
        let s = n[0] + n[1] + n[3] + n[5] + n[7] + n[11] + n[12];
        steps.entry(f[0].to_string()).or_default().push(s);
        steps.entry("all".into()).or_default().push(s);
    }
    println!("\n# Kernel steps per declaration added (cache misses: infer, whnf_core, whnf, is_def_eq, unfold, nat)");
    let mut kinds: Vec<_> = steps.into_iter().collect();
    kinds.sort();
    for (k, v) in kinds {
        println!("{k:10} {}", quantiles(v));
    }
    let names = [
        "infer_check",
        "infer_only",
        "infer_hit",
        "whnf_core",
        "whnf_core_hit",
        "whnf",
        "whnf_hit",
        "defeq_core",
        "defeq_true",
        "defeq_equiv_hit",
        "defeq_fail",
        "unfold",
        "nat",
    ];
    println!("\n# Totals");
    for (n, c) in names.iter().zip(cols) {
        println!("{n:16} {c}");
    }
}
