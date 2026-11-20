//! `cas`: a small command line over the CAS stages, for trying them out.
//!
//! The store is a directory (default `.cas`, or `$CAS_STORE`), or an
//! `https://` URL of a `cas serve` (HTTPS only). The history option defaults to `main`.
//! A served store with access control (spec/access.md) needs
//! `--token-file <file>`; tokens are never taken from the environment.

/// glibc's malloc spent a quarter of fsck finding room for the values
/// deltas decode into (git.git fsck 37.6 -> 25.3 s, gc 15.9 -> 13.4 s).
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use cas_core::algo::{self, Algorithm};
use cas_core::client::HttpStore;
use cas_core::disk::DiskBackend;
use cas_core::server::{Request, Server, Tls};
use cas_core::{AllowAll, Backend, CoreStore, Policy, Store, Who};
use cas_version::access::{self, Access, Grant};
use cas_version::annotated;
use cas_version::uni::Value;
use cas_version::{View, formats, gc, read_blob, sync};
use std::process::ExitCode;
use std::sync::Arc;

// `println!` that ends the process quietly when stdout is closed, as when
// piped into `head`. Output is buffered (a line-buffered stdout costs a write call per line:
// 330,000 of them for `objects` on git.git) and flushed when `run` ends.
thread_local! {
    static OUT: std::cell::RefCell<std::io::BufWriter<std::io::Stdout>> =
        std::cell::RefCell::new(std::io::BufWriter::with_capacity(1 << 16, std::io::stdout()));
}

macro_rules! out {
    ($($t:tt)*) => {{
        use std::io::Write;
        if crate::OUT.with(|o| writeln!(o.borrow_mut(), $($t)*)).is_err() {
            std::process::exit(0);
        }
    }};
}

mod commits;
mod forge;
mod help;
mod lean;

fn flush_out() {
    use std::io::Write;
    if OUT.with(|o| o.borrow_mut().flush()).is_err() {
        std::process::exit(0);
    }
}

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

/// Another store, reached with the same token and certificate options.
pub(crate) fn other_store(a: &Args, spec: &str) -> Res<Arc<dyn Store>> {
    let token = match &a.token_file {
        Some(f) => Some(std::fs::read_to_string(f).map_err(|x| format!("{f}: {x}"))?.trim().to_string()),
        None => None,
    };
    let ca = match &a.ca {
        Some(f) => Some(std::fs::read(f).map_err(|x| format!("{f}: {x}"))?),
        None => None,
    };
    open(spec, Remote { token: token.as_deref(), ca: ca.as_deref() })
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
    changelog: Option<String>,
    full: bool,
    notes: Option<String>,
    annotate: Vec<String>,
    show_diff: bool,
    annotated: bool,
    author: Option<String>,
    tag: Option<String>,
    change: Option<String>,
    all: bool,
    quiet: bool,
    from: Option<String>,
    help: bool,
    version: bool,
    remote: Option<String>,
    token_file: Option<String>,
    ca: Option<String>,
    meta: String,
    rest: Vec<String>,
    /// The options other than the general ones that `parse` took after the
    /// command, so that it can refuse one it does not read.
    flags: Vec<String>,
}

fn parse(mut argv: Vec<String>) -> Res<Args> {
    let mut a = Args {
        store: std::env::var("CAS_STORE").unwrap_or_else(|_| ".cas".into()),
        history: "main".into(),
        node: "latest".into(),
        changelog: None,
        full: false,
        notes: None,
        annotate: Vec::new(),
        show_diff: false,
        annotated: false,
        author: None,
        tag: None,
        change: None,
        all: false,
        quiet: false,
        from: None,
        help: false,
        version: false,
        remote: None,
        token_file: None,
        ca: None,
        meta: ".cas-meta".into(),
        rest: Vec::new(),
        flags: Vec::new(),
    };
    argv.reverse();
    while let Some(arg) = argv.pop() {
        // Options before the command may be for any command, as a script
        // that gives --author to all of them does; after it, only its own.
        if !a.rest.is_empty() && help::OPTIONS.iter().any(|o| !o.general && o.flag == arg) && !a.flags.contains(&arg) {
            a.flags.push(arg.clone());
        }
        let mut value = |name: &str| argv.pop().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--store" => a.store = value("--store")?,
            "-H" => a.history = value("-H")?,
            "-n" => a.node = value("-n")?,
            "-m" => {
                return Err("cas has no commit messages: a step says why through its notes \
                            (commit --notes <file>), a release through publish --changelog <file>"
                    .into());
            }
            "--changelog" => a.changelog = Some(value("--changelog")?),
            "--full" => a.full = true,
            "--notes" => a.notes = Some(value("--notes")?),
            "--annotate" => a.annotate.push(value("--annotate")?),
            "--diff" => a.show_diff = true,
            "--annotated" => a.annotated = true,
            "--author" => a.author = Some(value("--author")?),
            "--tag" => a.tag = Some(value("--tag")?),
            "--change" => a.change = Some(value("--change")?),
            "--all" => a.all = true,
            "--quiet" => a.quiet = true,
            "--from" => a.from = Some(value("--from")?),
            "--help" | "-h" => a.help = true,
            "--version" => a.version = true,
            "--as" => a.remote = Some(value("--as")?),
            "--token-file" => a.token_file = Some(value("--token-file")?),
            "--ca" => a.ca = Some(value("--ca")?),
            "--meta" => a.meta = value("--meta")?,
            "-j" => {
                let n = value("-j")?;
                cas_core::threads::set(n.parse().map_err(|_| format!("-j wants a number of threads, not {n:?}"))?);
            }
            _ => a.rest.push(arg),
        }
    }
    Ok(a)
}

fn e(x: impl std::fmt::Display) -> String {
    x.to_string()
}

/// The commands about cas itself, which open no store. True when one ran.
fn tool(a: &Args) -> Res<bool> {
    let print = |text: String| std::io::Write::write_all(&mut std::io::stdout(), text.as_bytes()).map_err(e);
    let first = a.rest.first().map(String::as_str);
    if a.version || first == Some("version") {
        print(format!("{}\n", help::version()))?;
        return Ok(true);
    }
    if a.help || first == Some("help") {
        let about = a.rest.iter().map(String::as_str).find(|w| *w != "help");
        print(match about {
            Some("--man") => help::man(),
            Some("--markdown") => help::markdown(),
            Some(c) => help::command(c)?,
            None => help::overview(),
        })?;
        return Ok(true);
    }
    match first {
        Some("completions") => {
            let shell = a.rest.get(1).ok_or_else(|| format!("missing <shell>\n{}", help::usage("completions")))?;
            print(help::completions(shell)?)?;
        }
        // For the completions: the histories of a store directory, read
        // from its variables' file names without opening the store.
        Some("__histories") => {
            let names = histories(&a.store);
            print(names.iter().map(|n| format!("{n}\n")).collect())?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// The names in a store directory that are not derived results, in order.
fn histories(store: &str) -> Vec<String> {
    let mut names = Vec::new();
    for f in std::fs::read_dir(std::path::Path::new(store).join("vars")).into_iter().flatten().flatten() {
        let hex = f.file_name().to_string_lossy().into_owned();
        let bytes: Option<Vec<u8>> =
            (0..hex.len() / 2).map(|i| u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()).collect();
        if let Some(name) = bytes.and_then(|b| String::from_utf8(b).ok())
            && hex.len() % 2 == 0
            && !name.starts_with("derived/")
            && !name.starts_with("lean/")
        {
            names.push(name);
        }
    }
    names.sort();
    names
}

/// Prove what each history of commits in `store` newly holds, whenever one
/// of them moves: the server's part of a push (docs/proofs.md).
fn prove_on_change(store: String) {
    use cas_core::Span;
    let span = |from: Option<&str>, to: Option<&str>| Span { from: from.map(Into::into), to: to.map(Into::into) };
    // Every variable but the derived results; blobs are between the first two.
    let spans = [span(None, Some("blake3:")), span(Some("blake3;"), Some("derived/")), span(Some("derived0"), None)];
    let mut proven: std::collections::HashMap<String, View> = std::collections::HashMap::new();
    let exe = std::env::current_exe().unwrap_or_else(|_| "cas".into());
    let Ok(s) = open(&store, Remote { token: None, ca: None }) else { return };
    loop {
        let names = spans.iter().flat_map(|sp| cas_core::list_all(&*s, sp).unwrap_or_default());
        for name in names {
            let Ok(Some(view)) = cas_version::head(&*s, &name) else { continue };
            if !cas_version::commits::is_commit(&view) || proven.get(&name) == Some(&view) {
                continue;
            }
            let out = std::process::Command::new(&exe)
                .args(["--store", &store, "-H", &name, "prove", "--all", "--quiet"])
                .output();
            match out {
                Ok(o) if o.status.success() => eprint!("{name}: {}", String::from_utf8_lossy(&o.stdout)),
                Ok(o) => eprint!("{name}: not proven: {}", String::from_utf8_lossy(&o.stderr)),
                Err(x) => eprintln!("{name}: not proven: {x}"),
            }
            proven.insert(name, view);
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn run(a: Args) -> Res<()> {
    if tool(&a)? {
        return Ok(());
    }
    help::check_words(&a.flags, &a.rest)?;
    // A word that is no command is refused before a store is opened.
    let mut before = a.rest.iter().peekable();
    while before.next_if(|w| *w == "--json").is_some()
        || before.next_if(|w| *w == "--project").is_some_and(|_| before.next().is_some())
    {}
    let cmd = match before.next() {
        None => return Err(help::overview()),
        Some(x) if x.starts_with('-') => {
            return Err(format!("no option {x}\n`cas help` lists the options."));
        }
        Some(x) if help::find(x).is_none() => {
            return Err(format!("no command {x}\n`cas help` lists the commands."));
        }
        Some(x) => x.as_str(),
    };
    // Each command that can make a store takes a word, asked for before
    // a store is made for nothing.
    if a.rest.len() < 2 && ["commit", "import-git", "pull", "serve", "access", "annotated"].contains(&cmd) {
        return Err(format!("missing {}\n{}", help::first_word(cmd), help::usage(cmd)));
    }
    if cmd == "commit" && !std::path::Path::new(&a.rest[1]).is_dir() {
        return Err(format!("{} is not a directory", a.rest[1]));
    }
    // Only a command that writes makes a store directory.
    const MAKES: &[&str] = &["commit", "import-git", "pull", "serve", "access", "annotated"];
    if !a.store.contains("://")
        && !MAKES.contains(&cmd)
        && !(cmd == "lean" && a.rest.get(1).is_some_and(|w| w == "import"))
        && !forge::COMMANDS.contains(&cmd)
        && !std::path::Path::new(&a.store).join("blobs").is_dir()
    {
        return Err(format!(
            "no store at {}: commit, import-git or pull makes one, and --store names another",
            a.store
        ));
    }
    // Forge options may come before the command: keep them for it.
    let mut words = a.rest.clone();
    let mut lead = Vec::new();
    while words.first().is_some_and(|w| w == "--json" || w == "--project") {
        let w = words.remove(0);
        if w == "--project" && !words.is_empty() {
            lead.push(w);
            lead.push(words.remove(0));
        } else {
            lead.push(w);
        }
    }
    if let Some(cmd) = words.first()
        && forge::COMMANDS.contains(&cmd.as_str())
    {
        words.extend(lead);
        let ca = match &a.ca {
            Some(f) => Some(std::fs::read(f).map_err(|x| format!("{f}: {x}"))?),
            None => None,
        };
        return forge::run(&a, &words, ca.as_deref());
    }
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
    if cmd == "lean" {
        return lean::run(&a, s);
    }
    if commits::handles(&a, s)? {
        return commits::run(&a, s);
    }
    let arg = |i: usize, what: &str| {
        a.rest
            .get(i)
            .cloned()
            .ok_or_else(|| format!("missing {what}\n{}", help::usage(a.rest.first().map_or("", String::as_str))))
    };
    let name = a.history.as_str();
    match a.rest.first().map(String::as_str) {
        Some("annotated") => match a.rest.get(1).map(String::as_str) {
            Some("make") => {
                let file = arg(2, "<file>")?;
                let form = std::fs::read(&file).map_err(|x| format!("{file}: {x}"))?;
                let (text, notes) = annotated::parse(&form).map_err(|x| format!("{file}: {x}"))?;
                let target = cas_version::put(s, b3(), "raw", &text).map_err(e)?;
                formats::publish(s).map_err(e)?;
                let v = annotated::write(s, b3(), &target, &notes).map_err(e)?;
                out!("{v}");
            }
            Some("show") => {
                let key = arg(2, "<key>")?;
                let form = annotated::show(s, &key).map_err(e)?;
                std::io::Write::write_all(&mut std::io::stdout(), &form).map_err(e)?;
            }
            _ => return Err(help::usage("annotated")),
        },
        Some("objects") => {
            for k in cas_core::list_all(s, &cas_core::Span::all()).map_err(e)? {
                if matches!(cas_core::key::classify(&k), Some(cas_core::key::Parsed::Blob { .. })) {
                    out!("{k}");
                }
            }
        }
        Some("watch") => {
            let dir = arg(1, "<dir>")?;
            let w = cas_version::watch::Watcher::start(dir.as_ref()).map_err(e)?;
            eprintln!("watching {dir}");
            w.run().map_err(e)?;
        }
        Some("fsck") => {
            let r = cas_version::fsck::fsck(s).map_err(e)?;
            for (holder, k) in &r.missing {
                out!("missing {k} (named by {holder})");
            }
            for (k, why) in &r.bad {
                out!("bad {k}: {why}");
            }
            out!(
                "{} checked, {} unreachable, {} missing, {} bad",
                r.checked,
                r.unreachable,
                r.missing.len(),
                r.bad.len()
            );
            if !r.ok() {
                return Err("the store has problems".into());
            }
        }
        Some("show") => {
            let bytes = read_blob(s, &arg(1, "<key>")?).map_err(e)?;
            match Value::deserialize(&bytes) {
                Ok(v) => out!("{v}"),
                Err(_) => print!("{}", String::from_utf8_lossy(&bytes)),
            }
        }
        Some(cmd @ ("push" | "pull")) => {
            let other = open(&arg(1, "<store>")?, remote)?;
            let remote = a.remote.clone().unwrap_or_else(|| name.to_string());
            let (outcome, n) = if cmd == "push" {
                sync::push(s, &*other, name, &remote)
            } else {
                sync::push(&*other, s, &remote, name)
            }
            .map_err(e)?;
            let what = match outcome {
                sync::Outcome::UpToDate => "up to date",
                sync::Outcome::Created => "created",
                sync::Outcome::FastForward => "fast-forward",
            };
            out!("{what}: {} blobs copied, {} already present", n.blobs, n.present);
        }
        Some("gc") => {
            let r = gc::collect_with(s, b3(), a.full).map_err(e)?;
            out!(
                "{} reachable, {} unreachable, {} removed ({})",
                r.reachable,
                r.unreachable,
                r.removed.len(),
                if r.full { "full" } else { "since the last full run" }
            );
            // A local store also drops the chunks no remaining blob uses.
            if !a.store.starts_with("http://") && !a.store.starts_with("https://") {
                let disk = DiskBackend::open(&a.store).map_err(e)?;
                // Chunks die with the blobs that name them, or are left loose
                // by a writer that stopped. A packed chunk of a blob removed
                // by a run since the last full one waits for the next.
                if r.full || disk.loose_chunks().map_err(e)? {
                    let n = disk.sweep_chunks().map_err(e)?;
                    out!("{n} chunks swept");
                }
                // Like git gc: rewrite into one pack, with deltas against each
                // path's previous version.
                if disk.chunked() {
                    // A full run hints everything again: a frame it would
                    // reuse may lose its base in this run.
                    let (order, hints) =
                        gc::delta_hints(s, &|k| !a.full && disk.sealed(k).unwrap_or(false)).map_err(e)?;
                    let r = disk.repack_with(&order, &hints, a.full).map_err(e)?;
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
                    let t = rules.add_user(&arg(2, "<name>")?).map_err(e)?;
                    access::save(&*meta, b3(), old, &rules).map_err(e)?;
                    out!("{t}");
                    return Ok(());
                }
                Some(op @ ("grant" | "revoke")) => {
                    let (who, g) = (arg(2, "<who>")?, Grant::parse(&arg(3, "<grant>")?).map_err(e)?);
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
                _ => return Err(help::usage("access")),
            }
            // Round-trip through the document, so a bad grantee is refused here.
            access::Rules::from_value(&rules.to_value()).map_err(e)?;
            access::save(&*meta, b3(), old, &rules).map_err(e)?;
        }
        Some("serve") => {
            let addr = arg(1, "<addr>")?;
            let max_body = match a.rest.iter().position(|x| x == "--max-body") {
                Some(i) => a.rest.get(i + 1).and_then(|n| n.parse().ok()).ok_or("--max-body takes a byte count")?,
                None => cas_core::server::DEFAULT_MAX_BODY,
            };
            let flag = |name: &str| {
                let i = a
                    .rest
                    .iter()
                    .position(|x| x == name)
                    .ok_or(format!("serve needs {name} <file> (it speaks only HTTPS)"))?;
                a.rest.get(i + 1).cloned().ok_or(format!("{name} needs a file"))
            };
            let tls = if a.rest.iter().any(|x| x == "--insecure-http") {
                insecure_http()?
            } else {
                Tls::from_files(flag("--cert")?.as_ref(), flag("--key")?.as_ref()).map_err(e)?
            };
            // With --prove, each history of commits is proven as it moves.
            let prove = a.rest.iter().any(|x| x == "--prove");
            if prove {
                commits::prover()?;
            }
            let backend = DiskBackend::open(&a.store).map_err(e)?;
            // A history moves only to a view whose blobs are all valid.
            let checker = Arc::new(cas_version::sync::Checker::new());
            let server = if a.rest.iter().any(|x| x == "--open") {
                eprintln!("serving without access control");
                let core = CoreStore::with_check(backend, Arc::new(AllowAll), Policy::permissive(), checker.clone());
                Server::start_with_max_body(core, &addr, 8, Arc::new(|_: &Request| Who::anonymous()), tls, max_body)
            } else {
                let meta = open(&a.meta, Remote { token: None, ..remote })?;
                if access::load(&*meta).map_err(e)?.0.is_none() {
                    eprintln!("no policy in {}: every request is denied (see `cas access`)", a.meta);
                }
                let gate = Access::new(meta);
                let core =
                    CoreStore::with_check(backend, Arc::new(gate.clone()), Policy::permissive(), checker.clone());
                Server::start_with_max_body(core, &addr, 8, gate.authenticate(), tls, max_body)
            }
            .map_err(e)?;
            out!("serving {} at {}", a.store, server.url());
            if prove {
                let store = a.store.clone();
                std::thread::spawn(move || prove_on_change(store));
            }
            loop {
                std::thread::park();
            }
        }
        Some(x) => return Err(format!("no command {x}\n`cas help` lists the commands.")),
        None => return Err(help::overview()),
    }
    Ok(())
}

fn short(key: &str) -> String {
    match key.split_once(':') {
        Some((alg, d)) => format!("{alg}:{}", &d[..12.min(d.len())]),
        None => key.to_string(),
    }
}

fn main() -> ExitCode {
    let r = parse(std::env::args().skip(1).collect()).and_then(run);
    flush_out();
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("{}", msg.trim_end());
            // 2 for a command that was not written correctly: such an error
            // points to the help.
            if msg.contains("`cas help") { ExitCode::from(2) } else { ExitCode::FAILURE }
        }
    }
}
