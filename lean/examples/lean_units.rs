//! Checks `cas lean`'s library path on an export: each declaration is
//! written as its own text and read back into a fresh environment (which
//! must give the same text), then every N-th unit is proved, checked, and
//! checked again after tampering with one cited row.
//!
//! Usage: `lean_units <export> [--every N] [--budget ROWS] [--workers N]`

use cas_lean::derivation::{Info, TermRow};
use cas_lean::import::Env;
use cas_lean::unit;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let opt = |k: &str| args.iter().position(|a| a == k).map(|i| args[i + 1].parse::<usize>().unwrap());
    let every = opt("--every").unwrap_or(1);
    let budget = opt("--budget").unwrap_or(2_000_000);
    let workers = opt("--workers").unwrap_or(4);
    let t0 = std::time::Instant::now();
    let whole = Env::parse(&std::fs::read_to_string(&args[1]).unwrap()).unwrap();
    let texts: Vec<String> = (0..whole.decls.len()).map(|i| whole.decl_text(i)).collect();
    drop(whole);
    let bytes: usize = texts.iter().map(|t| t.len()).sum();
    let mut env = Env::default();
    for t in &texts {
        env.absorb(t).unwrap();
    }
    for (i, t) in texts.iter().enumerate() {
        assert_eq!(&env.decl_text(i), t, "declaration {i} does not round-trip");
    }
    eprintln!(
        "{} declarations, {} MB of texts, round trip ok ({:.1} s)",
        texts.len(),
        bytes >> 20,
        t0.elapsed().as_secs_f64()
    );
    drop(texts);
    let env = Arc::new(env);
    let todo: Vec<usize> = (0..env.decls.len()).filter(|i| unit::is_unit(&env, *i)).step_by(every).collect();
    std::panic::set_hook(Box::new(|_| {}));
    let next = Arc::new(AtomicUsize::new(0));
    let todo = Arc::new(todo);
    let hs: Vec<_> = (0..workers)
        .map(|_| {
            let (env, next, todo) = (env.clone(), next.clone(), todo.clone());
            std::thread::Builder::new()
                .stack_size(1 << 30)
                .spawn(move || {
                    loop {
                        let k = next.fetch_add(1, Ordering::SeqCst);
                        let Some(&i) = todo.get(k) else { break };
                        let name = env.name(env.decls[i].name);
                        let (status, msg) = match unit::prove(&env, i, budget) {
                            Err(f) => (f.stage, f.message),
                            Ok(p) => {
                                let bytes = unit::encode(&p);
                                let q: unit::Proof = unit::decode(&bytes).unwrap();
                                assert_eq!(unit::encode(&q), bytes);
                                match unit::check(&env, i, &q) {
                                    Err(e) => ("check".into(), e),
                                    Ok(()) => match tamper(&q).map(|t| unit::check(&env, i, &t)) {
                                        Some(Ok(())) => ("tamper-accepted".into(), String::new()),
                                        _ => ("ok".into(), format!("{} bytes", bytes.len())),
                                    },
                                }
                            }
                        };
                        let msg: String = msg.chars().take(300).map(|c| if c == '\n' { ' ' } else { c }).collect();
                        println!("{name}\t{status}\t{msg}");
                    }
                })
                .unwrap()
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
}

/// The proof with the last cited declaration's type swapped for another
/// term of the derivation (if there is one to swap to), or a recursor's
/// information changed.
fn tamper(p: &unit::Proof) -> Option<unit::Proof> {
    let mut q = p.clone();
    let t = &mut q.derivation;
    let r = t.decls.last_mut()?;
    if let Info::Rec { minors, .. } = &mut r.info {
        *minors += 1;
        return Some(q);
    }
    let other = (0..t.terms.len() as u32).find(|x| *x != r.ty && matches!(t.terms[*x as usize], TermRow::Sort(_)))?;
    r.ty = other;
    Some(q)
}
