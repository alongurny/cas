//! `cas`: a small command line over the CAS stages, for trying them out.
//!
//! The store is a directory (default `.cas`, or `$CAS_STORE`), or an
//! `http://` URL of a `cas serve`. Branch options default to `main`.

use cas_core::algo::{self, Algorithm};
use cas_core::client::HttpStore;
use cas_core::disk::DiskBackend;
use cas_core::server::Server;
use cas_core::{AllowAll, CoreStore, Policy, Store, Who};
use cas_version::derived::{self, SUMMARY};
use cas_version::uni::Value;
use cas_version::{Node, Path, Ref, diff, formats, fs, history, read_blob, sync, walk};
use std::collections::BTreeMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const USAGE: &str = "usage: cas [--store DIR|URL] COMMAND
  commit DIR [-b BRANCH] [-m MESSAGE]   record DIR as a new version on BRANCH
  log [-b BRANCH]                       list records, newest first
  ls [-b BRANCH] [PREFIX]               list paths of the head version
  diff [-b BRANCH]                      changes made by the head record
  checkout DEST [-b BRANCH] [PREFIX]    write the head version (or a prefix) to DEST
  summary [-b BRANCH]                   directory and file counts (derived, cached)
  path-log PATH [-b BRANCH]             records that changed PATH
  show KEY                              print a blob: uni-bin as text, else raw
  push STORE [-b BRANCH]                send BRANCH to another store
  pull STORE [-b BRANCH]                fetch BRANCH from another store
  serve ADDR                            serve the store over HTTP";

type Res<T> = Result<T, String>;

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn open(spec: &str) -> Res<Arc<dyn Store>> {
    if spec.starts_with("http://") {
        return Ok(Arc::new(HttpStore::new(spec.trim_end_matches('/'))));
    }
    let backend = DiskBackend::open(spec).map_err(|e| format!("{spec}: {e}"))?;
    Ok(Arc::new(CoreStore::new(backend, AllowAll, Policy::permissive()).session(Who::anonymous())))
}

struct Args {
    store: String,
    branch: String,
    message: String,
    rest: Vec<String>,
}

fn parse(mut argv: Vec<String>) -> Res<Args> {
    let mut a = Args {
        store: std::env::var("CAS_STORE").unwrap_or_else(|_| ".cas".into()),
        branch: "main".into(),
        message: String::new(),
        rest: Vec::new(),
    };
    argv.reverse();
    while let Some(arg) = argv.pop() {
        let mut value = |name: &str| argv.pop().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--store" => a.store = value("--store")?,
            "-b" => a.branch = value("-b")?,
            "-m" => a.message = value("-m")?,
            _ => a.rest.push(arg),
        }
    }
    Ok(a)
}

fn e(x: impl std::fmt::Display) -> String {
    x.to_string()
}

fn head(s: &dyn Store, branch: &str) -> Res<Ref> {
    history::head(s, branch).map_err(e)?.ok_or(format!("no branch {branch}"))
}

/// UTC time as RFC 3339, from the system clock.
fn now() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem / 60 % 60, rem % 60)
}

fn meta_str(rec: &history::Record, key: &str) -> String {
    rec.meta.get(&Value::str(key)).and_then(Value::as_str).unwrap_or("").to_string()
}

fn node_text(n: &Option<Node>) -> String {
    match n {
        None => "-".into(),
        Some(Node::Dir) => "dir".into(),
        Some(Node::File(r)) => r.known_format().map_or("?", |f| f.alias).to_string(),
    }
}

fn run(a: Args) -> Res<()> {
    let s = open(&a.store)?;
    let s = &*s;
    let arg = |i: usize, what: &str| a.rest.get(i).cloned().ok_or(format!("missing {what}\n{USAGE}"));
    let prefix = |i: usize| a.rest.get(i).map(|p| Path::new(p.as_str()).map_err(e)).transpose();
    match a.rest.first().map(String::as_str) {
        Some("commit") => {
            let dir = arg(1, "DIR")?;
            let root = fs::import(s, b3(), formats::by_alias("dir").unwrap(), dir.as_ref()).map_err(e)?;
            formats::publish(s).map_err(e)?;
            let mut meta = BTreeMap::from([(Value::str("time"), Value::str(now()))]);
            if let Ok(user) = std::env::var("USER") {
                meta.insert(Value::str("author"), Value::str(user));
            }
            if !a.message.is_empty() {
                meta.insert(Value::str("message"), Value::str(&*a.message));
            }
            let r = history::commit(s, b3(), &a.branch, &root, meta).map_err(e)?;
            println!("{}  {}", r.key(), root);
        }
        Some("log") => {
            for (r, rec) in history::log(s, &head(s, &a.branch)?).map_err(e)? {
                let parents: Vec<String> = rec.parents.iter().map(|p| short(p.key())).collect();
                println!(
                    "{}  {}  {}  [{}]",
                    short(r.key()),
                    meta_str(&rec, "time"),
                    meta_str(&rec, "message"),
                    parents.join(" ")
                );
            }
        }
        Some("ls") => {
            let rec = history::read(s, &head(s, &a.branch)?).map_err(e)?;
            for item in walk(s, &rec.version, prefix(1)?.as_ref()) {
                let (p, n) = item.map_err(e)?;
                println!("{:8} {p}", node_text(&Some(n)));
            }
        }
        Some("diff") => {
            let r = head(s, &a.branch)?;
            let rec = history::read(s, &r).map_err(e)?;
            let Some(parent) = rec.parents.iter().next() else { return Err("the head record has no parent".into()) };
            let old = history::read(s, parent).map_err(e)?.version;
            for c in diff(s, &old, &rec.version, None).map_err(e)? {
                println!("{:8} -> {:8} {}", node_text(&c.old), node_text(&c.new), c.path);
            }
        }
        Some("checkout") => {
            let dest = arg(1, "DEST")?;
            let rec = history::read(s, &head(s, &a.branch)?).map_err(e)?;
            let n = fs::checkout(s, &rec.version, prefix(2)?.as_ref(), dest.as_ref()).map_err(e)?;
            println!("{n} paths written to {dest}");
        }
        Some("summary") => {
            let rec = history::read(s, &head(s, &a.branch)?).map_err(e)?;
            let r = derived::derive(s, b3(), &SUMMARY, &rec.version).map_err(e)?;
            println!("{}", derived::value(s, &r).map_err(e)?);
        }
        Some("path-log") => {
            let p = Path::new(arg(1, "PATH")?).map_err(e)?;
            for r in derived::path_log(s, b3(), &head(s, &a.branch)?, &p).map_err(e)? {
                let rec = history::read(s, &r).map_err(e)?;
                println!("{}  {}  {}", short(r.key()), meta_str(&rec, "time"), meta_str(&rec, "message"));
            }
        }
        Some("show") => {
            let bytes = read_blob(s, &arg(1, "KEY")?).map_err(e)?;
            match Value::deserialize(&bytes) {
                Ok(v) => println!("{v}"),
                Err(_) => print!("{}", String::from_utf8_lossy(&bytes)),
            }
        }
        Some(cmd @ ("push" | "pull")) => {
            let other = open(&arg(1, "STORE")?)?;
            let (src, dst) = if cmd == "push" { (s, &*other) } else { (&*other, s) };
            let (outcome, n) = sync::push(src, dst, &a.branch).map_err(e)?;
            println!("{outcome:?}: {} blobs copied, {} already present", n.blobs, n.present);
        }
        Some("serve") => {
            let addr = arg(1, "ADDR")?;
            let backend = DiskBackend::open(&a.store).map_err(e)?;
            let core = CoreStore::new(backend, AllowAll, Policy::permissive());
            let server =
                Server::start(core, &addr, 8, Arc::new(|_: &tiny_http::Request| Who::anonymous())).map_err(e)?;
            println!("serving {} at {}", a.store, server.url());
            loop {
                std::thread::park();
            }
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}

fn short(key: &str) -> String {
    key.split_once(':').map_or(key, |(_, d)| &d[..12.min(d.len())]).to_string()
}

fn main() -> ExitCode {
    match parse(std::env::args().skip(1).collect()).and_then(run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("{msg}");
            ExitCode::FAILURE
        }
    }
}
