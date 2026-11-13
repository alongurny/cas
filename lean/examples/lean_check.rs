//! Proves and checks Lean declarations from an export of
//! `measure/Export.lean`: for each axiom, definition, theorem and opaque
//! declaration, the builder cites what it needs from the export, writes
//! the tables, and the checker checks them. One line per declaration on
//! stdout: name, kind, status, rows, terms, milliseconds, message.
//!
//! Usage: `lean_check <export> [--only NAME] [--every N] [--budget ROWS]
//! [--workers N] [--limit N] [--blocks]`. With `--blocks`, each inductive
//! block instead: checked, and its recursors compared with Lean's.

use cas_lean::Checker;
use cas_lean::derive::{Builder, Cited, Source};
use cas_lean::import::{Env, Kind};
use cas_lean::tables::Key;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

struct Shared(Arc<Env>);
impl Source for Shared {
    fn cite(&self, key: &Key) -> Option<Cited> {
        self.0.cite(key)
    }
    fn name(&self, key: &Key) -> Option<String> {
        self.0.by_key(key).map(|d| self.0.name(d.name))
    }
}

fn one(env: &Arc<Env>, i: usize, budget: usize) -> (String, usize, usize, String) {
    let d = &env.decls[i];
    let mut b = Builder::new();
    b.source = Some(std::rc::Rc::new(Shared(env.clone())));
    b.budget = budget;
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), (String, String)> {
        let mut memo = HashMap::new();
        let (kind, _) = env.kind_info(d);
        let ty = env.term(d.ty, &d.lparams, &mut memo).map_err(|e| ("import".into(), e))?;
        let ty = b.row_of(&ty).map_err(|e| ("cite".into(), e))?;
        let value = match d.kind {
            Kind::Def { value, .. } | Kind::Thm { value } | Kind::Opaque { value } => {
                let v = env.term(value, &d.lparams, &mut memo).map_err(|e| ("import".into(), e))?;
                Some(b.row_of(&v).map_err(|e| ("cite".into(), e))?)
            }
            _ => None,
        };
        let goal = b.check_decl(kind, d.lparams.len() as u32, ty, value).map_err(|e| ("prover".into(), e))?;
        Checker::check(&b.t, &goal).map_err(|e| ("checker".into(), e))?;
        Ok(())
    }));
    let (status, msg) = match run {
        Ok(Ok(())) => ("ok".to_string(), String::new()),
        Ok(Err((s, m))) => (s, m),
        Err(p) => {
            let m = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            if m.contains("budget") { ("budget".into(), String::new()) } else { ("panic".into(), m) }
        }
    };
    (status, b.t.rows.len(), b.t.terms.len(), msg)
}

/// The inductive block whose first type is declaration i: checked as a
/// block (or a nested block), and the recursors built compared with
/// Lean's.
fn block(env: &Arc<Env>, i: usize, budget: usize) -> (String, usize, usize, String) {
    let d = &env.decls[i];
    let Kind::Induct { params, nested, all, .. } = &d.kind else { unreachable!() };
    let mut b = Builder::new();
    b.source = Some(std::rc::Rc::new(Shared(env.clone())));
    b.budget = budget;
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), (String, String)> {
        let cite = |b: &mut Builder, n: u32| b.cite(&env.key(n)).map_err(|e| ("cite".to_string(), e));
        let mut types = vec![];
        let mut ctors = vec![];
        for t in all {
            types.push(cite(&mut b, *t)?);
            let Some(Kind::Induct { ctors: cs, .. }) = env.by_name(*t).map(|x| &x.kind) else {
                return Err(("import".into(), "a type of the block is missing".into()));
            };
            ctors.push(cs.iter().map(|c| cite(&mut b, *c)).collect::<Result<Vec<_>, _>>()?);
        }
        let first = env.name(all[0]);
        let mut rec_names: Vec<String> = all.iter().map(|t| format!("{}.rec", env.name(*t))).collect();
        let mut j = 1;
        while env.find(&format!("{first}.rec_{j}")).is_some() {
            rec_names.push(format!("{first}.rec_{j}"));
            j += 1;
        }
        let rec_decls: Vec<_> = rec_names
            .iter()
            .map(|n| env.find(n).ok_or(("import".to_string(), format!("no {n}"))))
            .collect::<Result<_, _>>()?;
        let rec_keys: Vec<Key> = rec_decls.iter().map(|r| env.key(r.name)).collect();
        let levels = d.lparams.len() as u32;
        let recs = if *nested > 0 {
            let goal =
                b.nested_block(levels, *params, &types, &ctors, &rec_keys).map_err(|e| ("prover".to_string(), e))?;
            Checker::check_nested(&b.t, &goal).map_err(|e| ("checker".to_string(), e))?;
            goal.recursors
        } else {
            let goal =
                b.inductive_block(levels, *params, &types, &ctors, &rec_keys).map_err(|e| ("prover".to_string(), e))?;
            Checker::check_block(&b.t, &goal).map_err(|e| ("checker".to_string(), e))?;
            goal.recursors
        };
        // The recursors are Lean's: same type and rules, term for term.
        for (r, ld) in recs.iter().zip(&rec_decls) {
            let mut memo = HashMap::new();
            let ty = env.term(ld.ty, &ld.lparams, &mut memo).map_err(|e| ("import".to_string(), e))?;
            let ty = b.row_of(&ty).map_err(|e| ("cite".to_string(), e))?;
            if ty != b.t.decls[*r as usize].ty {
                return Err((
                    "differs".into(),
                    format!(
                        "{}: the type: lean {} built {}",
                        env.name(ld.name),
                        b.show(ty),
                        b.show(b.t.decls[*r as usize].ty)
                    ),
                ));
            }
            let Kind::Rec { rules, .. } = &ld.kind else { unreachable!() };
            let cas_lean::tables::Info::Rec { rules: built, .. } = b.t.decls[*r as usize].info.clone() else {
                unreachable!()
            };
            for ((c, n, rhs), br) in rules.iter().zip(&built) {
                let rhs = env.term(*rhs, &ld.lparams, &mut memo).map_err(|e| ("import".to_string(), e))?;
                let rhs = b.row_of(&rhs).map_err(|e| ("cite".to_string(), e))?;
                if env.key(*c) != br.ctor || *n != br.fields || rhs != br.rhs {
                    return Err(("differs".into(), format!("{}: the rule for {}", env.name(ld.name), env.name(*c))));
                }
            }
        }
        Ok(())
    }));
    let (status, msg) = match run {
        Ok(Ok(())) => ("ok".to_string(), String::new()),
        Ok(Err((s, m))) => (s, m),
        Err(p) => {
            let m = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            if m.contains("budget") { ("budget".into(), String::new()) } else { ("panic".into(), m) }
        }
    };
    (status, b.t.rows.len(), b.t.terms.len(), msg)
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let opt = |k: &str| args.iter().position(|a| a == k).map(|i| args[i + 1].clone());
    let text = std::fs::read_to_string(&args[1]).expect("an export");
    let env = Arc::new(Env::parse(&text).expect("a valid export"));
    drop(text);
    let every: usize = opt("--every").map_or(1, |s| s.parse().unwrap());
    let budget: usize = opt("--budget").map_or(2_000_000, |s| s.parse().unwrap());
    let workers: usize = opt("--workers").map_or(4, |s| s.parse().unwrap());
    let limit: usize = opt("--limit").map_or(usize::MAX, |s| s.parse().unwrap());
    let only = opt("--only");
    let blocks = args.iter().any(|a| a == "--blocks");
    let todo: Vec<usize> = (0..env.decls.len())
        .filter(|i| {
            let d = &env.decls[*i];
            (if blocks {
                matches!(&d.kind, Kind::Induct { all, .. } if all[0] == d.name)
            } else {
                matches!(d.kind, Kind::Axiom | Kind::Def { .. } | Kind::Thm { .. } | Kind::Opaque { .. })
            }) && only.as_ref().is_none_or(|n| env.name(d.name) == *n)
        })
        .step_by(every)
        .take(limit)
        .collect();
    std::panic::set_hook(Box::new(|_| {}));
    let next = Arc::new(AtomicUsize::new(0));
    let out = Arc::new(Mutex::new(()));
    let todo = Arc::new(todo);
    let hs: Vec<_> = (0..workers)
        .map(|_| {
            let (env, next, out, todo) = (env.clone(), next.clone(), out.clone(), todo.clone());
            std::thread::Builder::new()
                .stack_size(1 << 30)
                .spawn(move || {
                    loop {
                        let k = next.fetch_add(1, Ordering::SeqCst);
                        let Some(&i) = todo.get(k) else { break };
                        eprintln!("start {}", env.name(env.decls[i].name));
                        let t0 = std::time::Instant::now();
                        let (status, rows, terms, msg) =
                            if blocks { block(&env, i, budget) } else { one(&env, i, budget) };
                        let d = &env.decls[i];
                        let kind = match d.kind {
                            Kind::Axiom => "axiom",
                            Kind::Def { .. } => "def",
                            Kind::Thm { .. } => "thm",
                            Kind::Induct { .. } => "block",
                            _ => "opaque",
                        };
                        let msg: String =
                            msg.chars().take(1500).map(|c| if c == '\t' || c == '\n' { ' ' } else { c }).collect();
                        let _g = out.lock().unwrap();
                        println!(
                            "{}\t{kind}\t{status}\t{rows}\t{terms}\t{}\t{msg}",
                            env.name(d.name),
                            t0.elapsed().as_millis()
                        );
                    }
                })
                .unwrap()
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
}
