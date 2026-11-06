//! `cas`: a small command line over the CAS stages, for trying them out.
//!
//! The store is a directory (default `.cas`, or `$CAS_STORE`), or an
//! `http://` URL of a `cas serve`. Branch options default to `main`.
//! A served store with access control (spec/access.md) needs
//! `--token-file FILE`; tokens are never taken from the environment.

use cas_core::algo::{self, Algorithm};
use cas_core::client::HttpStore;
use cas_core::disk::DiskBackend;
use cas_core::server::Server;
use cas_core::{AllowAll, CoreStore, Policy, Store, Who};
use cas_version::access::{self, Access, Grant};
use cas_version::derived::{self, SUMMARY};
use cas_version::merge::{self, Outcome as MergeOutcome};
use cas_version::uni::Value;
use cas_version::{Node, Path, Ref, diff, formats, fs, gc, history, read_blob, sync, walk};
use std::collections::BTreeMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const USAGE: &str = "usage: cas [--store DIR|URL] [--token-file FILE] [--meta DIR] COMMAND
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
  branch NEW [-b BRANCH]                start branch NEW at BRANCH's head
  merge OTHER [-b BRANCH] [-m MESSAGE]  merge branch OTHER into BRANCH
  gc                                    one collection run (two-phase)
  purge KEY REASON                      delete a blob on purpose, leave a tombstone
  serve ADDR [--open] [--max-body BYTES] serve the store over HTTP, under the access
                                        policy in --meta, or with --open to anyone;
                                        request bodies up to BYTES (default 64 MiB)
  access show                           the policy in --meta (default .cas-meta)
  access user NAME                      add or re-key NAME; prints its token once
  access grant WHO 'ACTION SCOPE'       e.g. grant '*' 'read blobs'
  access revoke WHO 'ACTION SCOPE'";

type Res<T> = Result<T, String>;

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn open(spec: &str, token: Option<&str>) -> Res<Arc<dyn Store>> {
    if spec.starts_with("http://") || spec.starts_with("https://") {
        let h = HttpStore::new(spec.trim_end_matches('/'));
        return Ok(Arc::new(match token {
            Some(t) => h.with_token(t),
            None => h,
        }));
    }
    let backend = DiskBackend::open(spec).map_err(|e| format!("{spec}: {e}"))?;
    Ok(Arc::new(CoreStore::new(backend, AllowAll, Policy::permissive()).session(Who::anonymous())))
}

struct Args {
    store: String,
    branch: String,
    message: String,
    token_file: Option<String>,
    meta: String,
    rest: Vec<String>,
}

fn parse(mut argv: Vec<String>) -> Res<Args> {
    let mut a = Args {
        store: std::env::var("CAS_STORE").unwrap_or_else(|_| ".cas".into()),
        branch: "main".into(),
        message: String::new(),
        token_file: None,
        meta: ".cas-meta".into(),
        rest: Vec::new(),
    };
    argv.reverse();
    while let Some(arg) = argv.pop() {
        let mut value = |name: &str| argv.pop().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--store" => a.store = value("--store")?,
            "-b" => a.branch = value("-b")?,
            "-m" => a.message = value("-m")?,
            "--token-file" => a.token_file = Some(value("--token-file")?),
            "--meta" => a.meta = value("--meta")?,
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
    let token = match &a.token_file {
        Some(f) => Some(std::fs::read_to_string(f).map_err(|x| format!("{f}: {x}"))?.trim().to_string()),
        None => None,
    };
    let token = token.as_deref();
    let s = open(&a.store, token)?;
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
            let other = open(&arg(1, "STORE")?, token)?;
            let (src, dst) = if cmd == "push" { (s, &*other) } else { (&*other, s) };
            let (outcome, n) = sync::push(src, dst, &a.branch).map_err(e)?;
            println!("{outcome:?}: {} blobs copied, {} already present", n.blobs, n.present);
        }
        Some("branch") => {
            let new = arg(1, "NEW")?;
            let r = head(s, &a.branch)?;
            s.write(&new, r.to_string().as_bytes(), &cas_core::Expect::Absent).map_err(|x| format!("{new}: {x}"))?;
            println!("{new} at {}", short(r.key()));
        }
        Some("merge") => {
            let other = arg(1, "OTHER")?;
            let theirs = head(s, &other)?;
            let mut meta = BTreeMap::from([(Value::str("time"), Value::str(now()))]);
            let msg = if a.message.is_empty() { format!("Merge {other} into {}", a.branch) } else { a.message.clone() };
            meta.insert(Value::str("message"), Value::str(msg));
            match merge::merge_branch(s, b3(), &a.branch, &theirs, meta).map_err(e)? {
                MergeOutcome::UpToDate => println!("UpToDate"),
                MergeOutcome::FastForward(r) => println!("FastForward {}", short(r.key())),
                MergeOutcome::Merged(r) => println!("Merged {}", short(r.key())),
                MergeOutcome::Conflicts(m) => {
                    for c in &m.conflicts {
                        println!(
                            "conflict {:8} {:8} {:8} {}",
                            node_text(&c.base),
                            node_text(&c.ours),
                            node_text(&c.theirs),
                            c.path
                        );
                    }
                    return Err(format!("{} conflicts; nothing recorded", m.conflicts.len()));
                }
            }
        }
        Some("gc") => {
            let r = gc::collect(s, b3()).map_err(e)?;
            println!("{} reachable, {} unreachable, {} removed", r.reachable, r.unreachable, r.removed.len());
        }
        Some("purge") => {
            let (key, reason) = (arg(1, "KEY")?, arg(2, "REASON")?);
            gc::purge(s, &key, &reason).map_err(e)?;
            println!("purged {key}");
        }
        Some("access") => {
            let meta = open(&a.meta, token)?;
            let (old, mut rules) = access::load(&*meta).map_err(e)?;
            match a.rest.get(1).map(String::as_str) {
                Some("show") => {
                    for n in rules.users.keys() {
                        println!("user  {n}");
                    }
                    for (w, gs) in &rules.grants {
                        let gs: Vec<String> = gs.iter().map(Grant::to_string).collect();
                        println!("grant {w}: {}", gs.join(", "));
                    }
                    return Ok(());
                }
                Some("user") => {
                    let t = rules.add_user(&arg(2, "NAME")?).map_err(e)?;
                    access::save(&*meta, b3(), old, &rules).map_err(e)?;
                    println!("{t}");
                    return Ok(());
                }
                Some(op @ ("grant" | "revoke")) => {
                    let (who, g) = (arg(2, "WHO")?, Grant::parse(&arg(3, "GRANT")?).map_err(e)?);
                    let set = rules.grants.entry(who.clone()).or_default();
                    if op == "grant" {
                        set.insert(g);
                    } else {
                        set.remove(&g);
                    }
                    if set.is_empty() {
                        rules.grants.remove(&who);
                    }
                }
                _ => return Err(USAGE.into()),
            }
            // Round-trip through the document, so a bad grantee is refused here.
            access::Rules::from_value(&rules.to_value()).map_err(e)?;
            access::save(&*meta, b3(), old, &rules).map_err(e)?;
        }
        Some("serve") => {
            let addr = arg(1, "ADDR")?;
            let max_body = match a.rest.iter().position(|x| x == "--max-body") {
                Some(i) => a.rest.get(i + 1).and_then(|n| n.parse().ok()).ok_or("--max-body takes a byte count")?,
                None => cas_core::server::DEFAULT_MAX_BODY,
            };
            let backend = DiskBackend::open(&a.store).map_err(e)?;
            let server = if a.rest.iter().any(|x| x == "--open") {
                eprintln!("serving without access control");
                let core = CoreStore::new(backend, AllowAll, Policy::permissive());
                Server::start_with_max_body(
                    core,
                    &addr,
                    8,
                    Arc::new(|_: &tiny_http::Request| Who::anonymous()),
                    max_body,
                )
            } else {
                let meta = open(&a.meta, None)?;
                if access::load(&*meta).map_err(e)?.0.is_none() {
                    eprintln!("no policy in {}: every request is denied (see `cas access`)", a.meta);
                }
                let gate = Access::new(meta);
                let core = CoreStore::new(backend, gate.clone(), Policy::permissive());
                Server::start_with_max_body(core, &addr, 8, gate.authenticate(), max_body)
            }
            .map_err(e)?;
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
