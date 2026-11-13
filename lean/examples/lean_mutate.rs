//! Refusal tests on an export of `measure/Export.lean`. For a sample of
//! definitions and theorems:
//!
//! * Lean-level mutations: the declaration's value paired with another
//!   declaration's type (`swap`), and the type's first level parameter
//!   raised by one (`level`). Lean refuses these (almost always); so must
//!   the prover, or the checker on whatever it writes.
//! * Tampering: the honest derivation with one term row that the goal
//!   reaches changed (a child, a level, a constant, an index). When the
//!   checker accepts, the changed claim is proved again from scratch by a
//!   fresh builder; a claim the builder refuses is an alarm.
//!
//! One line per test on stdout: name, test, outcome, detail.
//!
//! Usage: `lean_mutate <export> [--every N] [--limit N] [--tamper K]
//! [--budget ROWS] [--workers N]`.

use cas_lean::Checker;
use cas_lean::derivation::{DeclKind, Derivation, Ix, Key, TermRow};
use cas_lean::derive::{Builder, Cited, Source};
use cas_lean::expr::{X, from_row};
use cas_lean::import::{Env, Kind};
use std::collections::{HashMap, HashSet};
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

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn builder(env: &Arc<Env>, budget: usize) -> Builder {
    let mut b = Builder::new();
    b.source = Some(std::rc::Rc::new(Shared(env.clone())));
    b.budget = budget;
    b
}

fn quiet<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|p| {
        p.downcast_ref::<String>()
            .cloned()
            .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default()
    })
}

/// Proves and checks the claim (kind, levels, ty, value) from scratch:
/// Ok(true) when both accept, Ok(false) when either refuses, Err when the
/// budget ran out or something panicked.
fn prove(
    env: &Arc<Env>,
    budget: usize,
    kind: DeclKind,
    levels: u32,
    ty: &X,
    value: Option<&X>,
) -> Result<bool, String> {
    let mut b = builder(env, budget);
    quiet(|| -> bool {
        let Ok(ty) = b.row_of(ty) else { return false };
        let value = match value.map(|v| b.row_of(v)) {
            Some(Ok(v)) => Some(v),
            Some(Err(_)) => return false,
            None => None,
        };
        match b.check_decl(kind, levels, ty, value) {
            Ok(goal) => Checker::check(&b.t, &goal).is_ok(),
            Err(e) => {
                if std::env::var("MUTATE_DEBUG").is_ok() {
                    eprintln!("builder refuses: {e}");
                }
                false
            }
        }
    })
}

/// The term rows reachable from e.
fn reach(t: &Derivation, e: Ix, out: &mut HashSet<Ix>) {
    let mut todo = vec![e];
    while let Some(e) = todo.pop() {
        if !out.insert(e) {
            continue;
        }
        match t.terms[e as usize] {
            TermRow::App(a, b) | TermRow::Lam(a, b) | TermRow::Pi(a, b) => todo.extend([a, b]),
            TermRow::Let(a, v, b) => todo.extend([a, v, b]),
            TermRow::Proj(_, _, x) => todo.push(x),
            _ => {}
        }
    }
}

/// A changed copy of term row j, whose children stay before it.
fn mutate(t: &Derivation, j: Ix, rng: &mut Rng) -> TermRow {
    let other = |rng: &mut Rng| rng.below(j as usize) as Ix;
    match t.terms[j as usize].clone() {
        TermRow::BVar(k) => TermRow::BVar(k + 1),
        TermRow::FVar(n) => TermRow::FVar(n + 1),
        TermRow::Sort(l) => TermRow::Sort((l + 1 + rng.below(t.levels.len() - 1) as Ix) % t.levels.len() as Ix),
        TermRow::Const(d, ls) => TermRow::Const((d + 1 + rng.below(t.decls.len() - 1) as Ix) % t.decls.len() as Ix, ls),
        TermRow::App(_, b) if rng.below(2) == 0 => TermRow::App(other(rng), b),
        TermRow::App(a, _) => TermRow::App(a, other(rng)),
        TermRow::Lam(_, b) if rng.below(2) == 0 => TermRow::Lam(other(rng), b),
        TermRow::Lam(a, _) => TermRow::Lam(a, other(rng)),
        TermRow::Pi(_, b) if rng.below(2) == 0 => TermRow::Pi(other(rng), b),
        TermRow::Pi(a, _) => TermRow::Pi(a, other(rng)),
        TermRow::Let(a, v, b) => match rng.below(3) {
            0 => TermRow::Let(other(rng), v, b),
            1 => TermRow::Let(a, other(rng), b),
            _ => TermRow::Let(a, v, other(rng)),
        },
        TermRow::Lit(mut v) => {
            match v.first_mut() {
                Some(x) if *x < 255 => *x += 1,
                _ => v.insert(0, 1),
            }
            TermRow::Lit(v)
        }
        TermRow::Str(mut s) => {
            s.push(b'x');
            TermRow::Str(s)
        }
        TermRow::Proj(s, i, x) => TermRow::Proj(s, i + 1, x),
    }
}

/// Whether every declaration row of t is the export's declaration of its
/// key (type, value when given, kind, levels and rules): what the citation
/// check outside the checker establishes.
fn cited_honestly(env: &Env, t: &Derivation) -> bool {
    let mut memo = HashMap::new();
    t.decls.iter().all(|d| {
        let Some(c) = env.cite(&d.key) else { return true };
        if c.kind != d.kind || c.levels != d.levels || from_row(t, d.ty, &mut memo) != c.ty {
            return false;
        }
        if let (Some(v), Some(cv)) = (d.value, &c.value)
            && from_row(t, v, &mut memo) != *cv
        {
            return false;
        }
        if let cas_lean::derivation::Info::Rec { rules, .. } = &d.info {
            return rules.len() == c.rules.len()
                && rules
                    .iter()
                    .zip(&c.rules)
                    .all(|(r, (k, n, rhs))| r.ctor == *k && r.fields == *n && from_row(t, r.rhs, &mut memo) == *rhs);
        }
        true
    })
}

fn run(env: &Arc<Env>, i: usize, j: usize, tamper: usize, budget: usize) -> Vec<(String, String, String)> {
    let d = &env.decls[i];
    let mut out = vec![];
    let mut memo = HashMap::new();
    let (kind, _) = env.kind_info(d);
    let levels = d.lparams.len() as u32;
    let value_of = |d: &cas_lean::import::Decl| match d.kind {
        Kind::Def { value, .. } | Kind::Thm { value } => Some(value),
        _ => None,
    };
    let Ok(ty) = env.term(d.ty, &d.lparams, &mut memo) else { return out };
    let Some(Ok(value)) = value_of(d).map(|v| env.term(v, &d.lparams, &mut memo)) else { return out };
    let verdict = |r: Result<bool, String>| match r {
        Ok(true) => "accepted".to_string(),
        Ok(false) => "refused".to_string(),
        Err(m) if m.contains("budget") => "budget".to_string(),
        Err(m) => format!("panic {m}"),
    };

    // swap: this value at another declaration's type.
    let o = &env.decls[j];
    if o.ty != d.ty {
        let mut m2 = HashMap::new();
        if let Ok(oty) = env.term(o.ty, &o.lparams, &mut m2) {
            let lv = levels.max(o.lparams.len() as u32);
            out.push(("swap".into(), verdict(prove(env, budget, kind, lv, &oty, Some(&value))), env.name(o.name)));
        }
    }
    // level: the type's first level parameter raised by one.
    if levels > 0 {
        use cas_lean::level::Level;
        let ls: Vec<Level> =
            (0..levels).map(|n| if n == 0 { Level::Param(0).succ() } else { Level::Param(n) }).collect();
        let raised = cas_lean::expr::inst_levels(&ty, &ls);
        if raised != ty {
            out.push(("level".into(), verdict(prove(env, budget, kind, levels, &raised, Some(&value))), String::new()));
        }
    }

    // Tampering with the honest derivation.
    let mut b = builder(env, budget);
    let honest = quiet(|| -> Option<cas_lean::derivation::Goal> {
        let t = b.row_of(&ty).ok()?;
        let v = b.row_of(&value).ok()?;
        let goal = b.check_decl(kind, levels, t, Some(v)).ok()?;
        Checker::check(&b.t, &goal).ok()?;
        Some(goal)
    });
    let Ok(Some(goal)) = honest else {
        out.push(("honest".into(), "not proved".into(), String::new()));
        return out;
    };
    let mut reached = HashSet::new();
    reach(&b.t, goal.ty, &mut reached);
    reach(&b.t, goal.value.unwrap(), &mut reached);
    let mut reached: Vec<Ix> = reached.into_iter().filter(|j| *j > 0).collect();
    reached.sort();
    let mut rng = Rng(0x9e3779b97f4a7c15 ^ (i as u64).wrapping_mul(0x2545f4914f6cdd1d));
    for _ in 0..tamper {
        let j = reached[rng.below(reached.len())];
        let mut t = b.t.clone();
        let row = mutate(&t, j, &mut rng);
        let what = format!("{:?} -> {:?}", t.terms[j as usize], row);
        t.terms[j as usize] = row;
        let accepted = quiet(|| Checker::check(&t, &goal).is_ok()).unwrap_or(false);
        if !accepted {
            out.push(("tamper".into(), "refused".into(), what));
            continue;
        }
        let mut m = HashMap::new();
        let (ty2, v2) = (from_row(&t, goal.ty, &mut m), from_row(&t, goal.value.unwrap(), &mut m));
        if !cited_honestly(env, &t) {
            out.push(("tamper".into(), "forged citation".into(), what));
            continue;
        }
        if ty2 == ty && v2 == value {
            out.push(("tamper".into(), "same claim".into(), what));
            continue;
        }
        if std::env::var("MUTATE_DEBUG").is_ok() {
            eprintln!("{what}\nbefore {ty:?}\n after {ty2:?}");
        }
        let again = verdict(prove(env, budget, kind, levels, &ty2, Some(&v2)));
        let outcome = if again == "refused" { "ALARM".to_string() } else { format!("valid claim ({again})") };
        out.push(("tamper".into(), outcome, what));
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let opt = |k: &str| args.iter().position(|a| a == k).map(|i| args[i + 1].clone());
    let every: usize = opt("--every").map_or(100, |s| s.parse().unwrap());
    let limit: usize = opt("--limit").map_or(usize::MAX, |s| s.parse().unwrap());
    let tamper: usize = opt("--tamper").map_or(10, |s| s.parse().unwrap());
    let budget: usize = opt("--budget").map_or(300_000, |s| s.parse().unwrap());
    let workers: usize = opt("--workers").map_or(4, |s| s.parse().unwrap());
    let text = std::fs::read_to_string(&args[1]).unwrap();
    let env = Arc::new(Env::parse(&text).unwrap());
    let only = opt("--only");
    let picked: Vec<usize> = (0..env.decls.len())
        .filter(|i| only.as_ref().is_none_or(|n| env.name(env.decls[*i].name) == *n))
        .filter(|i| !env.decls[*i].unsafe_ && matches!(env.decls[*i].kind, Kind::Def { .. } | Kind::Thm { .. }))
        .step_by(every)
        .take(limit)
        .collect();
    let next = AtomicUsize::new(0);
    let lock = Mutex::new(());
    std::thread::scope(|s| {
        for _ in 0..workers {
            std::thread::Builder::new()
                .stack_size(1 << 30)
                .spawn_scoped(s, || {
                    loop {
                        let k = next.fetch_add(1, Ordering::Relaxed);
                        if k >= picked.len() {
                            break;
                        }
                        let (i, j) = (picked[k], picked[(k + 1) % picked.len()]);
                        let rows = run(&env, i, j, tamper, budget);
                        let _g = lock.lock().unwrap();
                        for (test, outcome, detail) in rows {
                            println!("{}\t{test}\t{outcome}\t{detail}", env.name(env.decls[i].name));
                        }
                    }
                })
                .unwrap();
        }
    });
}
