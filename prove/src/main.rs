//! cas-prove prove|verify --store <dir> <manifest key>...
//!
//! Run by `cas prove` and `cas verify`. For each manifest it prints one
//! line; it fails if any proof could not be made or does not verify.

use cas_core::disk::DiskBackend;
use cas_core::{AllowAll, CoreStore, Policy, Store, Who};
use std::process::ExitCode;
use std::time::Instant;

fn run(args: &[String]) -> Result<bool, String> {
    let usage = "usage: cas-prove prove|verify --store <dir> <manifest key>...";
    let [cmd, flag, store, keys @ ..] = args else { return Err(usage.into()) };
    if flag != "--store" || keys.is_empty() || !["prove", "verify"].contains(&cmd.as_str()) {
        return Err(usage.into());
    }
    let backend = DiskBackend::open(store).map_err(|e| format!("{store}: {e}"))?;
    let session = CoreStore::new(backend, AllowAll, Policy::permissive()).session(Who::anonymous());
    let s: &dyn Store = &session;
    let mut ok = true;
    for key in keys {
        let start = Instant::now();
        let outcome = if cmd == "prove" {
            match cas_prove::recorded(s, key)? {
                Some(p) => Ok(format!("already proven, {} bytes", p.len())),
                None => cas_prove::prove_and_record(s, key).map(|n| format!("proven, {n} bytes")),
            }
        } else {
            cas_prove::verify(s, key).map(|()| "verified".to_string())
        };
        match outcome {
            Ok(what) => println!("{key}  {what}  {:.2}s", start.elapsed().as_secs_f64()),
            Err(why) => {
                println!("{key}  failed: {why}");
                ok = false;
            }
        }
    }
    Ok(ok)
}

fn main() -> ExitCode {
    // A circuit that is not satisfied stops the prover; that is reported
    // as a failure of that manifest, not as a crash.
    std::panic::set_hook(Box::new(|_| {}));
    match run(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(msg) => {
            eprintln!("{msg}");
            ExitCode::from(2)
        }
    }
}
