//! Long Lean bundles (spec/lean/bundle.md) without the succinct part:
//! `lean_bundle prove EXPORT OUT [--cache DIR] [--assume A,B] THEOREM...`
//! and `lean_bundle verify FILE`.
use cas_lean::bundle::{Bundle, Evidence};
use cas_lean::import::Env;
use std::collections::BTreeSet;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a.first().map(String::as_str) {
        Some("prove") => {
            let env = Env::parse(&std::fs::read_to_string(&a[1]).unwrap()).unwrap();
            let mut cache = None;
            let mut assumed = BTreeSet::new();
            let mut th = vec![];
            let mut i = 3;
            while i < a.len() {
                match a[i].as_str() {
                    "--cache" => {
                        cache = Some(std::path::PathBuf::from(&a[i + 1]));
                        i += 2
                    }
                    "--assume" => {
                        assumed.extend(a[i + 1].split(',').map(String::from));
                        i += 2
                    }
                    _ => {
                        th.push(a[i].clone());
                        i += 1
                    }
                }
            }
            if let Some(c) = &cache {
                std::fs::create_dir_all(c).unwrap()
            }
            let t0 = std::time::Instant::now();
            let (b, r) = Bundle::prove(&env, &th, &assumed, &BTreeSet::new(), cache.as_deref()).unwrap();
            let failed: Vec<_> = r.iter().filter(|p| p.outcome.is_err()).collect();
            let rows: u64 = r.iter().filter_map(|p| p.outcome.as_ref().ok()).sum();
            println!(
                "{} units, {rows} rows, {} cached, {} failed, {:.1} s",
                r.len(),
                r.iter().filter(|p| p.cached).count(),
                failed.len(),
                t0.elapsed().as_secs_f64()
            );
            for f in &failed {
                println!("failed: {}", f.outcome.as_ref().unwrap_err())
            }
            if let Some(b) = b {
                let bytes = cas_lean::unit::encode(&b);
                std::fs::write(&a[2], &bytes).unwrap();
                println!("wrote {} ({} KiB)", a[2], bytes.len() / 1024);
            } else {
                std::process::exit(1)
            }
        }
        Some("verify") => {
            let t0 = std::time::Instant::now();
            let b: Bundle = cas_lean::unit::decode(&std::fs::read(&a[1]).unwrap()).unwrap();
            match b.verify() {
                Ok(r) => {
                    println!(
                        "VERIFIED in {:.1} s: {} declarations, {} units long ({} rows), {} short, {} assumed",
                        t0.elapsed().as_secs_f64(),
                        r.decls,
                        r.long.len(),
                        r.long.iter().map(|x| x.1).sum::<u64>(),
                        r.short.len(),
                        r.assumed.len()
                    );
                    for (n, t) in &r.statements {
                        println!("theorem {n} : {t}")
                    }
                    println!("axioms: {}", r.axioms.join(", "));
                    println!("assumed: {}", r.assumed.join(", "));
                    println!("statement blake3:{}", blake3::Hash::from(r.statement_digest).to_hex());
                    let _ = Evidence::Assumed;
                }
                Err(e) => {
                    println!("REFUSED: {e}");
                    std::process::exit(1)
                }
            }
        }
        Some("tamper") => {
            // Each change must be refused.
            let b: Bundle = cas_lean::unit::decode(&std::fs::read(&a[1]).unwrap()).unwrap();
            let th = b.theorems[0].clone();
            let mut cases: Vec<(&str, Bundle)> = vec![];
            let mut x = b.clone();
            let k = x.units.iter().position(|u| u.0 == th).unwrap();
            x.units.remove(k);
            cases.push(("theorem without evidence", x));
            let mut x = b.clone();
            x.units.push(x.units[0].clone());
            cases.push(("evidence twice", x));
            let mut x = b.clone();
            x.decls.pop();
            cases.push(("a declaration dropped", x));
            let mut x = b.clone();
            x.theorems = vec!["Nat.no_such".into()];
            cases.push(("theorem not declared", x));
            // Another unit's proof given for the theorem.
            let mut x = b.clone();
            let other = x.units.iter().position(|u| u.0 != th && matches!(u.1, Evidence::Long(_))).unwrap();
            x.units[k].1 = x.units[other].1.clone();
            cases.push(("another unit's proof", x));
            // The theorem's statement changed: swap its type with another theorem's.
            let mut x = b.clone();
            let ti = x
                .decls
                .iter()
                .position(|t| t.contains("thm") && t.lines().any(|l| l.contains(&hexname(&th))))
                .unwrap_or(0);
            let t = x.decls[ti].clone();
            let mut lines: Vec<String> = t.lines().map(String::from).collect();
            let last = lines.len() - 1;
            let w: Vec<String> = lines[last].split(' ').map(String::from).collect();
            let mut w2 = w.clone();
            w2[2] = (w[2].parse::<u64>().unwrap() - 1).to_string();
            lines[last] = w2.join(" ");
            x.decls[ti] = lines.join("\n") + "\n";
            cases.push(("the theorem's type changed", x));
            let mut bad = 0;
            for (what, x) in cases {
                match x.verify() {
                    Ok(_) => {
                        bad += 1;
                        println!("ACCEPTED (bad): {what}")
                    }
                    Err(e) => println!("refused, {what}: {}", e.chars().take(150).collect::<String>()),
                }
            }
            if bad > 0 {
                std::process::exit(1)
            }
        }
        _ => eprintln!("usage: lean_bundle prove EXPORT OUT [--cache DIR] [--assume A,B] THEOREM... | verify FILE"),
    }
}

fn hexname(n: &str) -> String {
    n.rsplit('.').next().unwrap().bytes().map(|b| format!("{b:02x}")).collect()
}
