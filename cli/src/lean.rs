//! `cas lean`: Lean declarations, their proofs and the check of those
//! proofs, all held in the store (spec/lean/cli.md).

use crate::{Args, Res, b3, e};
use cas_core::{Expect, Store};
use cas_lean::import::{Env, Kind};
use cas_lean::unit::{self, Proof, Record, Summary};
use cas_version::lean::LeanEnv;
use cas_version::uni::Value;
use cas_version::{View, formats, read_blob, write_blob};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};

const EXPORTER: &str = include_str!("../../lean/measure/Export.lean");
const PROVE_SPEC: &str = include_str!("../../spec/functions/lean-prove.md");

/// The words after `cas lean <sub>`, with this command's own options
/// taken out.
struct Words {
    names: Vec<String>,
    closure: bool,
    failed: bool,
    budget: usize,
    slowest: usize,
    root: Option<String>,
    env: String,
}

fn words(a: &Args) -> Res<Words> {
    let mut w = Words {
        names: vec![],
        closure: false,
        failed: false,
        budget: 2_000_000,
        slowest: 0,
        root: None,
        env: "default".into(),
    };
    let mut it = a.rest.iter().skip(2);
    while let Some(x) = it.next() {
        let mut value = |flag: &str| it.next().cloned().ok_or_else(|| format!("{flag} wants a value"));
        let number =
            |flag: &str, v: String| v.parse::<usize>().map_err(|_| format!("{flag} wants a number, not {v:?}"));
        match x.as_str() {
            "--closure" => w.closure = true,
            "--failed" => w.failed = true,
            "--budget" => w.budget = number("--budget", value("--budget")?)?,
            "--slowest" => w.slowest = number("--slowest", value("--slowest")?)?,
            "--root" => w.root = Some(value("--root")?),
            "--env" => w.env = value("--env")?,
            f if f.starts_with("--") => return Err(format!("no option {f}\n{}", crate::help::usage("lean"))),
            _ => w.names.push(x.clone()),
        }
    }
    Ok(w)
}

fn var(env: &str) -> String {
    format!("lean/{env}")
}

fn format_id(alias: &str) -> String {
    formats::by_alias(alias).unwrap().id().to_string()
}

/// The function `lean-prove` (spec/derived.md): its object, and its id.
fn prove_function() -> (Vec<u8>, String) {
    let object = Value::map([
        ("input", Value::str(format_id("lean-decl"))),
        ("name", Value::str("lean-prove")),
        ("output", Value::str(format_id("lean-proof"))),
        ("spec", Value::str(b3().key(PROVE_SPEC.as_bytes()))),
    ])
    .serialize();
    let id = b3().key(&object);
    (object, id)
}

fn result_var(function: &str, decl_key: &str) -> String {
    format!("derived/{function}/{decl_key}")
}

/// The environment the variable holds.
fn manifest(s: &dyn Store, w: &Words) -> Res<LeanEnv> {
    let name = var(&w.env);
    let v = cas_version::head(s, &name)
        .map_err(e)?
        .ok_or_else(|| format!("no Lean environment {}: `cas lean import` makes one", w.env))?;
    if !formats::by_alias("lean-env").unwrap().is(v.format()) {
        return Err(format!("{name} holds {v}, which is not a Lean environment"));
    }
    LeanEnv::parse(&read_blob(s, v.key()).map_err(e)?)
}

/// The names of Lean's primitives, which a proof about literals cites
/// though no term names them.
const PRIMS: &[&str] = &[
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

/// Reads the named declarations and everything they reach into one
/// environment: the constants their terms name, a block's constructors
/// and recursors, and Lean's primitives when a term has a literal. With
/// `all`, the whole environment.
fn load(s: &dyn Store, m: &LeanEnv, names: &[String], all: bool) -> Res<Env> {
    let mut env = Env::default();
    let mut todo: Vec<String> = if all { m.decls.keys().cloned().collect() } else { names.to_vec() };
    let mut seen: BTreeSet<String> = todo.iter().cloned().collect();
    let mut prims = false;
    while let Some(name) = todo.pop() {
        let key = m.decls.get(&name).ok_or_else(|| format!("no declaration {name} in the environment"))?;
        let bytes = read_blob(s, key).map_err(e)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| format!("{name}: {key} is not text"))?;
        let i = env.absorb(text).map_err(|x| format!("{name}: {x}"))?;
        if all {
            continue;
        }
        let mut more: Vec<String> = env.references(i);
        if let Kind::Induct { all: block, .. } = &env.decls[i].kind {
            let first = env.name(block[0]);
            more.extend(block.iter().map(|t| format!("{}.rec", env.name(*t))));
            more.extend((1..).map(|j| format!("{first}.rec_{j}")).take_while(|n| m.decls.contains_key(n)));
        }
        if !prims && env.has_literal(i) {
            prims = true;
            more.extend(PRIMS.iter().map(|p| p.to_string()));
        }
        for n in more {
            if m.decls.contains_key(&n) && seen.insert(n.clone()) {
                todo.push(n);
            }
        }
    }
    Ok(env)
}

/// The declarations the words name: by name, or by the key of their blob.
fn named(m: &LeanEnv, w: &Words) -> Res<Vec<String>> {
    let by_key: HashMap<&str, &str> = if w.names.iter().any(|n| n.starts_with("blake3:")) {
        m.decls.iter().map(|(n, k)| (k.as_str(), n.as_str())).collect()
    } else {
        HashMap::new()
    };
    w.names
        .iter()
        .map(|n| {
            if m.decls.contains_key(n) {
                Ok(n.clone())
            } else if let Some(name) = by_key.get(n.as_str()) {
                Ok(name.to_string())
            } else {
                Err(format!("no declaration {n} in the environment"))
            }
        })
        .collect()
}

/// The units to work on: those of the named declarations; with
/// `--closure` or `--all`, every unit loaded.
fn targets(env: &Env, names: &[String], everything: bool) -> Res<Vec<usize>> {
    let mut units = BTreeSet::new();
    if everything {
        units.extend((0..env.decls.len()).filter(|i| unit::is_unit(env, *i)));
    } else {
        for n in names {
            let i = env.index_of(n).ok_or_else(|| format!("no declaration {n}"))?;
            match unit::unit_of(env, i) {
                Some(u) => units.insert(u),
                None => return Err(format!("{n} has no proof: it is a quotient builtin or unsafe")),
            };
        }
    }
    Ok(units.into_iter().collect())
}

fn key_of<'a>(m: &'a LeanEnv, env: &Env, i: usize) -> &'a str {
    &m.decls[&env.name(env.decls[i].name)]
}

/// The stored result of a unit: its view's key and the start of its record.
fn stored(s: &dyn Store, function: &str, decl_key: &str) -> Res<Option<(Vec<u8>, Vec<u8>)>> {
    let name = result_var(function, decl_key);
    let Some(raw) = s.read(&name).map_err(e)? else { return Ok(None) };
    let view =
        std::str::from_utf8(&raw).ok().and_then(|t| View::parse(t).ok()).ok_or(format!("{name} is not a view"))?;
    Ok(Some((raw, read_blob(s, view.key()).map_err(e)?)))
}

type Head = (Summary, Vec<(String, String)>);

/// Whether a record was made against the declarations the environment
/// has now.
fn current(m: &LeanEnv, head: &Head) -> bool {
    head.1.iter().all(|(n, k)| m.decls.get(n) == Some(k))
}

fn workers() -> usize {
    cas_core::threads::get().max(1)
}

pub fn run(a: &Args, s: &dyn Store) -> Res<()> {
    let sub = a
        .rest
        .get(1)
        .map(String::as_str)
        .ok_or_else(|| format!("missing a command\n{}", crate::help::usage("lean")))?;
    let w = words(a)?;
    match sub {
        "import" => import(s, &w),
        "prove" => prove(a, s, &w),
        "check" => check(a, s, &w),
        "show" => show(s, &w),
        "axioms" => axioms(s, &w),
        "report" => report(s, &w),
        x => Err(format!("no command lean {x}\n{}", crate::help::usage("lean"))),
    }
}

/// Runs the exporter in a Lake project with the project's own Lean.
fn export(root: &str, modules: &[String]) -> Res<(String, String)> {
    if modules.is_empty() {
        return Err(format!("missing <module>\n{}", crate::help::usage("lean")));
    }
    let dir = std::env::temp_dir().join(format!("cas-lean-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(e)?;
    let (script, out) = (dir.join("Export.lean"), dir.join("export.txt"));
    std::fs::write(&script, EXPORTER).map_err(e)?;
    let lake = |args: &[&std::ffi::OsStr]| {
        std::process::Command::new("lake")
            .current_dir(root)
            .arg("env")
            .arg("lean")
            .args(args)
            .output()
            .map_err(|x| format!("lake (in {root}): {x}"))
    };
    let version = lake(&["--version".as_ref()])?;
    let version = String::from_utf8_lossy(&version.stdout).trim().to_string();
    let mut args: Vec<&std::ffi::OsStr> = vec!["--run".as_ref(), script.as_ref(), out.as_ref()];
    args.extend(modules.iter().map(std::ffi::OsStr::new));
    let run = lake(&args)?;
    if !run.status.success() {
        return Err(format!("the exporter failed: {}", String::from_utf8_lossy(&run.stderr).trim()));
    }
    let text = std::fs::read_to_string(&out).map_err(e)?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok((text, version))
}

fn import(s: &dyn Store, w: &Words) -> Res<()> {
    let (text, lean, modules) = match &w.root {
        Some(root) => {
            let (text, version) = export(root, &w.names)?;
            (text, version, w.names.join(" "))
        }
        None => {
            let [file] = w.names.as_slice() else {
                return Err(format!(
                    "missing <export-file>, or --root <lake-dir> <module>...\n{}",
                    crate::help::usage("lean")
                ));
            };
            (std::fs::read_to_string(file).map_err(|x| format!("{file}: {x}"))?, String::new(), String::new())
        }
    };
    let env = Env::parse(&text)?;
    drop(text);
    let mut m = LeanEnv { lean, modules, decls: BTreeMap::new() };
    let mut new = 0;
    for i in 0..env.decls.len() {
        let text = env.decl_text(i);
        let key = b3().key(text.as_bytes());
        if s.read(&key).map_err(e)?.is_none() {
            write_blob(s, b3(), text.as_bytes()).map_err(e)?;
            new += 1;
        }
        if m.decls.insert(env.name(env.decls[i].name), key).is_some() {
            return Err(format!("{} is declared twice", env.name(env.decls[i].name)));
        }
    }
    let view = cas_version::put(s, b3(), "lean-env", &m.print()).map_err(e)?;
    let name = var(&w.env);
    let expect = match s.read(&name).map_err(e)? {
        Some(old) => Expect::Value(old),
        None => Expect::Absent,
    };
    s.write(&name, view.to_string().as_bytes(), &expect).map_err(e)?;
    out!("{} declarations, {new} new", m.decls.len());
    out!("environment {}: {}", w.env, view.key());
    Ok(())
}

/// The record of a unit's proof, or of its failure.
fn record(env: &Arc<Env>, m: &LeanEnv, i: usize, budget: usize) -> Record {
    let t0 = std::time::Instant::now();
    let name = env.name(env.decls[i].name);
    let run = unit::prove(env, i, budget);
    let millis = t0.elapsed().as_millis() as u64;
    let mut against: BTreeMap<String, String> = BTreeMap::new();
    let mut add = |keys: Vec<[u8; 32]>| {
        for k in keys {
            if let Some(d) = env.by_key(&k) {
                let n = env.name(d.name);
                if let Some(key) = m.decls.get(&n) {
                    against.insert(n, key.clone());
                }
            }
        }
    };
    add(unit::own_keys(env, i));
    let (status, rows, terms, outcome) = match run {
        Ok(p) => {
            add(unit::cited(env, i, &p));
            ("ok".to_string(), p.derivation.rows.len(), p.derivation.terms.len(), Ok(p))
        }
        Err(f) => (f.stage, 0, 0, Err(f.message)),
    };
    Record {
        summary: Summary { name, status, rows: rows as u64, terms: terms as u64, millis },
        against: against.into_iter().collect(),
        outcome,
    }
}

fn prove(a: &Args, s: &dyn Store, w: &Words) -> Res<()> {
    let m = manifest(s, w)?;
    let names = named(&m, w)?;
    if names.is_empty() && !a.all {
        return Err(format!("missing <name>, or --all\n{}", crate::help::usage("lean")));
    }
    let env = Arc::new(load(s, &m, &names, a.all)?);
    let units = targets(&env, &names, a.all || w.closure)?;
    let (object, function) = prove_function();
    write_blob(s, b3(), PROVE_SPEC.as_bytes()).map_err(e)?;
    write_blob(s, b3(), &object).map_err(e)?;
    // What is stored and current is kept.
    let mut todo = vec![];
    let mut old: HashMap<usize, Vec<u8>> = HashMap::new();
    let mut kept = 0;
    for &i in &units {
        match stored(s, &function, key_of(&m, &env, i))? {
            Some((raw, bytes)) => {
                let head: Head = unit::decode_prefix(&bytes)?;
                if head.0.status == "ok" && current(&m, &head) {
                    kept += 1;
                    continue;
                }
                old.insert(i, raw);
                todo.push(i);
            }
            None => todo.push(i),
        }
    }
    let m = Arc::new(m);
    let todo = Arc::new(todo);
    let next = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = mpsc::sync_channel::<(usize, Record)>(1);
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let hs: Vec<_> = (0..workers().min(todo.len().max(1)))
        .map(|_| {
            let (env, m, todo, next, tx, budget) =
                (env.clone(), m.clone(), todo.clone(), next.clone(), tx.clone(), w.budget);
            std::thread::Builder::new()
                .stack_size(1 << 30)
                .spawn(move || {
                    while let Some(&i) = todo.get(next.fetch_add(1, Ordering::SeqCst)) {
                        if tx.send((i, record(&env, &m, i, budget))).is_err() {
                            break;
                        }
                    }
                })
                .unwrap()
        })
        .collect();
    drop(tx);
    let mut failed = 0;
    for (i, r) in rx {
        let decl = key_of(&m, &env, i);
        let bytes = unit::encode(&r);
        let view = cas_version::put(s, b3(), "lean-proof", &bytes).map_err(e)?;
        let expect = old.remove(&i).map_or(Expect::Absent, Expect::Value);
        s.write(&result_var(&function, decl), view.to_string().as_bytes(), &expect).map_err(e)?;
        if r.summary.status != "ok" {
            failed += 1;
        }
        let note = r.outcome.as_ref().err().map(|x| one_line(x)).unwrap_or_default();
        out!(
            "{}\t{decl}\t{}\t{} rows\t{} ms\t{note}",
            r.summary.name,
            r.summary.status,
            r.summary.rows,
            r.summary.millis
        );
    }
    for h in hs {
        let _ = h.join();
    }
    std::panic::set_hook(hook);
    out!("{} proved, {kept} kept, {failed} failed", todo.len() - failed);
    if failed > 0 { Err(format!("{failed} not proved")) } else { Ok(()) }
}

fn one_line(s: &str) -> String {
    s.chars().take(300).map(|c| if c == '\t' || c == '\n' { ' ' } else { c }).collect()
}

fn check(a: &Args, s: &dyn Store, w: &Words) -> Res<()> {
    let m = manifest(s, w)?;
    let names = named(&m, w)?;
    if names.is_empty() && !a.all {
        return Err(format!("missing <name>, or --all\n{}", crate::help::usage("lean")));
    }
    let env = Arc::new(load(s, &m, &names, a.all)?);
    let units = targets(&env, &names, a.all || w.closure)?;
    let (_, function) = prove_function();
    let (jobs_tx, jobs_rx) = mpsc::sync_channel::<(usize, Vec<u8>)>(workers());
    let jobs_rx = Arc::new(Mutex::new(jobs_rx));
    let (tx, rx) = mpsc::channel::<(usize, Result<u64, String>)>();
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let hs: Vec<_> = (0..workers().min(units.len().max(1)))
        .map(|_| {
            let (env, jobs_rx, tx) = (env.clone(), jobs_rx.clone(), tx.clone());
            std::thread::Builder::new()
                .stack_size(1 << 30)
                .spawn(move || {
                    loop {
                        let job = jobs_rx.lock().unwrap().recv();
                        let Ok((i, bytes)) = job else { break };
                        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<u64, String> {
                            let r: Record = unit::decode(&bytes)?;
                            let p: Proof = r.outcome.map_err(|x| format!("not proved ({}): {x}", r.summary.status))?;
                            unit::check(&env, i, &p)?;
                            Ok(p.derivation.rows.len() as u64)
                        }));
                        let r = run.unwrap_or_else(|_| Err("the check panicked".into()));
                        if tx.send((i, r)).is_err() {
                            break;
                        }
                    }
                })
                .unwrap()
        })
        .collect();
    drop(tx);
    let (mut accepted, mut refused) = (0, 0);
    let mut say = |i: usize, r: Result<u64, String>| {
        let name = env.name(env.decls[i].name);
        match r {
            Ok(rows) => {
                accepted += 1;
                if !a.quiet {
                    out!("{name}\t{}\taccepted\t{rows} rows", key_of(&m, &env, i));
                }
            }
            Err(why) => {
                refused += 1;
                out!("{name}\t{}\trefused\t{}", key_of(&m, &env, i), one_line(&why));
            }
        }
    };
    for &i in &units {
        match stored(s, &function, key_of(&m, &env, i))? {
            None => say(i, Err("no proof is stored: `cas lean prove` makes one".into())),
            Some((_, bytes)) => {
                let head: Head = unit::decode_prefix(&bytes)?;
                if !current(&m, &head) {
                    say(i, Err("the stored proof was made against other declarations: prove again".into()));
                } else {
                    jobs_tx.send((i, bytes)).map_err(e)?;
                }
            }
        }
        while let Ok((i, r)) = rx.try_recv() {
            say(i, r);
        }
    }
    drop(jobs_tx);
    for (i, r) in rx {
        say(i, r);
    }
    for h in hs {
        let _ = h.join();
    }
    std::panic::set_hook(hook);
    // What the units rest on without a proof of their own.
    let builtin = (0..env.decls.len()).filter(|i| matches!(env.decls[*i].kind, Kind::Quot(_))).count();
    let scope = if a.all || w.closure {
        format!("everything they rest on is checked ({builtin} quotient builtins are Lean's own)")
    } else {
        "the declarations they cite are not checked here: --closure checks them too".to_string()
    };
    out!("{accepted} accepted, {refused} refused; {scope}");
    if refused > 0 { Err(format!("{refused} refused")) } else { Ok(()) }
}

fn show(s: &dyn Store, w: &Words) -> Res<()> {
    let m = manifest(s, w)?;
    let names = named(&m, w)?;
    let [name] = names.as_slice() else { return Err(format!("missing <name>\n{}", crate::help::usage("lean"))) };
    let env = Arc::new(load(s, &m, &names, false)?);
    let i = env.index_of(name).unwrap();
    let d = &env.decls[i];
    let kind = match &d.kind {
        Kind::Axiom => "axiom",
        Kind::Def { .. } => "definition",
        Kind::Thm { .. } => "theorem",
        Kind::Opaque { .. } => "opaque",
        Kind::Quot(_) => "quotient builtin",
        Kind::Induct { .. } => "inductive type",
        Kind::Ctor { .. } => "constructor",
        Kind::Rec { .. } => "recursor",
    };
    out!("{name}");
    out!("  key      {}", m.decls[name]);
    out!("  kind     {kind}{}", if d.unsafe_ { " (unsafe)" } else { "" });
    out!("  levels   {}", d.lparams.len());
    out!("  type     {}", unit::show_type(&env, i));
    let (_, function) = prove_function();
    match unit::unit_of(&env, i) {
        None => out!("  proof    none: Lean's kernel takes it as given"),
        Some(u) => {
            let uname = env.name(env.decls[u].name);
            if u != i {
                out!("  unit     {uname} (its inductive block)");
            }
            match stored(s, &function, &m.decls[&uname])? {
                None => out!("  proof    not stored"),
                Some((raw, bytes)) => {
                    let head: Head = unit::decode_prefix(&bytes)?;
                    let view = String::from_utf8_lossy(&raw).into_owned();
                    let key = view.split(' ').nth(1).unwrap_or("");
                    let state = if current(&m, &head) { "" } else { ", made against other declarations" };
                    out!("  proof    {key} ({}, {} rows, {} ms{state})", head.0.status, head.0.rows, head.0.millis);
                }
            }
        }
    }
    let cites = env.references(i);
    out!("  cites    {}", cites.len());
    for n in cites {
        out!("    {n}\t{}", m.decls.get(&n).map_or("not in the environment", String::as_str));
    }
    Ok(())
}

fn axioms(s: &dyn Store, w: &Words) -> Res<()> {
    let m = manifest(s, w)?;
    let names = named(&m, w)?;
    if names.is_empty() {
        return Err(format!("missing <name>\n{}", crate::help::usage("lean")));
    }
    let env = load(s, &m, &names, false)?;
    let mut found: Vec<String> = (0..env.decls.len())
        .filter(|i| matches!(env.decls[*i].kind, Kind::Axiom))
        .map(|i| env.name(env.decls[i].name))
        .collect();
    found.sort();
    for n in &found {
        out!("{n}\t{}", m.decls[n]);
    }
    if found.is_empty() {
        out!("no axioms");
    }
    Ok(())
}

fn report(s: &dyn Store, w: &Words) -> Res<()> {
    let m = manifest(s, w)?;
    let (_, function) = prove_function();
    let mut heads: Vec<(Head, bool)> = vec![];
    for key in m.decls.values() {
        if let Some((_, bytes)) = stored(s, &function, key)? {
            let head: Head = unit::decode_prefix(&bytes)?;
            let now = current(&m, &head);
            heads.push((head, now));
        }
    }
    let mut by: BTreeMap<&str, usize> = BTreeMap::new();
    for (h, _) in &heads {
        *by.entry(h.0.status.as_str()).or_default() += 1;
    }
    let stale = heads.iter().filter(|h| !h.1).count();
    out!("{} declarations, {} results stored, {stale} made against other declarations", m.decls.len(), heads.len());
    for (status, n) in &by {
        out!("  {status}\t{n}");
    }
    let mut rows: Vec<u64> = heads.iter().filter(|h| h.0.0.status == "ok").map(|h| h.0.0.rows).collect();
    rows.sort();
    if !rows.is_empty() {
        let at = |q: f64| rows[((rows.len() - 1) as f64 * q) as usize];
        out!("rows: median {}, p99 {}, largest {}, total {}", at(0.5), at(0.99), at(1.0), rows.iter().sum::<u64>());
        out!("prover time: {} s", heads.iter().map(|h| h.0.0.millis).sum::<u64>() / 1000);
    }
    if w.slowest > 0 {
        heads.sort_by_key(|h| std::cmp::Reverse(h.0.0.millis));
        out!("slowest:");
        for (h, _) in heads.iter().take(w.slowest) {
            out!("  {}\t{}\t{} rows\t{} ms", h.0.name, h.0.status, h.0.rows, h.0.millis);
        }
    }
    if w.failed {
        out!("failed:");
        for (h, _) in heads.iter().filter(|h| h.0.0.status != "ok") {
            out!("  {}\t{}", h.0.name, h.0.status);
        }
    }
    Ok(())
}
