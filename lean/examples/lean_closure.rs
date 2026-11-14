//! The declarations a set of theorems depends on, transitively through
//! types, values and inductive blocks, with their derivation rows from a
//! `lean_check` result file. Usage: `lean_closure <export> <check.tsv> NAME...`.

use cas_lean::import::{Env, Kind};
use std::collections::{HashMap, HashSet};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let env = Env::parse(&std::fs::read_to_string(&args[1]).unwrap()).unwrap();
    let rows: HashMap<String, (String, u64)> = std::fs::read_to_string(&args[2])
        .unwrap()
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            Some((f[0].to_string(), (f[2].to_string(), f.get(3)?.parse().ok()?)))
        })
        .collect();
    let mut seen_terms = HashSet::new();
    let mut done: HashSet<u32> = HashSet::new();
    let mut todo: Vec<u32> = args[3..].iter().map(|n| env.find(n).unwrap_or_else(|| panic!("no {n}")).name).collect();
    while let Some(n) = todo.pop() {
        if !done.insert(n) {
            continue;
        }
        let Some(d) = env.by_name(n) else { continue };
        let mut out = HashSet::new();
        env.constants(d.ty, &mut seen_terms, &mut out);
        if let Some(v) = Env::value_of(d) {
            env.constants(v, &mut seen_terms, &mut out);
        }
        match &d.kind {
            Kind::Induct { all, ctors, .. } => out.extend(all.iter().chain(ctors)),
            Kind::Ctor { induct, .. } => {
                out.insert(*induct);
            }
            Kind::Rec { all, rules, .. } => {
                out.extend(all);
                for r in rules {
                    out.insert(r.0);
                    env.constants(r.2, &mut seen_terms, &mut out);
                }
            }
            _ => {}
        }
        todo.extend(out.into_iter().filter(|c| !done.contains(c)));
    }
    let (mut decls, mut checked, mut total, mut missing) = (0, 0, 0u64, 0);
    for &n in &done {
        let Some(d) = env.by_name(n) else { continue };
        decls += 1;
        if matches!(d.kind, Kind::Axiom | Kind::Def { .. } | Kind::Thm { .. } | Kind::Opaque { .. }) {
            match rows.get(&env.name(n)) {
                Some((_, r)) => {
                    checked += 1;
                    total += r;
                }
                None => missing += 1,
            }
        }
    }
    let all: u64 = rows.values().map(|r| r.1).sum();
    println!("closure {decls} declarations, {checked} with rows ({missing} without), {total} rows of {all} in the file ({:.1}%)", 100.0 * total as f64 / all as f64);
}
