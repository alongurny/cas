//! `cas-lean`: prove a Lean theorem with one succinct proof, and verify it.
//!
//! ```text
//! cas-lean verify BUNDLE [KEY ...]
//! cas-lean prove [--long] [--assume A,B,...] [--export FILE] [--project DIR]
//!                [--work DIR] [--out DIR] MODULE THEOREM
//! ```
//!
//! A bundle (cas `spec/lean/bundle.md`) holds the theorem's declarations as
//! Lean text, which is what it states, and one piece of evidence for every
//! unit they rest on: a long proof (its derivation, checked here), a short
//! one (covered by the succinct proof it carries), or assumed (listed).
//! `prove` makes the succinct proof of the whole cone and long proofs for
//! what it does not cover; with `--long`, long proofs only.
//!
//! `verify` reads a bundle and its verification keys (by default the
//! `vk-<fingerprint>.vk` files beside it) and nothing else, and prints what
//! the bundle proves. `prove` exports MODULE from the Lake project DIR
//! (`lake env lean --run`), takes THEOREM's cone, checks it as tables of
//! local rules, proves the tables in segments and writes the bundle and
//! its keys to the out directory. Each step's output stays in the work
//! directory, so a run that stops goes on from its last finished step.
//!
//! The steps are the pipeline's own programs, linked into this one binary
//! and run as child processes of it (one process per segment keeps each
//! proof's memory its own).

#[path = "../../lean/examples/lean_closure.rs"]
mod lean_closure;
#[path = "../../../flock/crates/flock-prover/examples/lean_cone.rs"]
mod lean_cone;
#[path = "../../lean/examples/lean_dump.rs"]
mod lean_dump;
#[path = "../../../flock/crates/flock-prover/examples/lean_tower.rs"]
mod lean_tower;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::{fs, time::Instant};

use cas_lean::bundle::{Bundle, Evidence};
use cas_lean::import::Env;
use flock_field::F128;
use flock_prover::tower::lean_verify::{LeanBundle, LeanVerified, LeanVk, verify_lean_bundle};
use std::collections::{BTreeSet, HashSet};

/// The header of a bundle (format 2): zstd of its bincode follows.
const HEADER: &[u8; 16] = b"cas-lean-bundle2";

fn write_bundle(path: &Path, b: &Bundle) -> Result<usize, String> {
    let raw = cas_lean::unit::encode(b);
    let mut bytes = HEADER.to_vec();
    bytes.extend(zstd::encode_all(&raw[..], 19).map_err(|e| e.to_string())?);
    fs::write(path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(bytes.len())
}

fn read_bundle(bytes: &[u8]) -> Result<Bundle, String> {
    let body = bytes.strip_prefix(HEADER).ok_or("not a cas-lean bundle (format 2)")?;
    let raw = zstd::decode_all(body).map_err(|e| format!("the bundle does not decompress: {e}"))?;
    cas_lean::unit::decode(&raw)
}

/// How the succinct proof's statement names a declaration: the first 16
/// bytes of its key as two words.
fn key_words(name: &str) -> [F128; 2] {
    let k = cas_lean::derivation::lean_key(name);
    let w = |o: usize| u64::from_le_bytes(k[o..o + 8].try_into().unwrap());
    [F128::new(w(0), 0), F128::new(w(8), 0)]
}

const EXPORT_LEAN: &str = include_str!("../../lean/measure/Export.lean");

/// The tools, by the name a child process is started under.
const TOOLS: [(&str, fn()); 4] = [
    ("cas-lean-closure", lean_closure::main),
    ("cas-lean-dump", lean_dump::main),
    ("cas-lean-cone", lean_cone::main),
    ("cas-lean-tower", lean_tower::main),
];

const USAGE: &str = "usage:
  cas-lean verify BUNDLE [KEY ...]
  cas-lean prove [--long] [--assume A,B,...] [--export FILE] [--project DIR]
                 [--work DIR] [--out DIR] MODULE THEOREM";

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn main() -> ExitCode {
    // A child started under a tool's name is that tool.
    let argv0 = std::env::args().next().unwrap_or_default();
    let me = Path::new(&argv0).file_name().and_then(|s| s.to_str()).unwrap_or("");
    if let Some((_, tool)) = TOOLS.iter().find(|(n, _)| *n == me) {
        tool();
        return ExitCode::SUCCESS;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("verify") if args.len() >= 2 => verify(&args[1], &args[2..]),
        Some("prove") => prove(&args[1..]),
        Some("census") if args.len() == 2 => census(&args[1]),
        Some("facts-selftest") if args.len() == 2 => facts_selftest(&args[1]),
        Some("--version") => {
            println!("cas-lean {}", env!("CARGO_PKG_VERSION"));
            Ok(true)
        }
        _ => Err(USAGE.into()),
    };
    match r {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}

/// The bundle's declarations as the shared side of a cone: its facts must
/// check against the declarations (a test of the comparison).
fn facts_selftest(bundle: &str) -> Result<bool, String> {
    let b = read_bundle(&fs::read(bundle).map_err(|e| e.to_string())?)?;
    let mut env = Env::default();
    for t in &b.decls {
        env.absorb(t)?;
    }
    let t = cas_lean::bundle::decl_rows(&std::sync::Arc::new(env))?;
    let text = lean_dump::dump(&t, None, &[0u8; 32], &|_| 0);
    let mut cone = flock_prover::lean_rows::Cone::new();
    let mut batch = flock_prover::lean_rows::Rows::default();
    cone.add(flock_prover::lean_rows::Rows::parse(&text)?, &mut batch)?;
    let g = cone.finish();
    let (c, _) = flock_prover::lean_rows::LeanCircuit::build_side(
        &g,
        &flock_prover::lean_rows::Rows::default(),
        flock_prover::lean_rows::Side::Shared,
        flock_prover::lean_rows::SHARED_BASE,
        &Default::default(),
    )?;
    let ok = bound_to_text(&b, &c.stmt_facts)?;
    println!("{} facts; {ok:?}", c.stmt_facts.len());
    Ok(true)
}

/// The size of a statement binding a cone's declarations to their text.
fn census(cone: &str) -> Result<bool, String> {
    let bytes = fs::read(cone).map_err(|e| format!("{cone}: {e}"))?;
    let g: flock_prover::lean_rows::Rows = bincode::deserialize(&bytes).map_err(|e| e.to_string())?;
    let (all, stmt, decls) = g.statement_census();
    println!("{all} term rows, {stmt} reached from types, non-theorem values and rules; {decls} declarations");
    Ok(true)
}

fn verify(bundle: &str, keys: &[String]) -> Result<bool, String> {
    let t = Instant::now();
    let bytes = fs::read(bundle).map_err(|e| format!("{bundle}: {e}"))?;
    let dir = Path::new(bundle).parent().unwrap_or(Path::new("."));
    if !bytes.starts_with(HEADER) {
        // Format 1: a succinct proof alone.
        return Ok(match succinct(&bytes, dir, keys)? {
            Ok((b, v)) => {
                println!("VERIFIED in {:.1} s: a succinct proof alone", t.elapsed().as_secs_f64());
                say_succinct(&b, &v);
                println!("bundle    blake3:{}", hex(blake3::hash(&bytes).as_bytes()));
                println!("note: it names the declarations it checks by name only; a format 2 bundle says what they state");
                true
            }
            Err(e) => {
                println!("REFUSED: {e}");
                false
            }
        });
    }
    let b = match read_bundle(&bytes) {
        Ok(b) => b,
        Err(e) => {
            println!("REFUSED: {e}");
            return Ok(false);
        }
    };
    let r = match b.verify() {
        Ok(r) => r,
        Err(e) => {
            println!("REFUSED: {e}");
            return Ok(false);
        }
    };
    let mut short = None;
    if !r.short.is_empty() || !b.short.is_empty() {
        let (fb, v) = match succinct(&b.short, dir, keys)? {
            Ok(x) => x,
            Err(e) => {
                println!("REFUSED: the succinct proof: {e}");
                return Ok(false);
            }
        };
        let have: HashSet<[F128; 2]> = fb.statement.keys.iter().copied().collect();
        if let Some(n) = r.short.iter().find(|n| !have.contains(&key_words(n))) {
            println!("REFUSED: the succinct proof does not check {n}");
            return Ok(false);
        }
        // What the succinct proof's declarations are: the bundle's.
        let bound = if fb.statement.facts.is_empty() {
            None
        } else {
            match bound_to_text(&b, &fb.statement.facts) {
                Ok(c) => Some(c),
                Err(e) => {
                    println!("REFUSED: the succinct proof's declarations are not the bundle's: {e}");
                    return Ok(false);
                }
            }
        };
        short = Some((fb, v, bound));
    }
    let rows: u64 = r.long.iter().map(|x| x.1).sum();
    println!("VERIFIED in {:.1} s", t.elapsed().as_secs_f64());
    for (n, ty) in &r.statements {
        println!("theorem   {n} : {ty}");
    }
    println!("statement blake3:{} ({} declarations)", hex(&r.statement_digest), r.decls);
    println!("bundle    blake3:{}", hex(blake3::hash(&bytes).as_bytes()));
    println!("long      {} units, {rows} derivation rows, checked here", r.long.len());
    match &short {
        Some((fb, v, bound)) => {
            println!("short     {} units, by the succinct proof:", r.short.len());
            say_succinct(fb, v);
            match bound {
                Some(c) => println!(
                    "          its {} declarations are the bundle's ({} terms and {} recursor rules compared)",
                    c.decls, c.terms, c.rules
                ),
                None => println!("          (an older proof: it names them by name; that it checked these declarations' text is not bound)"),
            }
        }
        None => println!("short     none"),
    }
    let list = |v: &[String]| if v.is_empty() { "none".to_string() } else { v.join(", ") };
    println!("assumed   {}", list(&r.assumed));
    println!("axioms    {}", list(&r.axioms));
    let extra: Vec<String> = r
        .axioms
        .iter()
        .filter(|a| !cas_lean::bundle::STANDARD_AXIOMS.contains(&a.as_str()))
        .cloned()
        .collect();
    if !extra.is_empty() {
        println!("warning   axioms beyond Lean's three: {}", extra.join(", "));
    }
    if !r.builtins.is_empty() {
        println!("builtins  {} (Lean's quotient, no proof)", r.builtins.join(", "));
    }
    Ok(true)
}

/// Compares the succinct proof's facts about its declarations with the
/// bundle's declarations.
fn bound_to_text(
    b: &Bundle,
    facts: &[flock_prover::lean_rows::Tuple],
) -> Result<flock_prover::lean_rows::FactsChecked, String> {
    let mut env = Env::default();
    for t in &b.decls {
        env.absorb(t)?;
    }
    let t = cas_lean::bundle::decl_rows(&std::sync::Arc::new(env))?;
    let text = lean_dump::dump(&t, None, &[0u8; 32], &|_| 0);
    let rows = flock_prover::lean_rows::Rows::parse(&text)?;
    flock_prover::lean_rows::check_facts(facts, &rows)
}

fn say_succinct(b: &LeanBundle, v: &LeanVerified) {
    println!("          {} segment proofs over {} leaves", v.segments, v.leaves);
    println!("          statement blake3:{} ({} declarations checked)", hex(&v.statement), b.statement.keys.len());
    for (f, _) in &v.vks {
        println!("          key blake3:{}", hex(f));
    }
}

/// Verifies a succinct proof (a flock Lean bundle) against its keys: those
/// given, or the `vk-<fingerprint>.vk` files in `dir`. The outer error is
/// a key that cannot be read; the inner a refusal.
fn succinct(bytes: &[u8], dir: &Path, keys: &[String]) -> Result<Result<(LeanBundle, LeanVerified), String>, String> {
    let b = match LeanBundle::from_bytes(bytes) {
        Ok(b) => b,
        Err(e) => return Ok(Err(e)),
    };
    let files: Vec<PathBuf> = if keys.is_empty() {
        b.vks.iter().map(|f| dir.join(format!("vk-{}.vk", hex(f)))).collect()
    } else {
        keys.iter().map(PathBuf::from).collect()
    };
    let mut vks = Vec::new();
    for p in &files {
        let k = fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
        match LeanVk::from_bytes(&k) {
            Ok(vk) => vks.push(vk),
            Err(e) => return Ok(Err(format!("{}: {e}", p.display()))),
        }
    }
    Ok(verify_lean_bundle(&b, &vks).map(|v| (b, v)))
}

/// Proves every unit of the closure not in `short` or `assumed` long, and
/// writes the bundle with the succinct proof `short_bytes`.
fn long_bundle(
    r: &Run,
    export: &Path,
    theorem: &str,
    assumed: &BTreeSet<String>,
    short: &BTreeSet<String>,
    short_bytes: Vec<u8>,
    out: &Path,
) -> Result<PathBuf, String> {
    r.say(&format!("reading {}", export.display()));
    let text = fs::read_to_string(export).map_err(|e| format!("{}: {e}", export.display()))?;
    let env = Env::parse(&text)?;
    drop(text);
    let cache = r.work.join("long");
    fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    r.say(&format!("long proofs of what {theorem} rests on"));
    let (b, done) = Bundle::prove(&env, &[theorem.to_string()], assumed, short, Some(&cache))?;
    let rows: u64 = done.iter().filter_map(|p| p.outcome.as_ref().ok()).sum();
    let cached = done.iter().filter(|p| p.cached).count();
    r.say(&format!("{} units proved long ({cached} from the cache), {rows} rows", done.len()));
    let Some(mut b) = b else {
        for p in done.iter().filter(|p| p.outcome.is_err()) {
            println!("not proved: {}", p.outcome.as_ref().unwrap_err());
        }
        return Err("some units were not proved: fix them, or name them with --assume".into());
    };
    b.short = short_bytes;
    fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let path = out.join(format!("{theorem}.bundle"));
    let n = write_bundle(&path, &b)?;
    let (s, a) = (
        b.units.iter().filter(|u| u.1 == Evidence::Short).count(),
        b.units.iter().filter(|u| u.1 == Evidence::Assumed).count(),
    );
    r.say(&format!("wrote {} ({} KiB): {} declarations, {s} units short, {a} assumed", path.display(), n / 1024, b.decls.len()));
    Ok(path)
}

/// The settings every proving step runs with: the cone's row bits, the
/// rare rules' leaves, the publics a wide leaf needs, and memory limits
/// that keep a leaf proof under 16 GB.
const ENV: [(&str, &str); 8] = [
    ("LEAN_ROWS_NU", "13"),
    ("LEAN_RARE_SLOTS", "3"),
    ("LEAN_BODY_EXTRA", "2048"),
    ("LEAN_AROUND", "0"),
    ("LEAN_POOL_MB", "16"),
    ("MALLOC_MMAP_THRESHOLD_", "1048576"),
    ("MALLOC_TRIM_THRESHOLD_", "67108864"),
    ("MALLOC_ARENA_MAX", "2"),
];
/// Common segments (the rare rules' leaves make one more).
const SEGMENTS: &str = "8";
const SLOTS: &str = "34";

struct Run {
    work: PathBuf,
    t0: Instant,
}

impl Run {
    fn say(&self, what: &str) {
        println!("[{:>6.0} s] {what}", self.t0.elapsed().as_secs_f64());
    }

    /// Run `tool` in `dir` with `args` and extra `env`, its output to `log`.
    fn tool(&self, tool: &str, dir: &Path, args: &[&str], env: &[(&str, &str)], log: &str) -> Result<String, String> {
        use std::os::unix::process::CommandExt;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let out = fs::File::create(self.work.join(log)).map_err(|e| e.to_string())?;
        let status = Command::new(exe)
            .arg0(tool)
            .args(args)
            .current_dir(dir)
            .envs(ENV)
            .envs(env.iter().copied())
            .stdout(out.try_clone().map_err(|e| e.to_string())?)
            .stderr(out)
            .status()
            .map_err(|e| format!("{tool}: {e}"))?;
        let text = fs::read_to_string(self.work.join(log)).unwrap_or_default();
        if !status.success() {
            let tail: Vec<&str> = text.lines().rev().take(12).collect();
            return Err(format!(
                "{tool} {} failed ({status}); the end of {}:\n{}",
                args.join(" "),
                self.work.join(log).display(),
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            ));
        }
        Ok(text)
    }
}

fn prove(args: &[String]) -> Result<bool, String> {
    let mut project = PathBuf::from(".");
    let mut work: Option<PathBuf> = None;
    let mut out = PathBuf::from(".");
    let mut export: Option<PathBuf> = None;
    let mut long = false;
    let mut assumed: BTreeSet<String> = BTreeSet::new();
    let mut pos = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--long" => {
                long = true;
                i += 1;
            }
            "--assume" if i + 1 < args.len() => {
                assumed.extend(args[i + 1].split(',').filter(|x| !x.is_empty()).map(String::from));
                i += 2;
            }
            "--export" if i + 1 < args.len() => {
                export = Some(fs::canonicalize(&args[i + 1]).map_err(|e| format!("{}: {e}", args[i + 1]))?);
                i += 2;
            }
            "--project" | "--work" | "--out" if i + 1 < args.len() => {
                let v = PathBuf::from(&args[i + 1]);
                match args[i].as_str() {
                    "--project" => project = v,
                    "--work" => work = Some(v),
                    _ => out = v,
                }
                i += 2;
            }
            _ => {
                pos.push(args[i].clone());
                i += 1;
            }
        }
    }
    let [module, theorem] = pos.as_slice() else {
        return Err(USAGE.into());
    };
    let work = work.unwrap_or_else(|| PathBuf::from(format!("cas-lean-{theorem}")));
    fs::create_dir_all(work.join("cone")).map_err(|e| e.to_string())?;
    let work = fs::canonicalize(&work).map_err(|e| e.to_string())?;
    let r = Run {
        work: work.clone(),
        t0: Instant::now(),
    };
    let w = |p: &str| work.join(p).display().to_string();
    let have = |p: &str| work.join(p).exists();

    // 1. The module's declarations, as text.
    if let Some(x) = &export {
        let _ = fs::remove_file(work.join("export.txt"));
        std::os::unix::fs::symlink(x, work.join("export.txt")).map_err(|e| e.to_string())?;
    }
    if long {
        let p = long_bundle(&r, &work.join("export.txt"), theorem, &assumed, &BTreeSet::new(), Vec::new(), &out)?;
        return verify(&p.display().to_string(), &[]);
    }
    if !have("export.txt") {
        r.say(&format!("exporting {module} from {}", project.display()));
        fs::write(work.join("Export.lean"), EXPORT_LEAN).map_err(|e| e.to_string())?;
        let status = Command::new("lake")
            .args(["env", "lean", "--run", &w("Export.lean"), &w("export.txt.part"), module])
            .current_dir(&project)
            .status()
            .map_err(|e| format!("lake (is Lean installed, and {} a Lake project?): {e}", project.display()))?;
        if !status.success() {
            return Err(format!("the export failed ({status})"));
        }
        fs::rename(work.join("export.txt.part"), work.join("export.txt")).map_err(|e| e.to_string())?;
    }
    // 2. The theorem's cone: every declaration it depends on.
    if !have("cone.txt") {
        r.say(&format!("the cone of {theorem}"));
        r.tool("cas-lean-closure", &work, &[&w("export.txt"), "-", "--out", &w("cone.txt.part"), theorem], &[], "closure.log")?;
        fs::rename(work.join("cone.txt.part"), work.join("cone.txt")).map_err(|e| e.to_string())?;
    }
    // 3. Each declaration's derivation as rows, the inductive types' info,
    // and the numbers arithmetic reads.
    if !have("info.rows") {
        r.say("derivations as rows");
        fs::create_dir_all(work.join("rows")).map_err(|e| e.to_string())?;
        r.tool(
            "cas-lean-dump",
            &work,
            &[&w("export.txt"), &w("rows"), "--order", &w("cone.txt"), "--names", &w("cone.txt"), "--info", &w("info.rows.part")],
            &[],
            "dump.log",
        )?;
        fs::rename(work.join("info.rows.part"), work.join("info.rows")).map_err(|e| e.to_string())?;
    }
    if !have("cone/nat.rows") {
        let junk = w("nat-junk");
        fs::create_dir_all(&junk).map_err(|e| e.to_string())?;
        let nat = w("cone/nat.rows");
        // The dump exits with an error after writing the numbers; the file
        // is what counts.
        let _ = r.tool(
            "cas-lean-dump",
            &work,
            &[&w("export.txt"), &junk, "--order", &w("cone.txt"), "--names", &w("cone.txt")],
            &[("LEAN_DUMP_NAT", nat.as_str())],
            "nat.log",
        );
        let _ = fs::remove_dir_all(&junk);
        if !have("cone/nat.rows") {
            return Err(format!("no numbers were written; see {}", w("nat.log")));
        }
    }
    // 4. Batches of about 400k facts, in file order.
    if !have("batches.tsv") {
        let mut files: Vec<String> = fs::read_dir(work.join("rows"))
            .map_err(|e| e.to_string())?
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .collect();
        files.sort();
        let (mut b, mut s, mut out_t) = (0usize, 0usize, String::new());
        for f in &files {
            let text = fs::read_to_string(work.join("rows").join(f)).map_err(|e| e.to_string())?;
            let n = text.lines().filter(|l| l.starts_with('F')).count();
            if s + n > 400_000 && s > 0 {
                b += 1;
                s = 0;
            }
            s += n;
            out_t.push_str(&format!("{b}\t{f}\n"));
        }
        fs::write(work.join("batches.tsv"), out_t).map_err(|e| e.to_string())?;
    }
    let batches = fs::read_to_string(work.join("batches.tsv"))
        .map_err(|e| e.to_string())?
        .lines()
        .filter_map(|l| l.split('\t').next()?.parse::<usize>().ok())
        .max()
        .map_or(0, |m| m + 1)
        .to_string();
    // 5. The cone as tables, checked natively.
    if !have("cone/cone.bin") {
        r.say("the cone as tables");
        r.tool(
            "cas-lean-cone",
            &work,
            &["cone", &w("batches.tsv"), &w("rows"), &w("cone"), &w("info.rows")],
            &[],
            "cone.log",
        )?;
    }
    let cone = work.join("cone");
    // 6. Each batch's requests to the shared side, the multiplicity widths,
    // and the shared chunks.
    if !have("cone/asks.done") {
        r.say("the batches' requests");
        r.tool("cas-lean-tower", &cone, &["asks", &batches, "."], &[], "asks.log")?;
        for e in fs::read_dir(&cone).map_err(|e| e.to_string())?.flatten() {
            let p = e.path();
            if p.to_string_lossy().ends_with(".asks.new") {
                let to = p.to_string_lossy().trim_end_matches(".new").to_string();
                fs::rename(&p, to).map_err(|e| e.to_string())?;
            }
        }
        r.tool("cas-lean-tower", &cone, &["census", &batches, "."], &[], "census.log")?;
        r.tool("cas-lean-tower", &cone, &["shared", &batches, "."], &[], "shared.log")?;
        fs::write(cone.join("asks.done"), "").map_err(|e| e.to_string())?;
    }
    // 7. The segments, one process each; the rare rules' leaves (the last
    // segment) need the wider envelope.
    let run = |seg: &str, nu: &str, log: &str| {
        r.tool(
            "cas-lean-tower",
            &cone,
            &["run", &batches, ".", SLOTS, nu, "60", "31"],
            &[("LEAN_SEGMENTS", SEGMENTS), ("LEAN_SEGMENT", seg)],
            log,
        )
    };
    r.say("leaves committed (first pass)");
    let first = run("4294967295", "17", "first-pass.log")?;
    let total: usize = first
        .lines()
        .find_map(|l| {
            // "k of n segments proved", or, every segment already proved,
            // the join's "THE CONE in n proofs".
            l.strip_suffix(" segments proved")
                .and_then(|x| x.split(" of ").nth(1))
                .or_else(|| l.strip_prefix("THE CONE in ")?.split(' ').next())?
                .parse()
                .ok()
        })
        .ok_or("the first pass did not say how many segments there are")?;
    let common: usize = SEGMENTS.parse().unwrap();
    for s in 0..total {
        let nu = if s >= common { "18" } else { "17" };
        r.say(&format!("segment {} of {total}", s + 1));
        let log = run(&s.to_string(), nu, &format!("segment-{s}.log"))?;
        if let Some(l) = log.lines().find(|l| l.contains(" proved: leaves ")) {
            r.say(l);
        }
    }
    // 8. The join: the bus balanced over all segments, and the bundle.
    r.say("the join");
    let join = r.tool(
        "cas-lean-tower",
        &cone,
        &["run", &batches, ".", SLOTS, "17", "60", "31"],
        &[("LEAN_SEGMENTS", SEGMENTS)],
        "join.log",
    )?;
    let bundle = join
        .lines()
        .find_map(|l| l.strip_prefix("bundle ")?.split(':').next())
        .ok_or("the join wrote no bundle")?;
    let bundle = cone.join(bundle);
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let short_bytes = fs::read(&bundle).map_err(|e| e.to_string())?;
    let b = LeanBundle::from_bytes(&short_bytes)?;
    for f in &b.vks {
        let k = format!("vk-{}.vk", hex(f));
        fs::copy(cone.join(&k), out.join(&k)).map_err(|e| e.to_string())?;
    }
    // 9. The declarations, and long proofs of the units the succinct proof
    // does not check.
    let text = fs::read_to_string(work.join("export.txt")).map_err(|e| e.to_string())?;
    let env = Env::parse(&text)?;
    drop(text);
    let have_keys: HashSet<[F128; 2]> = b.statement.keys.iter().copied().collect();
    let idx = cas_lean::bundle::closure(&env, &[theorem.to_string()])?;
    let short: BTreeSet<String> = idx
        .iter()
        .map(|&i| env.name(env.decls[i].name))
        .filter(|n| !assumed.contains(n) && have_keys.contains(&key_words(n)))
        .collect();
    drop(env);
    r.say(&format!("{} units are checked by the succinct proof", short.len()));
    let p = long_bundle(&r, &work.join("export.txt"), theorem, &assumed, &short, short_bytes, &out)?;
    r.say(&format!("PROVED: {} and {} verification keys", p.display(), b.vks.len()));
    verify(&p.display().to_string(), &[])
}
