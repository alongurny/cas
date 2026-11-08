//! `cas`: a small command line over the CAS stages, for trying them out.
//!
//! The store is a directory (default `.cas`, or `$CAS_STORE`), or an
//! `https://` URL of a `cas serve` (HTTPS only). The history option defaults to `main`.
//! A served store with access control (spec/access.md) needs
//! `--token-file FILE`; tokens are never taken from the environment.

use cas_core::algo::{self, Algorithm};
use cas_core::client::HttpStore;
use cas_core::disk::DiskBackend;
use cas_core::server::{Request, Server, Tls};
use cas_core::{AllowAll, Backend, CoreStore, Policy, Store, Who};
use cas_version::access::{self, Access, Grant};
use cas_version::derived::{self, SUMMARY};
use cas_version::history::{self, History, NodeId, Step};
use cas_version::merge::{self, Outcome as MergeOutcome};
use cas_version::uni::Value;
use cas_version::{Node, Path, View, diff, formats, fs, gc, read_blob, sync, walk};
use std::collections::BTreeMap;
use std::process::ExitCode;
use std::sync::Arc;

/// `println!` that ends the process quietly when stdout is closed, as when
/// piped into `head`.
macro_rules! out {
    ($($t:tt)*) => {{
        use std::io::Write;
        if writeln!(std::io::stdout(), $($t)*).is_err() {
            std::process::exit(0);
        }
    }};
}

const USAGE: &str = "usage: cas [--store DIR|URL] [--token-file FILE] [--ca FILE] [--meta DIR] COMMAND
A URL is https:// (the only protocol); --ca trusts the CA certificates in FILE
instead of the public ones.
History options: -H HISTORY (default main), -n NODE (a tag, `latest`, or a key
prefix; default latest), -m CHANGELOG, --doc TEXT, --compatible.
  commit DIR                    add DIR as a new node after the tip
  log                           nodes, newest first, with tags and changelogs
  ls [PREFIX]                   paths of a node's version
  diff                          changes from a node's predecessor
  checkout DEST [PREFIX]        write a node's version (or a prefix) to DEST
  summary                       directory and file counts (derived, cached)
  path-log PATH                 nodes whose step changed PATH
  tag NAME                      tag a node (-n) in the history
  compatible FROM TO            whether upgrading FROM to TO is compatible
  show KEY                      print a blob: uni-bin as text, else raw
  push STORE [--as NAME]        send the history (fast-forward only)
  pull STORE [--as NAME]        fetch history NAME (fast-forward only)
  rebase ONTO                   replay this history's own nodes onto history ONTO
  publish PUBLIC                add this history's tip to history PUBLIC as one node
  merge [OTHER]                 merge history OTHER in (or the two tips) as one node
  import-git REPO [REV]         import a git repository's commits (default HEAD)
                                as history -H (refused if it exists)
  gc                            one collection run (two-phase)
  serve ADDR --cert FILE --key FILE [--open] [--max-body BYTES]
                                serve the store over HTTPS (the only protocol)
                                with the PEM certificate chain and key, under
                                the access policy in --meta, or with --open to anyone;
                                request bodies up to BYTES (default 64 MiB)
  access show                   the policy in --meta (default .cas-meta)
  access user NAME              add or re-key NAME; prints its token once
  access grant WHO 'ACTION SCOPE'   e.g. grant '*' 'read blobs'
  access revoke WHO 'ACTION SCOPE'";

type Res<T> = Result<T, String>;

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

/// How to reach a remote store: its token and the CA certificates to trust.
#[derive(Clone, Copy)]
struct Remote<'a> {
    token: Option<&'a str>,
    ca: Option<&'a [u8]>,
}

#[cfg(feature = "bench-insecure")]
fn insecure_http() -> Res<Tls> {
    eprintln!("serving plain HTTP: a benchmark build");
    Ok(Tls::plain_http_for_benchmarks())
}

#[cfg(not(feature = "bench-insecure"))]
fn insecure_http() -> Res<Tls> {
    Err("--insecure-http exists only in benchmark builds (feature bench-insecure); cas speaks HTTPS".into())
}

fn open(spec: &str, r: Remote) -> Res<Arc<dyn Store>> {
    if spec.contains("://") {
        // Remote stores are reached over HTTPS only; HttpStore refuses the rest.
        let h = HttpStore::new(spec).map_err(e)?;
        let h = match r.ca {
            Some(pem) => h.trusting(pem).map_err(e)?,
            None => h,
        };
        return Ok(Arc::new(match r.token {
            Some(t) => h.with_token(t),
            None => h,
        }));
    }
    let backend = DiskBackend::open(spec).map_err(|e| format!("{spec}: {e}"))?;
    Ok(Arc::new(CoreStore::new(backend, AllowAll, Policy::permissive()).session(Who::anonymous())))
}

struct Args {
    store: String,
    history: String,
    node: String,
    message: Option<String>,
    doc: Option<String>,
    compatible: bool,
    remote: Option<String>,
    token_file: Option<String>,
    ca: Option<String>,
    meta: String,
    rest: Vec<String>,
}

fn parse(mut argv: Vec<String>) -> Res<Args> {
    let mut a = Args {
        store: std::env::var("CAS_STORE").unwrap_or_else(|_| ".cas".into()),
        history: "main".into(),
        node: "latest".into(),
        message: None,
        doc: None,
        compatible: false,
        remote: None,
        token_file: None,
        ca: None,
        meta: ".cas-meta".into(),
        rest: Vec::new(),
    };
    argv.reverse();
    while let Some(arg) = argv.pop() {
        let mut value = |name: &str| argv.pop().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--store" => a.store = value("--store")?,
            "-H" => a.history = value("-H")?,
            "-n" => a.node = value("-n")?,
            "-m" => a.message = Some(value("-m")?),
            "--doc" => a.doc = Some(value("--doc")?),
            "--compatible" => a.compatible = true,
            "--as" => a.remote = Some(value("--as")?),
            "--token-file" => a.token_file = Some(value("--token-file")?),
            "--ca" => a.ca = Some(value("--ca")?),
            "--meta" => a.meta = value("--meta")?,
            _ => a.rest.push(arg),
        }
    }
    Ok(a)
}

fn e(x: impl std::fmt::Display) -> String {
    x.to_string()
}

fn load(s: &dyn Store, name: &str) -> Res<(Option<View>, History)> {
    history::load(s, name).map_err(e)
}

fn node(h: &History, what: &str, name: &str) -> Res<NodeId> {
    h.resolve(what).ok_or(format!("no node {what} in history {name}"))
}

fn node_text(n: &Option<Node>) -> String {
    match n {
        None => "-".into(),
        Some(Node::Dir) => "dir".into(),
        Some(Node::File(r)) => r.known_format().map_or("?", |f| f.alias).to_string(),
    }
}

fn report(outcome: MergeOutcome) -> Res<()> {
    match outcome {
        MergeOutcome::UpToDate => out!("UpToDate"),
        MergeOutcome::Done(id) => out!("Done {}", short(id.version().key())),
        MergeOutcome::Conflicts(m) => {
            for c in &m.conflicts {
                out!(
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
    Ok(())
}

fn run(a: Args) -> Res<()> {
    let token = match &a.token_file {
        Some(f) => Some(std::fs::read_to_string(f).map_err(|x| format!("{f}: {x}"))?.trim().to_string()),
        None => None,
    };
    let ca = match &a.ca {
        Some(f) => Some(std::fs::read(f).map_err(|x| format!("{f}: {x}"))?),
        None => None,
    };
    let remote = Remote { token: token.as_deref(), ca: ca.as_deref() };
    let s = open(&a.store, remote)?;
    let s = &*s;
    let arg = |i: usize, what: &str| a.rest.get(i).cloned().ok_or(format!("missing {what}\n{USAGE}"));
    let prefix = |i: usize| a.rest.get(i).map(|p| Path::new(p.as_str()).map_err(e)).transpose();
    let name = a.history.as_str();
    let step = Step { changelog: a.message.clone(), compatible: a.compatible };
    let at = || -> Res<NodeId> { node(&load(s, name)?.1, &a.node, name) };
    match a.rest.first().map(String::as_str) {
        Some("commit") => {
            let dir = arg(1, "DIR")?;
            let root = fs::import(s, b3(), formats::by_alias("dir").unwrap(), dir.as_ref()).map_err(e)?;
            formats::publish(s).map_err(e)?;
            let id = history::commit(s, b3(), name, &root, step, a.doc.clone()).map_err(e)?;
            out!("{}", id);
        }
        Some("log") => {
            let h = load(s, name)?.1;
            let mut tags: BTreeMap<&NodeId, Vec<&str>> = BTreeMap::new();
            for (t, n) in &h.tags {
                tags.entry(n).or_default().push(t);
            }
            for id in h.log() {
                let n = &h.nodes[&id];
                let after: Vec<String> = n
                    .after
                    .iter()
                    .map(|(p, st)| format!("{}{}", short(p.version().key()), if st.compatible { "~" } else { "" }))
                    .collect();
                let notes: Vec<&str> = n.after.values().filter_map(|st| st.changelog.as_deref()).collect();
                let t = tags.get(&id).map(|t| format!(" ({})", t.join(", "))).unwrap_or_default();
                out!("{}{t}  [{}]  {}", short(id.version().key()), after.join(" "), notes.join(" / "));
            }
        }
        Some("ls") => {
            for item in walk(s, &at()?.version(), prefix(1)?.as_ref()) {
                let (p, n) = item.map_err(e)?;
                out!("{:8} {p}", node_text(&Some(n)));
            }
        }
        Some("diff") => {
            let h = load(s, name)?.1;
            let id = node(&h, &a.node, name)?;
            let Some(pred) = h.nodes[&id].after.keys().next() else { return Err("the node has no predecessor".into()) };
            for c in diff(s, &pred.version(), &id.version(), None).map_err(e)? {
                out!("{:8} -> {:8} {}", node_text(&c.old), node_text(&c.new), c.path);
            }
        }
        Some("checkout") => {
            let dest = arg(1, "DEST")?;
            let n = fs::checkout(s, &at()?.version(), prefix(2)?.as_ref(), dest.as_ref()).map_err(e)?;
            out!("{n} paths written to {dest}");
        }
        Some("summary") => {
            let r = derived::derive(s, b3(), &SUMMARY, &at()?.version()).map_err(e)?;
            out!("{}", derived::value(s, &r).map_err(e)?);
        }
        Some("path-log") => {
            let p = Path::new(arg(1, "PATH")?).map_err(e)?;
            let h = load(s, name)?.1;
            for id in derived::path_log(s, &h, &p).map_err(e)? {
                let notes: Vec<&str> = h.nodes[&id].after.values().filter_map(|st| st.changelog.as_deref()).collect();
                out!("{}  {}", short(id.version().key()), notes.join(" / "));
            }
        }
        Some("tag") => {
            let t = arg(1, "NAME")?;
            history::record(s, b3(), name, |h| {
                let id = h.resolve(&a.node).ok_or(cas_version::Error::Missing(format!("node {}", a.node)))?;
                h.tags.insert(t.clone(), id);
                Ok(())
            })
            .map_err(e)?;
        }
        Some("compatible") => {
            let h = load(s, name)?.1;
            let (x, y) = (node(&h, &arg(1, "FROM")?, name)?, node(&h, &arg(2, "TO")?, name)?);
            out!("{}", if h.compatible(&x, &y) { "compatible" } else { "breaking" });
        }
        Some("show") => {
            let bytes = read_blob(s, &arg(1, "KEY")?).map_err(e)?;
            match Value::deserialize(&bytes) {
                Ok(v) => out!("{v}"),
                Err(_) => print!("{}", String::from_utf8_lossy(&bytes)),
            }
        }
        Some(cmd @ ("push" | "pull")) => {
            let other = open(&arg(1, "STORE")?, remote)?;
            let remote = a.remote.clone().unwrap_or_else(|| name.to_string());
            let (outcome, n) = if cmd == "push" {
                sync::push(s, &*other, name, &remote)
            } else {
                sync::push(&*other, s, &remote, name)
            }
            .map_err(e)?;
            out!("{outcome:?}: {} blobs copied, {} already present", n.blobs, n.present);
        }
        Some("rebase") => {
            let onto = arg(1, "ONTO")?;
            let target = load(s, &onto)?.1;
            let (old, mine) = load(s, name)?;
            let (h, outcome) = merge::rebase(s, b3(), &mine, &target).map_err(e)?;
            if matches!(outcome, MergeOutcome::Done(_)) {
                history::save(s, b3(), name, old.as_ref(), &h).map_err(e)?;
            }
            report(outcome)?;
        }
        Some("publish") => {
            let public = arg(1, "PUBLIC")?;
            let mine = load(s, name)?.1;
            let (_, outcome) =
                history::record(s, b3(), &public, |p| merge::publish(&mine, p, step.clone(), a.doc.clone()))
                    .map_err(e)?;
            report(outcome)?;
        }
        Some("merge") => {
            let theirs = match a.rest.get(1) {
                Some(o) => Some(load(s, o)?.1),
                None => None,
            };
            let (old, mine) = load(s, name)?;
            let mut h = match &theirs {
                Some(t) => merge::combine(&mine, t),
                None => mine.clone(),
            };
            let tips: Vec<NodeId> = h.tips().into_iter().collect();
            let [x, y] = &tips[..] else { return Err(format!("{} tips; merge needs two", tips.len())) };
            // Ours first: the tip this history had.
            let (x, y) = if mine.nodes.contains_key(y) && !mine.nodes.contains_key(x) { (y, x) } else { (x, y) };
            let (x, y) = (x.clone(), y.clone());
            let outcome = merge::merge_nodes(s, b3(), &mut h, &x, &y, &step).map_err(e)?;
            if matches!(outcome, MergeOutcome::Done(_)) {
                history::save(s, b3(), name, old.as_ref(), &h).map_err(e)?;
            }
            report(outcome)?;
        }
        Some("import-git") => {
            let repo = arg(1, "REPO")?;
            let rev = a.rest.get(2).cloned().unwrap_or_else(|| "HEAD".into());
            let start = std::time::Instant::now();
            let (h, n) = cas_version::git::import(s, b3(), repo.as_ref(), &rev).map_err(e)?;
            formats::publish(s).map_err(e)?;
            history::save(s, b3(), name, None, &h).map_err(|x| format!("{name}: {x}"))?;
            out!(
                "{} commits, {} trees, {} files, {} submodules skipped, {} nodes, {} tags in {:.1}s",
                n.commits,
                n.trees,
                n.files,
                n.skipped,
                h.nodes.len(),
                h.tags.len(),
                start.elapsed().as_secs_f64()
            );
        }
        Some("gc") => {
            let r = gc::collect(s, b3()).map_err(e)?;
            out!("{} reachable, {} unreachable, {} removed", r.reachable, r.unreachable, r.removed.len());
            // A local store also drops the chunks no remaining blob uses.
            if !a.store.starts_with("http://") && !a.store.starts_with("https://") {
                let disk = DiskBackend::open(&a.store).map_err(e)?;
                let n = disk.sweep_chunks().map_err(e)?;
                out!("{n} chunks swept");
                // Like git gc: rewrite into one pack, with deltas against each
                // path's previous version.
                if disk.chunked() {
                    let (order, hints) = gc::delta_hints(s).map_err(e)?;
                    let r = disk.repack(&order, &hints).map_err(e)?;
                    out!("repacked: {} deltas, {} whole", r.deltas, r.whole);
                }
            }
        }
        Some("access") => {
            let meta = open(&a.meta, remote)?;
            let (old, mut rules) = access::load(&*meta).map_err(e)?;
            match a.rest.get(1).map(String::as_str) {
                Some("show") => {
                    for n in rules.users.keys() {
                        out!("user  {n}");
                    }
                    for (w, gs) in &rules.grants {
                        let gs: Vec<String> = gs.iter().map(Grant::to_string).collect();
                        out!("grant {w}: {}", gs.join(", "));
                    }
                    return Ok(());
                }
                Some("user") => {
                    let t = rules.add_user(&arg(2, "NAME")?).map_err(e)?;
                    access::save(&*meta, b3(), old, &rules).map_err(e)?;
                    out!("{t}");
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
            let flag = |name: &str| {
                let i = a
                    .rest
                    .iter()
                    .position(|x| x == name)
                    .ok_or(format!("serve needs {name} FILE (it speaks only HTTPS)"))?;
                a.rest.get(i + 1).cloned().ok_or(format!("{name} needs a file"))
            };
            let tls = if a.rest.iter().any(|x| x == "--insecure-http") {
                insecure_http()?
            } else {
                Tls::from_files(flag("--cert")?.as_ref(), flag("--key")?.as_ref()).map_err(e)?
            };
            let backend = DiskBackend::open(&a.store).map_err(e)?;
            let server = if a.rest.iter().any(|x| x == "--open") {
                eprintln!("serving without access control");
                let core = CoreStore::new(backend, AllowAll, Policy::permissive());
                Server::start_with_max_body(core, &addr, 8, Arc::new(|_: &Request| Who::anonymous()), tls, max_body)
            } else {
                let meta = open(&a.meta, Remote { token: None, ..remote })?;
                if access::load(&*meta).map_err(e)?.0.is_none() {
                    eprintln!("no policy in {}: every request is denied (see `cas access`)", a.meta);
                }
                let gate = Access::new(meta);
                let core = CoreStore::new(backend, gate.clone(), Policy::permissive());
                Server::start_with_max_body(core, &addr, 8, gate.authenticate(), tls, max_body)
            }
            .map_err(e)?;
            out!("serving {} at {}", a.store, server.url());
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
