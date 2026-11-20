//! The declarations a set of theorems depends on, transitively through
//! types, values and inductive blocks, with their derivation rows from a
//! `lean_check` result file (`-` for none). Usage:
//! `lean_closure <export> <check.tsv> [--out FILE] NAME...`; FILE gets the
//! names of the axioms, definitions, theorems and opaques of the closure.

use cas_lean::import::{Env, Kind};
use std::collections::{HashMap, HashSet};

pub fn main() {
    let args: Vec<String> = std::env::args().collect();
    let env = Env::parse(&std::fs::read_to_string(&args[1]).unwrap()).unwrap();
    let out_file = args.iter().position(|a| a == "--out").map(|i| args[i + 1].clone());
    let names: Vec<&String> = match args.iter().position(|a| a == "--out") {
        Some(i) => args[3..i].iter().chain(&args[i + 2..]).collect(),
        None => args[3..].iter().collect(),
    };
    let rows: HashMap<String, (String, u64)> =
        (if args[2] == "-" { String::new() } else { std::fs::read_to_string(&args[2]).unwrap() })
            .lines()
            .filter_map(|l| {
                let f: Vec<&str> = l.split('\t').collect();
                Some((f[0].to_string(), (f[2].to_string(), f.get(3)?.parse().ok()?)))
            })
            .collect();
    let mut seen_terms = HashSet::new();
    let mut done: HashSet<u32> = HashSet::new();
    let mut todo: Vec<u32> = names.iter().map(|n| env.find(n).unwrap_or_else(|| panic!("no {n}")).name).collect();
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
    let mut listed = Vec::new();
    let mut blocks = Vec::new();
    for &n in &done {
        let Some(d) = env.by_name(n) else { continue };
        decls += 1;
        if let Kind::Induct { all, .. } = &d.kind {
            if all[0] == n && !d.unsafe_ {
                blocks.push(env.name(n));
            }
        }
        if matches!(d.kind, Kind::Axiom | Kind::Def { .. } | Kind::Thm { .. } | Kind::Opaque { .. }) {
            if !d.unsafe_ {
                listed.push(env.name(n));
            }
            match rows.get(&env.name(n)) {
                Some((_, r)) => {
                    checked += 1;
                    total += r;
                }
                None => missing += 1,
            }
        }
    }
    if let Some(f) = out_file {
        listed.sort();
        std::fs::write(&f, listed.join("\n") + "\n").unwrap();
        blocks.sort();
        std::fs::write(format!("{f}.blocks"), blocks.join("\n") + "\n").unwrap();
    }
    let all: u64 = rows.values().map(|r| r.1).sum();
    println!(
        "closure {decls} declarations, {checked} with rows ({missing} without), {total} rows of {all} in the file ({:.1}%)",
        100.0 * total as f64 / all as f64
    );
}
