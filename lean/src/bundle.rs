//! A Lean bundle: what a theorem states and the evidence for every
//! declaration it rests on (spec/lean/bundle.md).
//!
//! The statement is the declarations themselves, as the canonical text
//! [`Env::decl_text`] writes: the theorems' and every declaration their
//! types, values and blocks reach, down to the axioms. Each unit of that
//! closure ([`unit::is_unit`]) has exactly one piece of evidence:
//!
//! - **long**: its derivation ([`unit::Proof`]), which the verifier checks
//!   with the derivation checker against these declarations;
//! - **short**: it is among the units one succinct proof covers (the flock
//!   Lean bundle the bundle carries, checked by the caller);
//! - **assumed**: nothing; the verifier lists it.
//!
//! A unit with no evidence, a declaration that names one missing from the
//! bundle, or an unsafe declaration is refused.

use crate::import::{Env, Kind};
use crate::unit::{self, Proof};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{Arc, Mutex, mpsc};

/// The names of Lean's primitives, which a proof about literals cites
/// though no term names them.
pub const PRIMS: &[&str] = &[
    "Nat",
    "Nat.zero",
    "Nat.succ",
    "Nat.add",
    "Nat.sub",
    "Nat.mul",
    "Nat.pow",
    "Nat.gcd",
    "Nat.mod",
    "Nat.div",
    "Nat.beq",
    "Nat.ble",
    "Nat.land",
    "Nat.lor",
    "Nat.xor",
    "Nat.shiftLeft",
    "Nat.shiftRight",
    "Bool.true",
    "Bool.false",
    "String",
    "String.mk",
    "Char",
    "Char.ofNat",
    "List.nil",
    "List.cons",
];

/// Lean's three standard axioms.
pub const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Evidence {
    /// The unit's proof, encoded as [`unit::encode`] writes it.
    Long(Vec<u8>),
    /// Covered by the bundle's succinct proof.
    Short,
    /// Taken as given.
    Assumed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bundle {
    /// The theorems the bundle is about, by Lean name.
    pub theorems: Vec<String>,
    /// Every declaration of the closure, as [`Env::decl_text`] writes it.
    pub decls: Vec<String>,
    /// The evidence for each unit, by the unit's Lean name.
    pub units: Vec<(String, Evidence)>,
    /// The succinct proof of the `Short` units: a flock Lean bundle file
    /// (its format 1), empty when no unit is short.
    pub short: Vec<u8>,
}

/// What a bundle establishes, once verified.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// Each theorem's name and type, as the checker prints terms.
    pub statements: Vec<(String, String)>,
    /// BLAKE3 of the bincode encoding of `decls`: the statement's digest.
    pub statement_digest: [u8; 32],
    pub decls: usize,
    /// Units checked here, with their rows.
    pub long: Vec<(String, u64)>,
    /// Units the caller must find among the succinct proof's.
    pub short: Vec<String>,
    pub assumed: Vec<String>,
    /// The axioms of the closure (each is also a unit above).
    pub axioms: Vec<String>,
    /// Lean's quotient builtins in the closure (no proof: Lean's own).
    pub builtins: Vec<String>,
}

/// Declarations `roots` reach in `env`: through their terms, blocks,
/// recursors, and Lean's primitives when a term has a literal. Positions
/// in `env`, in the order reached.
pub fn closure(env: &Env, roots: &[String]) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut todo: Vec<String> = roots.to_vec();
    for r in roots {
        if env.index_of(r).is_none() {
            return Err(format!("no declaration {r}"));
        }
    }
    let mut prims = false;
    while let Some(name) = todo.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Some(i) = env.index_of(&name) else { continue };
        out.push(i);
        let mut more = env.references(i);
        more.extend(recursors(env, i));
        if !prims && env.has_literal(i) {
            prims = true;
            more.extend(PRIMS.iter().map(|p| p.to_string()));
        }
        todo.extend(more.into_iter().filter(|n| !seen.contains(n)));
    }
    Ok(out)
}

/// The recursors of an inductive type's block that `env` has (one per
/// type, then a nested block's `rec_1`, `rec_2`, ...).
fn recursors(env: &Env, i: usize) -> Vec<String> {
    let Kind::Induct { all, .. } = &env.decls[i].kind else { return vec![] };
    let first = env.name(all[0]);
    let mut v: Vec<String> = all.iter().map(|t| format!("{}.rec", env.name(*t))).collect();
    v.extend((1..).map(|j| format!("{first}.rec_{j}")).take_while(|n| env.index_of(n).is_some()));
    v
}

/// The declarations of `idx` as one environment, and their texts.
pub fn sub_env(env: &Env, idx: &[usize]) -> Result<(Env, Vec<String>), String> {
    let mut sub = Env::default();
    let mut texts = Vec::with_capacity(idx.len());
    for &i in idx {
        let t = env.decl_text(i);
        sub.absorb(&t)?;
        texts.push(t);
    }
    Ok((sub, texts))
}

/// The units of an environment, by position.
pub fn units(env: &Env) -> Vec<usize> {
    (0..env.decls.len()).filter(|i| unit::is_unit(env, *i)).collect()
}

fn workers() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get())
}

/// Runs `f` on each job on worker threads with large stacks (terms can
/// be deep); results in the jobs' order. A panic is an error.
pub fn par_map<J: Send + 'static, R: Send + 'static>(
    jobs: Vec<J>,
    f: impl Fn(J) -> Result<R, String> + Send + Sync + 'static,
) -> Vec<Result<R, String>> {
    let n = jobs.len();
    let (jtx, jrx) = mpsc::channel::<(usize, J)>();
    for (k, j) in jobs.into_iter().enumerate() {
        jtx.send((k, j)).unwrap();
    }
    drop(jtx);
    let jrx = Arc::new(Mutex::new(jrx));
    let f = Arc::new(f);
    let (tx, rx) = mpsc::channel::<(usize, Result<R, String>)>();
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let hs: Vec<_> = (0..workers().min(n.max(1)))
        .map(|_| {
            let (jrx, tx, f) = (jrx.clone(), tx.clone(), f.clone());
            std::thread::Builder::new()
                .stack_size(1 << 30)
                .spawn(move || {
                    loop {
                        let job = jrx.lock().unwrap().recv();
                        let Ok((k, j)) = job else { break };
                        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(j)))
                            .unwrap_or_else(|_| Err("panicked".into()));
                        if tx.send((k, r)).is_err() {
                            break;
                        }
                    }
                })
                .unwrap()
        })
        .collect();
    drop(tx);
    let mut out: Vec<Option<Result<R, String>>> = (0..n).map(|_| None).collect();
    for (k, r) in rx {
        out[k] = Some(r);
    }
    for h in hs {
        let _ = h.join();
    }
    std::panic::set_hook(hook);
    out.into_iter().map(|r| r.unwrap_or_else(|| Err("lost".into()))).collect()
}

impl Bundle {
    /// The statement's digest: BLAKE3 of the encoding of the declarations.
    pub fn statement_digest(&self) -> [u8; 32] {
        *blake3::hash(&unit::encode(&self.decls)).as_bytes()
    }

    /// Checks everything but the succinct proof: the declarations form a
    /// closed environment holding the theorems, every unit has exactly one
    /// piece of evidence, and every long proof is accepted. The caller
    /// checks that the succinct proof covers `Report::short`.
    pub fn verify(&self) -> Result<Report, String> {
        let mut env = Env::default();
        for (k, t) in self.decls.iter().enumerate() {
            env.absorb(t).map_err(|e| format!("declaration {k}: {e}"))?;
        }
        let env = Arc::new(env);
        // Closed: whatever a declaration names is here.
        for i in 0..env.decls.len() {
            let d = &env.decls[i];
            let name = env.name(d.name);
            if d.unsafe_ {
                return Err(format!("{name} is unsafe"));
            }
            for r in env.references(i).into_iter().chain(recursors(&env, i)) {
                if env.index_of(&r).is_none() {
                    return Err(format!("{name} names {r}, which the bundle does not declare"));
                }
            }
        }
        let mut report =
            Report { statement_digest: self.statement_digest(), decls: env.decls.len(), ..Report::default() };
        for t in &self.theorems {
            let i = env.index_of(t).ok_or_else(|| format!("the bundle does not declare {t}"))?;
            report.statements.push((t.clone(), unit::show_type(&env, i)));
        }
        // Exactly one piece of evidence per unit.
        let mut evidence: BTreeMap<&str, &Evidence> = BTreeMap::new();
        for (n, e) in &self.units {
            if evidence.insert(n, e).is_some() {
                return Err(format!("{n} has evidence twice"));
            }
        }
        let all: Vec<usize> = units(&env);
        let names: BTreeSet<String> = all.iter().map(|&i| env.name(env.decls[i].name)).collect();
        if let Some(n) = evidence.keys().find(|n| !names.contains(**n)) {
            return Err(format!("evidence for {n}, which is not a unit of the bundle"));
        }
        let mut jobs = Vec::new();
        for &i in &all {
            let n = env.name(env.decls[i].name);
            match evidence.get(n.as_str()) {
                None => return Err(format!("{n} has no evidence: prove it or list it as assumed")),
                Some(Evidence::Long(b)) => jobs.push((i, n, b.clone())),
                Some(Evidence::Short) => report.short.push(n),
                Some(Evidence::Assumed) => report.assumed.push(n),
            }
        }
        let env2 = env.clone();
        let names: Vec<String> = jobs.iter().map(|j| j.1.clone()).collect();
        let checked = par_map(jobs, move |(i, _, b)| {
            let p: Proof = unit::decode(&b)?;
            unit::check(&env2, i, &p)?;
            Ok(p.derivation.rows.len() as u64)
        });
        for (n, r) in names.into_iter().zip(checked) {
            match r {
                Ok(rows) => report.long.push((n, rows)),
                Err(e) => return Err(format!("{n}: {e}")),
            }
        }
        for d in &env.decls {
            match &d.kind {
                Kind::Axiom => report.axioms.push(env.name(d.name)),
                Kind::Quot(_) => report.builtins.push(env.name(d.name)),
                _ => {}
            }
        }
        report.axioms.sort();
        report.builtins.sort();
        Ok(report)
    }
}

/// How one unit fared when proving a bundle.
#[derive(Clone, Debug)]
pub struct Proved {
    pub name: String,
    /// Rows of its derivation, or why it was not proved.
    pub outcome: Result<u64, String>,
    pub cached: bool,
    pub millis: u64,
}

impl Bundle {
    /// Proves `theorems` long: takes their closure in `env`, proves every
    /// unit not in `assumed` and not in `short`, and writes the bundle. With
    /// `cache`, a unit's proof is kept in that directory under the BLAKE3
    /// hash of its declaration's text and reused when it still checks.
    /// Units that fail are returned, and the bundle is only returned when
    /// none did.
    pub fn prove(
        env: &Env,
        theorems: &[String],
        assumed: &BTreeSet<String>,
        short: &BTreeSet<String>,
        cache: Option<&std::path::Path>,
    ) -> Result<(Option<Bundle>, Vec<Proved>), String> {
        let idx = closure(env, theorems)?;
        let (sub, decls) = sub_env(env, &idx)?;
        let sub = Arc::new(sub);
        let mut units_ev: Vec<(String, Evidence)> = Vec::new();
        let mut jobs = Vec::new();
        for i in units(&sub) {
            let n = sub.name(sub.decls[i].name);
            if assumed.contains(&n) {
                units_ev.push((n, Evidence::Assumed));
            } else if short.contains(&n) {
                units_ev.push((n, Evidence::Short));
            } else {
                let file =
                    cache.map(|c| c.join(format!("{}.proof", blake3::hash(sub.decl_text(i).as_bytes()).to_hex())));
                jobs.push((i, n, file));
            }
        }
        let s2 = sub.clone();
        let results = par_map(jobs, move |(i, n, file)| {
            let t0 = std::time::Instant::now();
            if let Some(f) = &file {
                if let Ok(b) = std::fs::read(f) {
                    if let Ok(p) = unit::decode::<Proof>(&b) {
                        if unit::check(&s2, i, &p).is_ok() {
                            let rows = p.derivation.rows.len() as u64;
                            return Ok((n, b, rows, true, t0.elapsed().as_millis() as u64));
                        }
                    }
                }
            }
            let p = unit::prove(&s2, i, 0).map_err(|f| format!("{n}: {} ({})", f.message, f.stage))?;
            let b = unit::encode(&p);
            if let Some(f) = &file {
                let tmp = f.with_extension("part");
                if std::fs::write(&tmp, &b).is_ok() {
                    let _ = std::fs::rename(&tmp, f);
                }
            }
            let rows = p.derivation.rows.len() as u64;
            Ok((n, b, rows, false, t0.elapsed().as_millis() as u64))
        });
        let mut report = Vec::new();
        let mut ok = true;
        for r in results {
            match r {
                Ok((n, b, rows, cached, millis)) => {
                    report.push(Proved { name: n.clone(), outcome: Ok(rows), cached, millis });
                    units_ev.push((n, Evidence::Long(b)));
                }
                Err(e) => {
                    ok = false;
                    let name = e.split(':').next().unwrap_or("").to_string();
                    report.push(Proved { name, outcome: Err(e), cached: false, millis: 0 });
                }
            }
        }
        units_ev.sort_by(|a, b| a.0.cmp(&b.0));
        let b = ok.then(|| Bundle { theorems: theorems.to_vec(), decls, units: units_ev, short: Vec::new() });
        Ok((b, report))
    }
}
