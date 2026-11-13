//! Prints a declaration of an export: its level parameters and type.
use cas_lean::import::Env;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let env = Env::parse(&std::fs::read_to_string(&a[1]).unwrap()).unwrap();
    let d = env.find(&a[2]).expect("no such declaration");
    let ps: Vec<String> = d.lparams.iter().map(|p| env.name(*p)).collect();
    println!("{} levels {:?} kind {:?}", a[2], ps, d.kind);
}
