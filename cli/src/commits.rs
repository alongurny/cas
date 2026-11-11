//! Commands on a history of commits (spec/formats/commit.md): the history
//! variable holds a view of its newest commit, versions are `directory`
//! trees, and notes are `annotation` trees.

use crate::{Args, Res, b3, e, node_text, short};
use cas_core::Store;
use cas_core::disk::DiskBackend;
use cas_version::commits::{self, Commit, Notes};
use cas_version::stepnotes::{self, StepNotes};
use cas_version::{Path, View, annotated, diff, formats, fs, history, object_at, walk};
use std::collections::{BTreeMap, HashMap};

/// The commands that work on a history of commits.
const PORTED: &[&str] =
    &["commit", "status", "reset", "clean", "log", "ls", "diff", "checkout", "sparse", "import-git"];

/// The commands that are not about one history.
const NO_HISTORY: &[&str] = &["gc", "fsck", "objects", "show", "serve", "access", "annotated"];

/// Whether the command is for this module: the history holds a commit, or
/// it is unset and `--commits` asks for one.
pub fn handles(a: &Args, s: &dyn Store) -> Res<bool> {
    let Some(cmd) = a.rest.first().map(String::as_str) else { return Ok(false) };
    if NO_HISTORY.contains(&cmd) {
        return Ok(false);
    }
    let name = a.history.as_str();
    let ours = match history::head(s, name).map_err(e)? {
        Some(v) => commits::is_commit(&v),
        None => a.commits,
    };
    if !ours {
        if a.commits {
            return Err(format!("history {name} is in the earlier format; --commits starts a new history"));
        }
        return Ok(false);
    }
    if !PORTED.contains(&cmd) {
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
    let arg = |i: usize, what: &str| a.rest.get(i).cloned().ok_or(format!("missing {what}"));
    let prefix = |i: usize| a.rest.get(i).map(|p| Path::new(p.as_str()).map_err(e)).transpose();
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
            if !a.annotate.is_empty() {
                return Err("commit --annotate is not yet available for a history of commits".into());
            }
            let head = match &tip {
                Some(t) => Some(commits::read(s, t).map_err(e)?.version()),
                None => None,
            };
            let root = fs::import_worktree(s, b3(), dirf, dir.as_ref(), Some(&a.store), head.as_ref()).map_err(e)?;
            formats::publish(s).map_err(e)?;
            let mut notes = None;
            if let Some(file) = &a.notes {
                // Written on the diff from the tip to this version, which
                // the file's text must be exactly.
                let form = std::fs::read(file).map_err(|x| format!("{file}: {x}"))?;
                let sn = stepnotes::from_form(s, head.as_ref(), &root, &form).map_err(|x| format!("{file}: {x}"))?;
                if let Some(sn) = sn {
                    let n = Notes { note: sn.note, paths: sn.paths };
                    commits::fits(s, &n, head.as_ref(), &root).map_err(|x| format!("{file}: {x}"))?;
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
                let step = c.parents.iter().find(|(_, s)| s.is_some()).or(c.parents.iter().next());
                let (before, notes): (Option<View>, Option<&String>) = match step {
                    Some((p, n)) => (Some(commits::read(s, p).map_err(e)?.version()), n.as_ref()),
                    None => (None, c.notes.as_ref()),
                };
                let n = match notes {
                    Some(k) => commits::read_notes(s, k).map_err(e)?,
                    None => Notes::default(),
                };
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
