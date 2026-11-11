//! Commands on a history of commits (spec/formats/commit.md): the history
//! variable holds a view of its newest commit, versions are `directory`
//! trees, and notes are `annotation` trees.

use crate::{Args, Res, b3, e, flush_out, node_text, short};
use cas_core::Store;
use cas_core::disk::DiskBackend;
use cas_version::commits::{self, Commit, Notes};
use cas_version::releases;
use cas_version::stepnotes::{self, StepNotes};
use cas_version::{Node, Path, View, annotated, diff, formats, fs, history, object_at, read_blob, walk};
use cas_version::{derived, proofs};
use std::collections::{BTreeMap, HashMap};

/// Every command this module knows, for the help table's check.
#[cfg(test)]
pub const COMMANDS: [&[&str]; 3] = [PORTED, RELEASE, NO_HISTORY];

/// The commands that work on a history of commits.
const PORTED: &[&str] = &[
    "commit",
    "status",
    "reset",
    "clean",
    "log",
    "ls",
    "diff",
    "checkout",
    "sparse",
    "import-git",
    "merge",
    "rebase",
    "merge-base",
    "path-log",
    "publish",
    "stash",
    "last-modified",
    "summary",
    "notes",
    "tag",
    "manifest",
    "prove",
    "verify",
];

/// The commands that work on a release history.
const RELEASE: &[&str] = &["log", "ls", "checkout", "sparse", "compatible"];

/// The commands that are not about one history.
const NO_HISTORY: &[&str] = &["gc", "fsck", "objects", "show", "serve", "access", "annotated", "push", "pull", "watch"];

/// Whether the command is for this module: the history holds a commit, or
/// it is unset (and `--nodes` does not ask for the earlier format).
pub fn handles(a: &Args, s: &dyn Store) -> Res<bool> {
    let Some(cmd) = a.rest.first().map(String::as_str) else { return Ok(false) };
    if NO_HISTORY.contains(&cmd) {
        return Ok(false);
    }
    let name = a.history.as_str();
    let ours = match history::head(s, name).map_err(e)? {
        Some(v) if releases::is_release(&v) => {
            if !RELEASE.contains(&cmd) {
                return Err(format!("{cmd} is not for a release history ({name}): {}", RELEASE.join(", ")));
            }
            true
        }
        Some(v) => commits::is_commit(&v),
        // A new history is one of commits, unless --nodes asks for the
        // earlier format (which the forge still keeps).
        None => !a.nodes,
    };
    if a.nodes && ours {
        return Err(format!("history {name} is a history of commits; --nodes is for the earlier format"));
    }
    if !ours {
        return Ok(false);
    }
    if !PORTED.contains(&cmd) && !RELEASE.contains(&cmd) {
        return Err(format!("{cmd} is not yet available for a history of commits ({name})"));
    }
    Ok(true)
}

/// A time as `YYYY-MM-DD HH:MM` (UTC).
fn date(t: u64) -> String {
    let (days, secs) = ((t / 86400) as i64, t % 86400);
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

fn author(a: &Args) -> Res<String> {
    let found = a.author.clone().or_else(|| std::env::var("CAS_AUTHOR").ok()).or_else(|| std::env::var("USER").ok());
    found.filter(|x| !x.trim().is_empty()).ok_or("a commit needs an author: --author <name> (or $CAS_AUTHOR)".into())
}

pub fn run(a: &Args, s: &dyn Store) -> Res<()> {
    let name = a.history.as_str();
    let dirf = formats::by_alias("directory").unwrap();
    let arg = |i: usize, what: &str| {
        a.rest.get(i).cloned().ok_or_else(|| format!("missing {what}\n{}", crate::help::usage(&a.rest[0])))
    };
    let prefix = |i: usize| a.rest.get(i).map(|p| Path::new(p.as_str()).map_err(e)).transpose();
    if history::head(s, name).map_err(e)?.is_some_and(|v| releases::is_release(&v)) {
        return run_release(a, s);
    }
    let tip = commits::tip(s, name).map_err(e)?;
    // The commit -n names: `latest`, a key, or the start of one.
    let at = || -> Res<(String, Commit)> {
        let tip = tip.clone().ok_or(format!("no history {name}"))?;
        let k =
            commits::resolve(s, &tip, &a.node).map_err(e)?.ok_or(format!("no commit {} in history {name}", a.node))?;
        let c = commits::read(s, &k).map_err(e)?;
        Ok((k, c))
    };
    match a.rest[0].as_str() {
        "commit" => {
            let dir = arg(1, "<dir>")?;
            let head = match &tip {
                Some(t) => Some(commits::read(s, t).map_err(e)?.version()),
                None => None,
            };
            let root = fs::import_worktree(s, b3(), dirf, dir.as_ref(), Some(&a.store), head.as_ref()).map_err(e)?;
            formats::publish(s).map_err(e)?;
            let mut n = Notes::default();
            if let Some(file) = &a.notes {
                // Written on the diff from the tip to this version, which
                // the file's text must be exactly.
                let form = std::fs::read(file).map_err(|x| format!("{file}: {x}"))?;
                let sn = stepnotes::from_form(s, head.as_ref(), &root, &form).map_err(|x| format!("{file}: {x}"))?;
                if let Some(sn) = sn {
                    n = Notes { note: sn.note, paths: sn.paths };
                    commits::fits(s, &n, head.as_ref(), &root).map_err(|x| format!("{file}: {x}"))?;
                }
            }
            // Notes on a file as it is in this version: one for each line.
            for spec in &a.annotate {
                let (p, file) = spec.rsplit_once('=').ok_or(format!("--annotate {spec}: <path>=<file>"))?;
                let path = Path::new(p).map_err(e)?;
                let form = std::fs::read(file).map_err(|x| format!("{file}: {x}"))?;
                let (text, lines) = annotated::parse(&form).map_err(|x| format!("{file}: {x}"))?;
                let Some(target) = object_at(s, &root, &path).map_err(e)?.filter(|v| !v.is_dir_view()) else {
                    return Err(format!("--annotate {p}: no such file in {dir}"));
                };
                if text != read_blob(s, target.key()).map_err(e)? {
                    return Err(format!("{file}: its text is not {p} as committed"));
                }
                n.paths.entry(path.to_string()).or_default().new = stepnotes::runs_of(&lines);
            }
            let mut notes = None;
            if !n.is_empty() {
                commits::fits(s, &n, head.as_ref(), &root).map_err(e)?;
                let is_dir = |p: &str| -> cas_version::Result<bool> {
                    let path = Path::new(p)?;
                    for v in [Some(&root), head.as_ref()].into_iter().flatten() {
                        if let Some(o) = object_at(s, v, &path)? {
                            return Ok(o.is_dir_view());
                        }
                    }
                    Ok(false)
                };
                notes = commits::write_notes(s, b3(), &n, &is_dir).map_err(e)?;
            }
            let author = commits::put_text(s, b3(), "author", &commits::one_line(&author(a)?)).map_err(e)?;
            let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(e)?.as_secs();
            // A first commit has no step: its notes are on its version.
            let c = match &tip {
                Some(t) => Commit {
                    version: root.key().to_string(),
                    author,
                    notes: None,
                    time,
                    parents: BTreeMap::from([(t.clone(), notes)]),
                },
                None => Commit { version: root.key().to_string(), author, notes, time, parents: BTreeMap::new() },
            };
            let key = commits::write(s, b3(), &c).map_err(e)?;
            commits::advance(s, name, tip.as_deref(), &key).map_err(|x| format!("{name}: {x}"))?;
            out!("{key}");
        }
        cmd @ ("status" | "reset" | "clean") => {
            let dir = arg(1, "<dir>")?;
            if cmd == "status" && a.show_diff {
                // Before the first commit, the diff from nothing.
                let v = match &tip {
                    Some(_) => Some(at()?.1.version()),
                    None => None,
                };
                let root = fs::import_worktree(s, b3(), dirf, dir.as_ref(), Some(&a.store), v.as_ref()).map_err(e)?;
                formats::publish(s).map_err(e)?;
                let d = annotated::diff(s, v.as_ref(), &root).map_err(e)?;
                std::io::Write::write_all(&mut std::io::stdout(), &annotated::render(&d, &[])).map_err(e)?;
                return Ok(());
            }
            let v = at()?.1.version();
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
        "stash" => {
            // Kept as commits of their own in history stash/<history>:
            // each stash after the one before it.
            let pop = a.rest.get(1).is_some_and(|x| x == "pop");
            let dir = arg(if pop { 2 } else { 1 }, "<dir>")?;
            let stash = format!("stash/{name}");
            let kept = commits::tip(s, &stash).map_err(e)?;
            let base = at()?.1.version();
            if pop {
                let top = kept.ok_or(format!("nothing stashed in {stash}"))?;
                if !fs::status(s, b3(), dirf, dir.as_ref(), Some(&a.store), &base).map_err(e)?.is_empty() {
                    return Err(format!("{dir} has changes; commit, stash or reset them first"));
                }
                let c = commits::read(s, &top).map_err(e)?;
                fs::reset(s, b3(), dirf, dir.as_ref(), Some(&a.store), &c.version(), false).map_err(e)?;
                match c.parents.keys().next() {
                    Some(below) => commits::advance(s, &stash, Some(&top), below),
                    None => commits::drop(s, &stash, &top),
                }
                .map_err(|x| format!("{stash}: {x}"))?;
                out!("{top}");
            } else {
                let root = fs::import_worktree(s, b3(), dirf, dir.as_ref(), Some(&a.store), Some(&base)).map_err(e)?;
                formats::publish(s).map_err(e)?;
                let author = commits::put_text(s, b3(), "author", &commits::one_line(&author(a)?)).map_err(e)?;
                let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(e)?.as_secs();
                let parents = kept.iter().map(|k| (k.clone(), None)).collect();
                let c = Commit { version: root.key().to_string(), author, notes: None, time, parents };
                let key = commits::write(s, b3(), &c).map_err(e)?;
                commits::advance(s, &stash, kept.as_deref(), &key).map_err(|x| format!("{stash}: {x}"))?;
                fs::reset(s, b3(), dirf, dir.as_ref(), Some(&a.store), &base, false).map_err(e)?;
                out!("{key}");
            }
        }
        "last-modified" => {
            let dir = prefix(1)?;
            let (k, c) = at()?;
            let mut entries = Vec::new();
            for item in walk(s, &c.version(), dir.as_ref()) {
                let (p, _) = item.map_err(e)?;
                if p.parent() == dir {
                    entries.push(p);
                }
            }
            for (p, last) in entries.iter().zip(commits::last_changes(s, &k, &entries).map_err(e)?) {
                match last {
                    Some(k) => out!("{}  {p}", short(&k)),
                    None => out!("-  {p}"),
                }
            }
        }
        "summary" => {
            let (mut dirs, mut files) = (0u64, 0u64);
            for item in walk(s, &at()?.1.version(), None) {
                match item.map_err(e)?.1 {
                    Node::Dir => dirs += 1,
                    Node::File(_) => files += 1,
                }
            }
            out!("{dirs} directories, {files} files");
        }
        "notes" => {
            let (_, c) = at()?;
            let (_, n) = step_notes(s, &c)?;
            match a.rest.get(1) {
                None => {
                    for (p, pn) in &n.paths {
                        out!("{p}  {}", pn.note.lines().next().unwrap_or(""));
                    }
                }
                Some(p) => {
                    let path = Path::new(p.as_str()).map_err(e)?;
                    let pn = n.paths.get(&path.to_string()).ok_or(format!("no notes on {p} in this commit"))?;
                    if !pn.note.is_empty() {
                        out!("{}", pn.note);
                    }
                    if let Some(runs) = &pn.new {
                        let file = object_at(s, &c.version(), &path).map_err(e)?.ok_or(format!("no file {p}"))?;
                        let text = read_blob(s, file.key()).map_err(e)?;
                        flush_out();
                        let form = annotated::render(&text, &stepnotes::expand(runs));
                        std::io::Write::write_all(&mut std::io::stdout(), &form).map_err(e)?;
                    }
                }
            }
        }
        cmd @ ("prove" | "verify") => {
            // The prover is a program of its own (prove/): it builds Flock.
            if a.store.contains("://") {
                return Err(format!("{cmd} works on a store directory: run it where the store is"));
            }
            let (k, _) = at()?;
            // Oldest first, so that what is proven is always a history's start.
            let mut commits: Vec<String> =
                if a.all { commits::log(s, &k).map_err(e)?.into_iter().map(|(k, _)| k).collect() } else { vec![k] };
            commits.reverse();
            let mut manifests = Vec::new();
            let mut blobs = 0;
            for k in &commits {
                let view = derived::derive(s, b3(), &proofs::MANIFEST, &commits::view("commit", k)).map_err(e)?;
                let proven = derived::cached(s, &proofs::PROOF, &view).map_err(e)?.is_some();
                if cmd == "verify" && !proven {
                    return Err(format!("commit {} has no proof: cas prove makes one", short(k)));
                }
                if cmd == "verify" || !proven {
                    blobs += proofs::read(s, view.key()).map_err(e)?.blobs.len();
                    manifests.push(view.key().to_string());
                }
            }
            if manifests.is_empty() {
                out!("{} commits, all proven already", commits.len());
                return Ok(());
            }
            flush_out();
            let program = prover()?;
            for part in manifests.chunks(256) {
                let status = std::process::Command::new(&program)
                    .arg(cmd)
                    .arg("--store")
                    .arg(&a.store)
                    .args(part)
                    .stdout(if a.quiet { std::process::Stdio::null() } else { std::process::Stdio::inherit() })
                    .status()
                    .map_err(|x| format!("{}: {x}", program.display()))?;
                if !status.success() {
                    return Err(format!(
                        "{cmd} failed{}",
                        if a.quiet { "; run it without --quiet to see where" } else { "" }
                    ));
                }
            }
            let done = if cmd == "prove" { "proven" } else { "verified" };
            out!("{} commits {done}: {blobs} blobs valid", manifests.len());
        }
        "manifest" => {
            // Kept as derived data, so it is worked out once for a commit.
            let (k, _) = at()?;
            let view = derived::derive(s, b3(), &proofs::MANIFEST, &commits::view("commit", &k)).map_err(e)?;
            let m = proofs::read(s, view.key()).map_err(e)?;
            for (key, (alias, len)) in &m.blobs {
                out!("{alias:10} {} {len}", cas_version::typed::key_text(key));
            }
            out!("{} blobs, {} bytes, manifest {}", m.blobs.len(), m.bytes(), view.key());
        }
        "tag" => {
            return Err("a history of commits has no tags: publish --tag <version> names a release".into());
        }
        "log" => {
            let Some(tip) = &tip else { return Ok(()) };
            let mut authors: HashMap<String, String> = HashMap::new();
            for (k, c) in commits::log(s, tip).map_err(e)? {
                if !authors.contains_key(&c.author) {
                    authors.insert(c.author.clone(), commits::text(s, &c.author).map_err(e)?);
                }
                let parents: Vec<String> = c.parents.keys().map(|p| short(p)).collect();
                let line = commits::line(s, &c).map_err(e)?;
                out!("{}  [{}]  {}  {}  {}", short(&k), parents.join(" "), date(c.time), authors[&c.author], line);
            }
        }
        "ls" => {
            for item in walk(s, &at()?.1.version(), prefix(1)?.as_ref()) {
                let (p, n) = item.map_err(e)?;
                out!("{:8} {p}", node_text(&Some(n)));
            }
        }
        "diff" => {
            let (_, c) = at()?;
            let after = c.version();
            if a.annotated {
                // The step with notes, else any step; a first commit's
                // notes are on its version.
                let (before, n) = step_notes(s, &c)?;
                commits::fits(s, &n, before.as_ref(), &after).map_err(e)?;
                let d = annotated::diff(s, before.as_ref(), &after).map_err(e)?;
                let sn = StepNotes { before, after, note: n.note, paths: n.paths };
                let form = annotated::render(&d, &stepnotes::line_notes(&sn, &d));
                std::io::Write::write_all(&mut std::io::stdout(), &form).map_err(e)?;
                return Ok(());
            }
            if c.parents.is_empty() {
                return Err("the commit has no parent".into());
            }
            for p in c.parents.keys() {
                if c.parents.len() > 1 {
                    out!("from {}", short(p));
                }
                let before = commits::read(s, p).map_err(e)?.version();
                for ch in diff(s, &before, &after, None).map_err(e)? {
                    out!("{:8} -> {:8} {}", node_text(&ch.old), node_text(&ch.new), ch.path);
                }
            }
        }
        "checkout" => {
            let dest = arg(1, "<dest>")?;
            let n = fs::checkout_indexed(s, &at()?.1.version(), prefix(2)?.as_ref(), dest.as_ref(), Some(&a.store))
                .map_err(e)?;
            out!("{n} paths written to {dest}");
        }
        "sparse" => {
            let dest = arg(1, "<dest>")?;
            let paths = a.rest[2..].iter().map(Path::new).collect::<Result<Vec<_>, _>>().map_err(e)?;
            if paths.is_empty() {
                return Err("sparse needs at least one <path>".into());
            }
            let n = fs::checkout_sparse(s, &at()?.1.version(), &paths, dest.as_ref(), Some(&a.store)).map_err(e)?;
            out!("{n} paths written to {dest}");
        }
        cmd @ ("merge" | "rebase") => {
            let other = arg(1, if cmd == "merge" { "<other>" } else { "<onto>" })?;
            let ours = tip.clone().ok_or(format!("no history {name}"))?;
            let theirs = commits::tip(s, &other).map_err(e)?.ok_or(format!("no history {other}"))?;
            let outcome = if cmd == "merge" {
                let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(e)?.as_secs();
                commits::merge(s, b3(), &ours, &theirs, &author(a)?, time)
            } else {
                commits::rebase(s, b3(), &ours, &theirs)
            }
            .map_err(e)?;
            match outcome {
                commits::Outcome::UpToDate => out!("UpToDate"),
                commits::Outcome::FastForward(k) | commits::Outcome::Done(k) => {
                    commits::advance(s, name, Some(&ours), &k).map_err(|x| format!("{name}: {x}"))?;
                    out!("Done {}", short(&k));
                }
                commits::Outcome::Conflicts(m) => {
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
        }
        "merge-base" => {
            let tip = tip.clone().ok_or(format!("no history {name}"))?;
            let find = |what: &str| -> Res<String> {
                commits::resolve(s, &tip, what).map_err(e)?.ok_or(format!("no commit {what} in history {name}"))
            };
            let (x, y) = (find(&arg(1, "<a>")?)?, find(&arg(2, "<b>")?)?);
            if let Some(k) = commits::merge_base(s, &x, &y).map_err(e)? {
                out!("{}", short(&k));
            }
        }
        "path-log" => {
            let p = Path::new(arg(1, "<path>")?).map_err(e)?;
            let Some(tip) = &tip else { return Ok(()) };
            for (k, c) in commits::path_log(s, tip, &p).map_err(e)? {
                let line = commits::line(s, &c).map_err(e)?;
                out!("{}  {line}", short(&k));
            }
        }
        "publish" => {
            if let Some(x) = a.rest.get(1) {
                return Err(format!("publish takes no {x:?}: the release history is `releases`, or --as <name>"));
            }
            let public = a.remote.clone().unwrap_or_else(|| "releases".into());
            let tag = a.tag.as_ref().ok_or("publish needs --tag <version>, a semantic version such as 1.4.0")?;
            let changelog = match &a.changelog {
                Some(file) => Some(std::fs::read_to_string(file).map_err(|x| format!("{file}: {x}"))?),
                None => None,
            };
            let edge = match (&a.change, &changelog) {
                (Some(c), Some(l)) => Some((c.as_str(), l.as_str())),
                (None, None) => None,
                _ => return Err("publish takes --change <kind> and --changelog <file> together".into()),
            };
            let key = releases::publish(s, b3(), &public, &at()?.1.version(), tag, edge).map_err(e)?;
            out!("{tag} {}", short(&key));
        }
        "compatible" => {
            return Err("compatible is for a release history: -H releases compatible <from> <to>".into());
        }
        "import-git" => {
            if tip.is_some() {
                return Err(format!("history {name} exists"));
            }
            let repo = arg(1, "<repo>")?;
            let rev = a.rest.get(2).cloned().unwrap_or_else(|| "HEAD".into());
            let start = std::time::Instant::now();
            let (key, n) = commits::import_git(s, b3(), repo.as_ref(), &rev).map_err(e)?;
            formats::publish(s).map_err(e)?;
            commits::advance(s, name, None, &key).map_err(|x| format!("{name}: {x}"))?;
            if !a.store.starts_with("http://")
                && !a.store.starts_with("https://")
                && let Ok(disk) = DiskBackend::open(&a.store)
            {
                disk.seal_packs().map_err(e)?;
            }
            out!(
                "{} commits, {} files, {} submodules skipped in {:.1}s; newest {}",
                n.commits,
                n.files,
                n.skipped,
                start.elapsed().as_secs_f64(),
                short(&key)
            );
        }
        _ => unreachable!("handles() lets only ported commands through"),
    }
    Ok(())
}

/// The prover program: next to this one, else on the path.
fn prover() -> Res<std::path::PathBuf> {
    let name = "cas-prove";
    let beside = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(name)));
    let on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|d| d.join(name));
    beside.into_iter().chain(on_path).find(|p| p.is_file()).ok_or_else(|| {
        "the prover is not installed: build it from prove/ (docs/proofs.md) and put cas-prove on the path".to_string()
    })
}

/// The notes of a commit and the version they start from: those of the
/// step with notes, else of any step; a first commit's are on its version.
fn step_notes(s: &dyn Store, c: &Commit) -> Res<(Option<View>, Notes)> {
    let step = c.parents.iter().find(|(_, s)| s.is_some()).or(c.parents.iter().next());
    let (before, notes): (Option<View>, Option<&String>) = match step {
        Some((p, n)) => (Some(commits::read(s, p).map_err(e)?.version()), n.as_ref()),
        None => (None, c.notes.as_ref()),
    };
    let n = match notes {
        Some(k) => commits::read_notes(s, k).map_err(e)?,
        None => Notes::default(),
    };
    Ok((before, n))
}

/// Commands on a release history (spec/formats/release.md).
fn run_release(a: &Args, s: &dyn Store) -> Res<()> {
    let name = a.history.as_str();
    let arg = |i: usize, what: &str| {
        a.rest.get(i).cloned().ok_or_else(|| format!("missing {what}\n{}", crate::help::usage(&a.rest[0])))
    };
    let tip = releases::tip(s, name).map_err(e)?.ok_or(format!("no history {name}"))?;
    let find = |what: &str| -> Res<String> {
        releases::resolve(s, &tip, what).map_err(e)?.ok_or(format!("no release {what} in {name}"))
    };
    let version = || -> Res<View> { Ok(releases::read(s, &find(&a.node)?).map_err(e)?.version()) };
    match a.rest[0].as_str() {
        "log" => {
            for (k, r, tag) in releases::log(s, &tip).map_err(e)? {
                out!("{tag}  {}", short(&k));
                for (from, (changelog, change)) in &r.from {
                    let from_tag = commits::text(s, &releases::read(s, from).map_err(e)?.tag).map_err(e)?;
                    let text = commits::text(s, changelog).map_err(e)?;
                    out!("  {change} from {from_tag}: {text}");
                }
            }
        }
        "ls" => {
            let prefix = a.rest.get(1).map(|p| Path::new(p.as_str()).map_err(e)).transpose()?;
            for item in walk(s, &version()?, prefix.as_ref()) {
                let (p, n) = item.map_err(e)?;
                out!("{:8} {p}", node_text(&Some(n)));
            }
        }
        "checkout" => {
            let dest = arg(1, "<dest>")?;
            let prefix = a.rest.get(2).map(|p| Path::new(p.as_str()).map_err(e)).transpose()?;
            let n = fs::checkout_indexed(s, &version()?, prefix.as_ref(), dest.as_ref(), Some(&a.store)).map_err(e)?;
            out!("{n} paths written to {dest}");
        }
        "sparse" => {
            let dest = arg(1, "<dest>")?;
            let paths = a.rest[2..].iter().map(Path::new).collect::<Result<Vec<_>, _>>().map_err(e)?;
            if paths.is_empty() {
                return Err("sparse needs at least one <path>".into());
            }
            let n = fs::checkout_sparse(s, &version()?, &paths, dest.as_ref(), Some(&a.store)).map_err(e)?;
            out!("{n} paths written to {dest}");
        }
        "compatible" => {
            let (from, to) = (find(&arg(1, "<from>")?)?, find(&arg(2, "<to>")?)?);
            let safe = releases::safe(s, &from, &to).map_err(e)?;
            out!("{}", if safe { "compatible" } else { "breaking" });
        }
        _ => unreachable!("handles() lets only release commands through"),
    }
    Ok(())
}
