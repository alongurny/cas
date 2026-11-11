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
use cas_version::derived::{self, SUMMARY};
use cas_version::graph;
use cas_version::history::{self, History, NodeId, Step};
use cas_version::merge::{self, Outcome as MergeOutcome};
use cas_version::uni::Value;
use cas_version::{Node, Path, View, diff, formats, fs, gc, read_blob, sync, walk};
use cas_version::{annotated, stepnotes};
use std::collections::BTreeMap;
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

fn flush_out() {
    use std::io::Write;
    if OUT.with(|o| o.borrow_mut().flush()).is_err() {
        std::process::exit(0);
    }
}

const USAGE: &str =
    "usage: cas [--store <dir>|<url>] [--token-file <file>] [--ca <file>] [--meta <dir>] [-j <threads>] <command>
A URL is https:// (the only protocol); --ca trusts the CA certificates in <file>
instead of the public ones. -j <threads> caps the threads a command uses
(default: one per core).
History options: -H <history> (default main), -n <node> (a tag, `latest`, or a key
prefix; default latest), --compatible. There is no commit
message: a step says why through its notes (commit --notes), and a release
through its changelog (publish --changelog).
With --commits, commit and import-git start a history of commits (spec/formats/commit.md:
versions as `directory`, notes as `annotation`, an author and a time on each commit;
--author <name>, default $CAS_AUTHOR or $USER). On such a history commit, status, reset,
clean, log, ls, diff, checkout, sparse, path-log, merge-base, merge <other>, rebase <onto>,\npush and pull work; stash, last-modified and summary are being moved over. From such a history\npublish --tag <semantic version> [--change breaking|feature|patch|security --changelog <file>]\nadds its tip to the release history (default releases; --as <name>): a first release has no\nchange, a later one says its change from the one before. On a release history (-H releases)\nlog, ls, checkout, sparse (-n <tag>) and compatible <from> <to> work.
  commit <dir>                    add <dir> as a new node after the tip (<dir>/.cas-index
                                  caches stat data, so unchanged files are not rehashed)
  commit <dir> --notes <file>     ... with a note on each line of the step's diff
                                  (<file>: what status --diff prints, notes added;
                                  a first node keeps them on its files' lines)
  commit <dir> --annotate <path>=<file>  ... with notes on the file <path> as it is
                                  (<file>: its lines as `| line`, with `# note` lines)
  notes [<path>]                  a node's annotated files, or <path>'s with its notes
  status <dir>                    how <dir> differs from the node (nothing is stored)
  status <dir> --diff             the line diff from the node to <dir>, to annotate
                                  (stores <dir>'s file blobs)
  watch <dir>                     follow <dir>'s changes (inotify) until stopped, so
                                  commit and status look only at what changed
  reset <dir>                     make <dir> the node's version again (discards changes)
  clean <dir>                     remove from <dir> the paths the node does not have
  stash <dir> | stash pop <dir>   keep <dir> in history stash/<history> and reset it, or
                                  put the newest stash back into <dir> and drop it
  log                             nodes, newest first, with tags and what each step says
  ls [<prefix>]                   paths of a node's version
  diff [--annotated]              changes from a node's predecessor (--annotated:
                                  the line diff with the step's notes)
  annotated make <file>           store <file>'s text and notes (the authoring form:
                                  `| line`, `# note`); prints the annotation's view
  annotated show <key>            an annotation's text with its notes
  checkout <dest> [<prefix>]      write a node's version (or a prefix) to <dest>
  sparse <dest> <path>...         check out only <path>s (files or directories) of a
                                  node into <dest>; a commit of <dest> keeps the rest
  summary                         directory and file counts (derived, cached)
  path-log <path>                 nodes whose step changed <path>
  tag <name>                      tag a node (-n) in the history
  compatible <from> <to>          whether upgrading <from> to <to> is compatible
  merge-base <a> <b>              best common ancestors of nodes <a> and <b>
  last-modified [<prefix>]        each entry under <prefix> with the node that last changed it
  objects                         every blob key in the store
  fsck                            check every reachable blob: readable, valid, complete
  show <key>                      print a blob: uni-bin as text, else raw
  push <store> [--as <name>]      send the history (fast-forward only)
  pull <store> [--as <name>]      fetch history <name> (fast-forward only)
  rebase <onto>                   replay this history's own nodes onto history <onto>
  publish --changelog <file> [--as <name>]
                                  add this history's tip to the release history
                                  (default releases) as one node whose changelog,
                                  the file's text, says what changed since the
                                  last release (no notes reach it;
                                  spec/release-history.md)
  merge [<other>]                 merge history <other> in (or the two tips) as one node
  import-git <repo> [<rev>]       import a git repository's commits (default HEAD)
                                  as history -H (refused if it exists); a commit's
                                  message becomes its step's notes, and the tip's
                                  files note per line the commit that last changed it
  gc [--full]                     one collection run (two-phase); without --full,
                                  walks and repacks only what came since the last
                                  full run while that is small
  serve <addr> --cert <file> --key <file> [--open] [--max-body <bytes>]
                                  serve the store over HTTPS (the only protocol)
                                  with the PEM certificate chain and key, under
                                  the access policy in --meta, or with --open to anyone;
                                  request bodies up to <bytes> (default 64 MiB)
  access show                     the policy in --meta (default .cas-meta)
  access user <name>              add or re-key <name>; prints its token once
  access grant <who> '<action> <scope>'  e.g. grant '*' 'read blobs'
  access revoke <who> '<action> <scope>'";

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
    changelog: Option<String>,
    compatible: bool,
    full: bool,
    notes: Option<String>,
    annotate: Vec<String>,
    show_diff: bool,
    annotated: bool,
    author: Option<String>,
    tag: Option<String>,
    change: Option<String>,
    commits: bool,
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
        changelog: None,
        compatible: false,
        full: false,
        notes: None,
        annotate: Vec::new(),
        show_diff: false,
        annotated: false,
        author: None,
        tag: None,
        change: None,
        commits: false,
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
            "-m" => {
                return Err("cas has no commit messages: a step says why through its notes \
                            (commit --notes <file>), a release through publish --changelog <file>"
                    .into());
            }
            "--changelog" => a.changelog = Some(value("--changelog")?),
            "--compatible" => a.compatible = true,
            "--full" => a.full = true,
            "--notes" => a.notes = Some(value("--notes")?),
            "--annotate" => a.annotate.push(value("--annotate")?),
            "--diff" => a.show_diff = true,
            "--annotated" => a.annotated = true,
            "--author" => a.author = Some(value("--author")?),
            "--tag" => a.tag = Some(value("--tag")?),
            "--change" => a.change = Some(value("--change")?),
            "--commits" => a.commits = true,
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

/// The one node whose version key starts with `prefix`, in a directory
/// format, found from one range of the history's tree. `None` when no
/// such node or several: the whole history then decides.
/// The node `what` names (a tag, `latest`, an id, a key prefix) from the
/// root and the parts that hold it; only an ambiguous name reads the whole
/// history.
fn find_node(st: &history::Stored, what: &str, name: &str) -> Res<NodeId> {
    if let Some(id) = st.root.tags.get(what) {
        return Ok(id.clone());
    }
    if what == "latest" && st.root.tips.len() == 1 {
        return Ok(st.root.tips.first().unwrap().clone());
    }
    if let Ok(id) = NodeId::parse(what)
        && st.contains(&id).map_err(e)?
    {
        return Ok(id);
    }
    if let Some(id) = by_prefix(st, what)? {
        return Ok(id);
    }
    node(&st.read_all().map_err(e)?, what, name)
}

/// Merge bases from an ancestry blob and the history's root alone; `None`
/// when a node is not found that way.
fn quick_merge_bases(
    s: &dyn Store,
    v: &View,
    ix: &cas_version::ancestry::Ancestry,
    a: &str,
    b: &str,
) -> Option<Vec<NodeId>> {
    let root = history::Root::decode(&cas_version::read_blob(s, v.key()).ok()?).ok()?;
    let find = |what: &str| -> Option<NodeId> {
        if let Some(id) = root.tags.get(what) {
            return Some(id.clone());
        }
        if what == "latest" && root.tips.len() == 1 {
            return root.tips.first().cloned();
        }
        if let Ok(id) = NodeId::parse(what)
            && ix.find(&id).is_some()
        {
            return Some(id);
        }
        let prefix = prefix_key(what)?;
        let mut hits = Vec::new();
        for f in formats::all().iter().filter(|f| matches!(f.kind, formats::Kind::Dir(_))) {
            hits.extend(ix.with_key_prefix(f.id(), &prefix, 2)?);
        }
        if hits.len() == 1 { hits.pop() } else { None }
    };
    let (x, y) = (find(a)?, find(b)?);
    let mut out = cas_version::ancestry::merge_bases_in(ix, &x, &y)?;
    out.sort();
    Some(out)
}

/// A node key prefix as the full form `alg:hex` (blake3 when no algorithm
/// is named), if `prefix` can be one.
fn prefix_key(prefix: &str) -> Option<String> {
    if prefix.is_empty() || !prefix.bytes().all(|b| b.is_ascii_hexdigit() || b == b':') {
        return None;
    }
    Some(if prefix.contains(':') { prefix.to_string() } else { format!("blake3:{prefix}") })
}

fn by_prefix(st: &history::Stored, prefix: &str) -> Res<Option<NodeId>> {
    let Some(prefix) = prefix_key(prefix) else { return Ok(None) };
    let mut hits = Vec::new();
    for f in formats::all().iter().filter(|f| matches!(f.kind, formats::Kind::Dir(_))) {
        hits.extend(st.with_key_prefix(f.id(), &prefix, 2).map_err(e)?);
    }
    Ok(if hits.len() == 1 { hits.pop() } else { None })
}

fn load(s: &dyn Store, name: &str) -> Res<(Option<View>, History)> {
    history::load(s, name).map_err(e)
}

fn node(h: &History, what: &str, name: &str) -> Res<NodeId> {
    h.resolve(what).ok_or(format!("no node {what} in history {name}"))
}

fn node_text(n: &Option<Node>) -> &'static str {
    match n {
        None => "-",
        Some(Node::Dir) => "dir",
        Some(Node::File(r)) => r.known_format().map_or("?", |f| f.alias),
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
    if commits::handles(&a, s)? {
        return commits::run(&a, s);
    }
    let arg = |i: usize, what: &str| a.rest.get(i).cloned().ok_or(format!("missing {what}\n{USAGE}"));
    let prefix = |i: usize| a.rest.get(i).map(|p| Path::new(p.as_str()).map_err(e)).transpose();
    let name = a.history.as_str();
    let step = Step { changelog: None, compatible: a.compatible, notes: None };
    // The node -n names: a tag, `latest` or a node id is found from the
    // history's root and one leaf; only a key prefix needs every node.
    let at = || -> Res<NodeId> {
        let Some(v) = history::head(s, name).map_err(e)? else { return Err(format!("no history {name}")) };
        let st = history::Stored::open(s, Some(&v)).map_err(e)?;
        find_node(&st, &a.node, name)
    };
    match a.rest.first().map(String::as_str) {
        Some("commit") => {
            let dir = arg(1, "<dir>")?;
            // The stat cache in DIR spares rehashing unchanged files; a sparse
            // checkout keeps the rest of the history's head.
            let head = match history::head(s, name).map_err(e)? {
                Some(v) => {
                    let tips = history::Stored::open(s, Some(&v)).map_err(e)?.root.tips;
                    if tips.len() == 1 { tips.first().map(NodeId::version) } else { None }
                }
                None => None,
            };
            let root = fs::import_worktree(
                s,
                b3(),
                formats::by_alias("tree").unwrap(),
                dir.as_ref(),
                Some(&a.store),
                head.as_ref(),
            )
            .map_err(e)?;
            formats::publish(s).map_err(e)?;
            let mut step = step;
            let mut first_notes = BTreeMap::new();
            if let Some(file) = &a.notes {
                // The notes are written on the diff from the tip to this
                // version, which the file's text must be exactly; they are
                // kept on the files' lines, the diff is not stored.
                let form = std::fs::read(file).map_err(|x| format!("{file}: {x}"))?;
                let sn = stepnotes::from_form(s, head.as_ref(), &root, &form).map_err(|x| format!("{file}: {x}"))?;
                if head.is_none() {
                    // A first node has no step: its notes are on its files.
                    if let Some(sn) = &sn {
                        if !sn.note.is_empty() || sn.paths.values().any(|p| !p.note.is_empty()) {
                            return Err(format!(
                                "{file}: a first node has no step, so notes go on its files' lines, \
                                 not on the diff or a path line"
                            ));
                        }
                        first_notes = stepnotes::file_notes(s, b3(), sn).map_err(e)?;
                    }
                } else {
                    step.notes = sn.map(|sn| stepnotes::write(s, b3(), &sn)).transpose().map_err(e)?;
                }
            }
            let mut notes = first_notes;
            for spec in &a.annotate {
                let (p, file) = spec.rsplit_once('=').ok_or(format!("--annotate {spec}: <path>=<file>"))?;
                let path = Path::new(p).map_err(e)?;
                let form = std::fs::read(file).map_err(|x| format!("{file}: {x}"))?;
                let (text, lines) = annotated::parse(&form).map_err(|x| format!("{file}: {x}"))?;
                let Some(target) = cas_version::object_at(s, &root, &path).map_err(e)?.filter(|v| !v.is_dir_view())
                else {
                    return Err(format!("--annotate {p}: no such file in {dir}"));
                };
                if text != read_blob(s, target.key()).map_err(e)? {
                    return Err(format!("{file}: its text is not {p} as committed"));
                }
                notes.insert(p.to_string(), annotated::write(s, b3(), &target, &lines).map_err(e)?);
            }
            let id = history::commit_with_notes(s, b3(), name, &root, step, None, notes).map_err(e)?;
            out!("{}", id);
        }
        Some(cmd @ ("status" | "reset" | "clean")) => {
            let dir = arg(1, "<dir>")?;
            let dirf = formats::by_alias("tree").unwrap();
            if cmd == "status" && a.show_diff {
                // Before the first commit, the diff from nothing.
                let v = match history::head(s, name).map_err(e)? {
                    Some(_) => Some(at()?.version()),
                    None => None,
                };
                let root = fs::import_worktree(s, b3(), dirf, dir.as_ref(), Some(&a.store), v.as_ref()).map_err(e)?;
                formats::publish(s).map_err(e)?;
                let d = annotated::diff(s, v.as_ref(), &root).map_err(e)?;
                std::io::Write::write_all(&mut std::io::stdout(), &annotated::render(&d, &[])).map_err(e)?;
                return Ok(());
            }
            let v = at()?.version();
            if cmd == "status" {
                for c in fs::status(s, b3(), dirf, dir.as_ref(), Some(&a.store), &v).map_err(e)? {
                    let mark = match (&c.old, &c.new) {
                        (None, _) => "added",
                        (_, None) => "deleted",
                        _ => "changed",
                    };
                    out!("{mark:8} {}", c.path);
                }
            } else {
                let n = fs::reset(s, b3(), dirf, dir.as_ref(), Some(&a.store), &v, cmd == "clean").map_err(e)?;
                out!("{} paths {}", n.len(), if cmd == "clean" { "removed" } else { "restored" });
            }
        }
        Some("stash") => {
            let pop = a.rest.get(1).is_some_and(|x| x == "pop");
            let dir = arg(if pop { 2 } else { 1 }, "<dir>")?;
            let dirf = formats::by_alias("tree").unwrap();
            let stash = format!("stash/{name}");
            if pop {
                let (old, mut h) = load(s, &stash)?;
                let tip = h.tip().ok_or(format!("nothing stashed in {stash}"))?;
                if !fs::status(s, b3(), dirf, dir.as_ref(), Some(&a.store), &at()?.version()).map_err(e)?.is_empty() {
                    return Err(format!("{dir} has changes; commit, stash or reset them first"));
                }
                fs::reset(s, b3(), dirf, dir.as_ref(), Some(&a.store), &tip.version(), false).map_err(e)?;
                h.nodes.remove(&tip);
                h.tags.retain(|_, n| *n != tip);
                history::save(s, b3(), &stash, old.as_ref(), &h).map_err(e)?;
                out!("{tip}");
            } else {
                let base = at()?.version();
                let root = fs::import_worktree(s, b3(), dirf, dir.as_ref(), Some(&a.store), Some(&base)).map_err(e)?;
                formats::publish(s).map_err(e)?;
                let id = history::commit(s, b3(), &stash, &root, step.clone(), None).map_err(e)?;
                fs::reset(s, b3(), dirf, dir.as_ref(), Some(&a.store), &base, false).map_err(e)?;
                out!("{id}");
            }
        }
        Some("log") => {
            // From the graph when one is cached (path-log builds it), which
            // spares parsing the whole history.
            let Some(v) = history::head(s, name).map_err(e)? else { return Ok(()) };
            let heads = derived::heads(s, &v).map_err(e)?;
            let g = graph::graph_or_order(s, &v).map_err(e)?;
            let mut tags: BTreeMap<&NodeId, Vec<&str>> = BTreeMap::new();
            for (t, n) in &heads.tags {
                tags.entry(n).or_default().push(t);
            }
            let mut stored = None;
            for r in &g.nodes {
                let after: Vec<String> = r
                    .after
                    .iter()
                    .map(|&(p, c)| format!("{}{}", short(g.nodes[p].id.version().key()), if c { "~" } else { "" }))
                    .collect();
                let t = tags.get(&r.id).map(|t| format!(" ({})", t.join(", "))).unwrap_or_default();
                // A first node has no step: what it says is its doc.
                let doc = if r.after.is_empty() && r.notes.is_empty() {
                    let st = stored.get_or_insert(history::Stored::open(s, Some(&v)).map_err(e)?);
                    st.get(&r.id).map_err(e)?.and_then(|n| n.doc).unwrap_or_default()
                } else {
                    String::new()
                };
                out!("{}{t}  [{}]  {}{doc}", short(r.id.version().key()), after.join(" "), r.notes);
            }
        }
        Some("ls") => {
            for item in walk(s, &at()?.version(), prefix(1)?.as_ref()) {
                let (p, n) = item.map_err(e)?;
                out!("{:8} {p}", node_text(&Some(n)));
            }
        }
        Some("diff") => {
            let v = history::head(s, name).map_err(e)?.ok_or(format!("no history {name}"))?;
            let st = history::Stored::open(s, Some(&v)).map_err(e)?;
            let id = find_node(&st, &a.node, name)?;
            let preds: Vec<NodeId> = st.get(&id).map_err(e)?.map(|n| n.after.into_keys().collect()).unwrap_or_default();
            if a.annotated {
                let node = st.get(&id).map_err(e)?.unwrap_or_default();
                let noted = node.after.iter().find(|(_, st)| st.notes.is_some());
                let form = match noted.or(node.after.iter().next()) {
                    Some((_, step)) if let Some(n) = &step.notes => {
                        if n.known_format().is_some_and(|f| f.alias == "step-notes") {
                            stepnotes::show(s, n.key()).map_err(e)?
                        } else {
                            annotated::show(s, n.key()).map_err(e)?
                        }
                    }
                    Some((p, _)) => {
                        annotated::render(&annotated::diff(s, Some(&p.version()), &id.version()).map_err(e)?, &[])
                    }
                    None => annotated::render(&annotated::diff(s, None, &id.version()).map_err(e)?, &[]),
                };
                std::io::Write::write_all(&mut std::io::stdout(), &form).map_err(e)?;
                return Ok(());
            }
            let Some(pred) = preds.first() else { return Err("the node has no predecessor".into()) };
            for c in diff(s, &pred.version(), &id.version(), None).map_err(e)? {
                out!("{:8} -> {:8} {}", node_text(&c.old), node_text(&c.new), c.path);
            }
        }
        Some("notes") => {
            let Some(v) = history::head(s, name).map_err(e)? else { return Err(format!("no history {name}")) };
            let st = history::Stored::open(s, Some(&v)).map_err(e)?;
            let node = st.get(&at()?).map_err(e)?.unwrap_or_default();
            match a.rest.get(1) {
                None => {
                    for (p, n) in &node.notes {
                        out!("{p}  {n}");
                    }
                }
                Some(p) => {
                    let n = node.notes.get(p.as_str()).ok_or(format!("no notes on {p} in this node"))?;
                    let form = annotated::show(s, n.key()).map_err(e)?;
                    std::io::Write::write_all(&mut std::io::stdout(), &form).map_err(e)?;
                }
            }
        }
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
            _ => return Err(format!("annotated make <file> | annotated show <key>\n{USAGE}")),
        },
        Some("checkout") => {
            let dest = arg(1, "<dest>")?;
            let n = fs::checkout_indexed(s, &at()?.version(), prefix(2)?.as_ref(), dest.as_ref(), Some(&a.store))
                .map_err(e)?;
            out!("{n} paths written to {dest}");
        }
        Some("sparse") => {
            let dest = arg(1, "<dest>")?;
            let paths = a.rest[2..].iter().map(Path::new).collect::<Result<Vec<_>, _>>().map_err(e)?;
            if paths.is_empty() {
                return Err("sparse needs at least one <path>".into());
            }
            let n = fs::checkout_sparse(s, &at()?.version(), &paths, dest.as_ref(), Some(&a.store)).map_err(e)?;
            out!("{n} paths written to {dest}");
        }
        Some("summary") => {
            let r = derived::derive(s, b3(), &SUMMARY, &at()?.version()).map_err(e)?;
            let v = derived::value(s, &r).map_err(e)?;
            out!("{v}");
        }
        Some("path-log") => {
            let p = Path::new(arg(1, "<path>")?).map_err(e)?;
            let Some(v) = history::head(s, name).map_err(e)? else { return Ok(()) };
            // A cached graph skips the steps its filters rule out; without
            // one, walking every step is faster than building the graph.
            let g = graph::graph_or_order(s, &v).map_err(e)?;
            for i in g.path_log(s, &p).map_err(e)? {
                out!("{}  {}", short(g.nodes[i].id.version().key()), g.nodes[i].notes);
            }
        }
        Some("tag") => {
            let t = arg(1, "<name>")?;
            let id = at()?;
            history::edit(s, b3(), name, |_, root| {
                root.tags.insert(t.clone(), id.clone());
                Ok((BTreeMap::new(), ()))
            })
            .map_err(e)?;
        }
        Some("merge-base") => {
            // Reads only the nodes the walk reaches (by generation), unless
            // a node is named by a key prefix.
            let v = history::head(s, name).map_err(e)?.ok_or(format!("no history {name}"))?;
            // With the ancestry of this very history, only its root is read.
            if let Some(ix) = cas_version::ancestry::exact(s, &v)
                && let Some(bases) = quick_merge_bases(s, &v, &ix, &arg(1, "<a>")?, &arg(2, "<b>")?)
            {
                for b in bases {
                    out!("{b}");
                }
                return Ok(());
            }
            let st = history::Stored::open(s, Some(&v)).map_err(e)?;
            let (x, y) = (find_node(&st, &arg(1, "<a>")?, name)?, find_node(&st, &arg(2, "<b>")?, name)?);
            // From the ancestry blob gc and import keep, when there is one.
            let index = cas_version::ancestry::cached(s, &st, &v, 64);
            for b in cas_version::ancestry::merge_bases(&st, index.as_ref(), &x, &y).map_err(e)? {
                out!("{b}");
            }
        }
        Some("last-modified") => {
            let dir = prefix(1)?;
            let Some(v) = history::head(s, name).map_err(e)? else { return Ok(()) };
            let g = graph::graph_near(s, b3(), &v).map_err(e)?;
            let tip = at()?.version();
            let entries: Vec<Path> = walk(s, &tip, dir.as_ref())
                .filter_map(|item| match item {
                    Ok((p, _)) if p.parent() == dir => Some(Ok(p)),
                    Ok(_) => None,
                    Err(x) => Some(Err(x)),
                })
                .collect::<Result<_, _>>()
                .map_err(e)?;
            for (p, last) in entries.iter().zip(g.last_changes(s, &entries).map_err(e)?) {
                match last {
                    Some(i) => out!("{}  {p}", short(g.nodes[i].id.version().key())),
                    None => out!("-  {p}"),
                }
            }
        }
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
        Some("compatible") => {
            let h = load(s, name)?.1;
            let (x, y) = (node(&h, &arg(1, "<from>")?, name)?, node(&h, &arg(2, "<to>")?, name)?);
            out!("{}", if h.compatible(&x, &y) { "compatible" } else { "breaking" });
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
            out!("{outcome:?}: {} blobs copied, {} already present", n.blobs, n.present);
        }
        Some("rebase") => {
            let onto = arg(1, "<onto>")?;
            let target = load(s, &onto)?.1;
            let (old, mine) = load(s, name)?;
            let (h, outcome) = merge::rebase(s, b3(), &mine, &target).map_err(e)?;
            if matches!(outcome, MergeOutcome::Done(_)) {
                history::save(s, b3(), name, old.as_ref(), &h).map_err(e)?;
            }
            report(outcome)?;
        }
        Some("publish") => {
            if let Some(x) = a.rest.get(1) {
                return Err(format!("publish takes no {x:?}: the release history is `releases`, or --as <name>"));
            }
            let public = a.remote.clone().unwrap_or_else(|| "releases".into());
            // A release says what is different from the release before it.
            let file = a.changelog.as_ref().ok_or("publish needs --changelog <file>: what this release changes")?;
            let text = std::fs::read_to_string(file).map_err(|x| format!("{file}: {x}"))?;
            let step = Step { changelog: Some(text.trim_end().to_string()), ..step.clone() };
            let mine = load(s, name)?.1;
            let (_, outcome) =
                history::record(s, b3(), &public, |p| merge::publish(&mine, p, step.clone(), None)).map_err(e)?;
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
            let repo = arg(1, "<repo>")?;
            let rev = a.rest.get(2).cloned().unwrap_or_else(|| "HEAD".into());
            let start = std::time::Instant::now();
            let (h, n, filters) =
                cas_version::git::import_with_filters(s, b3(), repo.as_ref(), &rev, true).map_err(e)?;
            formats::publish(s).map_err(e)?;
            let v = history::save(s, b3(), name, None, &h).map_err(|x| format!("{name}: {x}"))?;
            cas_version::ancestry::record(s, b3(), &v, &h);
            // The graph too (git's commit-graph with changed-path filters):
            // path-log and last-modified skip the steps it rules out.
            graph::record_with(s, b3(), &v, &h, filters).map_err(e)?;
            // Its deltas are a repack's (each file against its path's last
            // version): one sealed pack, so a gc reuses them.
            if !a.store.starts_with("http://")
                && !a.store.starts_with("https://")
                && let Ok(disk) = DiskBackend::open(&a.store)
            {
                disk.seal_packs().map_err(e)?;
            }
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
                    let (order, hints) = gc::delta_hints(s, &|k| disk.sealed(k).unwrap_or(false)).map_err(e)?;
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
                _ => return Err(USAGE.into()),
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
        _ => return Err(format!("{USAGE}\n{}", forge::USAGE)),
    }
    Ok(())
}

fn short(key: &str) -> String {
    key.split_once(':').map_or(key, |(_, d)| &d[..12.min(d.len())]).to_string()
}

fn main() -> ExitCode {
    let r = parse(std::env::args().skip(1).collect()).and_then(run);
    flush_out();
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("{msg}");
            ExitCode::FAILURE
        }
    }
}
