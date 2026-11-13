//! Reads an export of `measure/Export.lean` and prints what it holds.
//! Usage: `cargo run --release --example lean_env -- <export>`.
use cas_lean::import::{Env, Kind};
use std::collections::HashMap;

fn main() {
    let path = std::env::args().nth(1).expect("an export file");
    let text = std::fs::read_to_string(&path).expect("readable");
    let t0 = std::time::Instant::now();
    let env = Env::parse(&text).expect("a valid export");
    let mut kinds: HashMap<&str, usize> = HashMap::new();
    let mut failed = 0;
    for d in &env.decls {
        let k = match d.kind {
            Kind::Axiom => "axiom",
            Kind::Def { .. } => "def",
            Kind::Thm { .. } => "thm",
            Kind::Opaque { .. } => "opaque",
            Kind::Quot(_) => "quot",
            Kind::Induct { .. } => "induct",
            Kind::Ctor { .. } => "ctor",
            Kind::Rec { .. } => "rec",
        };
        *kinds.entry(k).or_default() += 1;
        if env.term(d.ty, &d.lparams, &mut HashMap::new()).is_err() {
            failed += 1;
        }
    }
    println!("{} declarations in {:?}: {kinds:?}; types not read: {failed}", env.decls.len(), t0.elapsed());
    let add = env.find("Nat.add").expect("Nat.add");
    println!("Nat.add : {:?}", env.term(add.ty, &add.lparams, &mut HashMap::new()).unwrap());
}
