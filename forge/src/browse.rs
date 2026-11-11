//! Reading a project: histories, logs, trees, files, steps and diffs.

use crate::names::{ProjectId, check_history};
use crate::project::RELEASE;
use crate::{Error, Forge, Result};
use cas_version::commits::{self, Commit};
use cas_version::{Kind, Path, View, annotated, object_at, read_blob, releases, stepnotes};
use serde_json::{Value as J, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

const HISTORIES_KEPT: usize = 8;

/// The largest file the API returns whole.
pub const MAX_FILE: usize = 8 << 20;

/// A history of commits as the forge reads it: its tip and every commit
/// the tip reaches, newest first.
pub struct Hist {
    pub tip: String,
    pub log: Vec<(String, Commit)>,
    at: HashMap<String, usize>,
}

impl Hist {
    pub fn get(&self, key: &str) -> Option<&Commit> {
        self.at.get(key).map(|i| &self.log[*i].1)
    }

    pub fn has(&self, key: &str) -> bool {
        self.at.contains_key(key)
    }

    /// The version of commit `key`, which the history holds.
    pub fn version(&self, key: &str) -> View {
        self.log[self.at[key]].1.version()
    }

    /// The parent the step into `key` is shown from: the one whose step has
    /// notes, else the first.
    pub fn parent(&self, key: &str) -> Option<&String> {
        let c = self.get(key)?;
        c.parents.iter().find(|(_, s)| s.is_some()).or(c.parents.iter().next()).map(|(p, _)| p)
    }
}

impl Forge {
    /// The history of commits at `view`, from the cache when lately read.
    pub fn history_at(&self, view: &View) -> Result<Arc<Hist>> {
        {
            let c = self.caches.lock().unwrap();
            if let Some((_, h)) = c.histories.iter().find(|(v, _)| v == view) {
                return Ok(h.clone());
            }
        }
        if !commits::is_commit(view) {
            return Err(Error::bad(format!("{view} is not a commit")));
        }
        let log = commits::log(&*self.store, view.key())?;
        let at = log.iter().enumerate().map(|(i, (k, _))| (k.clone(), i)).collect();
        let h = Arc::new(Hist { tip: view.key().to_string(), log, at });
        let mut c = self.caches.lock().unwrap();
        c.histories.push((view.clone(), h.clone()));
        if c.histories.len() > HISTORIES_KEPT {
            c.histories.remove(0);
        }
        Ok(h)
    }

    /// History `var` of `p`, with the view it holds. The release history
    /// is not one of commits: see [`Forge::releases`].
    pub fn history(&self, p: &ProjectId, var: &str) -> Result<Option<(View, Arc<Hist>)>> {
        check_history(var)?;
        match crate::record::head(&*self.store, &p.var(var))? {
            None => Ok(None),
            Some(v) => Ok(Some((v.clone(), self.history_at(&v)?))),
        }
    }

    pub fn require_history(&self, p: &ProjectId, var: &str) -> Result<(View, Arc<Hist>)> {
        self.history(p, var)?.ok_or_else(|| Error::missing(format!("no history {var} in {p}")))
    }

    /// The version at the tip of history `var` of `p`, which may be the
    /// release history.
    pub fn tip_version(&self, p: &ProjectId, var: &str) -> Result<Option<View>> {
        match crate::record::head(&*self.store, &p.var(var))? {
            None => Ok(None),
            Some(v) if releases::is_release(&v) => Ok(Some(releases::read(&*self.store, v.key())?.version())),
            Some(v) => Ok(Some(commits::read(&*self.store, v.key())?.version())),
        }
    }

    /// The histories of `p`: name, view, tip.
    pub fn histories(&self, p: &ProjectId) -> Result<Vec<J>> {
        let prefix = p.prefix();
        let mut out = Vec::new();
        for var in self.store.list_all(&cas_core::Span::prefix(prefix.as_bytes()))? {
            let rest = &var[prefix.len()..];
            if !(rest.starts_with("h/") || rest.starts_with("u/")) {
                continue;
            }
            let Some(v) = crate::record::head(&*self.store, &var)? else { continue };
            out.push(json!({ "name": rest, "view": v.to_string(), "tips": [v.key()] }));
        }
        Ok(out)
    }

    /// The tags of the releases of `p`, by the key of the version released.
    pub fn release_tags(&self, p: &ProjectId) -> Result<BTreeMap<String, Vec<String>>> {
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        if let Some(tip) = releases::tip(&*self.store, &p.var(RELEASE))? {
            for (_, r, tag) in releases::log(&*self.store, &tip)? {
                out.entry(r.version).or_default().push(tag);
            }
        }
        Ok(out)
    }

    /// The commit `what` names in `h`: a key, the start of one, the tag of
    /// a release (the newest commit with the version released), or (empty)
    /// the tip.
    pub fn node(&self, p: &ProjectId, h: &Hist, what: &str) -> Result<String> {
        if what.is_empty() || what == "latest" {
            return Ok(h.tip.clone());
        }
        if h.has(what) {
            return Ok(what.to_string());
        }
        let digits = what.strip_prefix("blake3:").unwrap_or(what);
        if !digits.is_empty() && digits.bytes().all(|c| c.is_ascii_hexdigit()) {
            let want = format!("blake3:{}", digits.to_ascii_lowercase());
            let mut hits = h.log.iter().filter(|(k, _)| k.starts_with(&want));
            if let (Some((k, _)), None) = (hits.next(), hits.next()) {
                return Ok(k.clone());
            }
        }
        let tags = self.release_tags(p)?;
        let version = tags.iter().find(|(_, t)| t.iter().any(|t| t == what)).map(|(v, _)| v);
        version
            .and_then(|v| h.log.iter().find(|(_, c)| c.version == *v))
            .map(|(k, _)| k.clone())
            .ok_or_else(|| Error::missing(format!("no commit {what:?}")))
    }

    /// What a commit says, in one line.
    pub fn line(&self, c: &Commit) -> String {
        commits::line(&*self.store, c).unwrap_or_default()
    }

    fn entry(&self, key: &str, c: &Commit, tags: &BTreeMap<String, Vec<String>>) -> J {
        json!({
            "id": key,
            "version": c.version().to_string(),
            "line": self.line(c),
            "author": commits::text(&*self.store, &c.author).unwrap_or_default(),
            "time": c.time,
            "after": c.parents.iter().map(|(p, n)| json!({ "id": p, "notes": n })).collect::<Vec<_>>(),
            "tags": tags.get(&c.version).cloned().unwrap_or_default(),
        })
    }

    /// The log of history `var`, newest first: `skip` entries left out,
    /// at most `limit`; only commits that changed `path` when one is given.
    pub fn log(&self, p: &ProjectId, var: &str, path: Option<&str>, skip: usize, limit: usize) -> Result<J> {
        let (_, h) = self.require_history(p, var)?;
        let tags = self.release_tags(p)?;
        let (total, page): (usize, Vec<J>) = match path.filter(|p| !p.is_empty()) {
            Some(path) => {
                let hits = commits::path_log(&*self.store, &h.tip, &Path::new(path)?)?;
                (hits.len(), hits.iter().skip(skip).take(limit).map(|(k, c)| self.entry(k, c, &tags)).collect())
            }
            None => (h.log.len(), h.log.iter().skip(skip).take(limit).map(|(k, c)| self.entry(k, c, &tags)).collect()),
        };
        Ok(json!({ "total": total, "entries": page }))
    }

    /// One commit: its entry.
    pub fn node_info(&self, p: &ProjectId, var: &str, what: &str) -> Result<J> {
        let (_, h) = self.require_history(p, var)?;
        let id = self.node(p, &h, what)?;
        Ok(self.entry(&id, h.get(&id).expect("a commit of the history"), &self.release_tags(p)?))
    }

    /// What is at `path` (none: the root) of `version`: a directory's
    /// entries, or a file's view.
    pub fn tree(&self, version: &View, path: Option<&str>) -> Result<J> {
        let at = match path.filter(|p| !p.is_empty()) {
            None => version.clone(),
            Some(p) => object_at(&*self.store, version, &Path::new(p)?)?
                .ok_or_else(|| Error::missing(format!("nothing at {p}")))?,
        };
        let Some(Kind::Dir(d)) = at.known_format().map(|f| &f.kind) else {
            return Ok(json!({ "kind": "file", "view": at.to_string(), "format": format_name(&at) }));
        };
        let items = d.load(&*self.store, at.key(), &read_blob(&*self.store, at.key())?)?;
        let entries: Vec<J> = items
            .iter()
            .map(|it| {
                json!({
                    "name": it.name,
                    "kind": if it.child.is_dir_view() { "dir" } else { "file" },
                    "view": it.child.to_string(),
                    "format": format_name(&it.child),
                })
            })
            .collect();
        Ok(json!({ "kind": "dir", "view": at.to_string(), "entries": entries }))
    }

    /// The file at `path` of `version`: its view and bytes.
    pub fn file(&self, version: &View, path: &str) -> Result<(View, Vec<u8>)> {
        let v = object_at(&*self.store, version, &Path::new(path)?)?
            .ok_or_else(|| Error::missing(format!("no file {path}")))?;
        if v.is_dir_view() {
            return Err(Error::bad(format!("{path} is a directory")));
        }
        let bytes = read_blob(&*self.store, v.key())?;
        Ok((v, bytes))
    }

    /// The diff of the step into commit `what` (from nothing for a first
    /// commit), each line with its note.
    pub fn step(&self, p: &ProjectId, var: &str, what: &str) -> Result<J> {
        let (_, h) = self.require_history(p, var)?;
        let id = self.node(p, &h, what)?;
        let c = h.get(&id).expect("a commit of the history");
        let (from, n) = commits::step(&*self.store, c)?;
        let before = from.map(|f| commits::read(&*self.store, &f).map(|c| c.version())).transpose()?;
        let after = c.version();
        let d = annotated::diff(&*self.store, before.as_ref(), &after)?;
        let sn = stepnotes::StepNotes { before: before.clone(), after: after.clone(), note: n.note, paths: n.paths };
        let notes = stepnotes::line_notes(&sn, &d);
        let lines: Vec<J> = annotated::lines(&d)
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let text = String::from_utf8_lossy(l);
                json!({ "text": text.strip_suffix('\n').unwrap_or(&text), "note": notes.get(i).copied().unwrap_or_default() })
            })
            .collect();
        Ok(json!({
            "entry": self.entry(&id, c, &self.release_tags(p)?),
            "before": before.as_ref().map(View::to_string),
            "after": after.to_string(),
            "note": sn.note,
            "lines": lines,
        }))
    }

    /// The diff between two versions.
    pub fn compare(&self, a: Option<&View>, b: &View) -> Result<J> {
        let d = annotated::diff(&*self.store, a, b)?;
        let lines: Vec<String> = annotated::lines(&d)
            .iter()
            .map(|l| String::from_utf8_lossy(l).trim_end_matches('\n').to_string())
            .collect();
        Ok(json!({ "before": a.map(View::to_string), "after": b.to_string(), "lines": lines }))
    }

    /// The readme of a version, if it has one.
    pub fn readme(&self, p: &ProjectId, version: &View) -> Option<(String, String)> {
        let s = self.settings(p).ok()??;
        let path = s.opt("readme").unwrap_or("README.md").to_string();
        let (_, bytes) = self.file(version, &path).ok()?;
        Some((path, String::from_utf8(bytes).ok()?))
    }
}

/// A format's alias, or its id when unknown.
pub fn format_name(v: &View) -> String {
    v.known_format().map_or_else(|| v.format().to_string(), |f| f.alias.to_string())
}

/// Parse a view given in a query.
pub fn view(s: &str) -> Result<View> {
    View::parse(s).map_err(|e| Error::bad(e.to_string()))
}
