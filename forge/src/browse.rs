//! Reading a project: histories, logs, trees, files, steps and diffs.

use crate::names::{ProjectId, check_history};
use crate::{Error, Forge, Result};
use cas_version::history::{History, NodeId};
use cas_version::{Kind, Path, View, annotated, object_at, read_blob, stepnotes};
use serde_json::{Value as J, json};
use std::sync::Arc;

const HISTORIES_KEPT: usize = 8;

/// The largest file the API returns whole.
pub const MAX_FILE: usize = 8 << 20;

impl Forge {
    /// The history at `view`, from the cache when lately read.
    pub fn history_at(&self, view: &View) -> Result<Arc<History>> {
        {
            let c = self.caches.lock().unwrap();
            if let Some((_, h)) = c.histories.iter().find(|(v, _)| v == view) {
                return Ok(h.clone());
            }
        }
        let h = Arc::new(cas_version::history::read(&*self.store, view)?);
        let mut c = self.caches.lock().unwrap();
        c.histories.push((view.clone(), h.clone()));
        if c.histories.len() > HISTORIES_KEPT {
            c.histories.remove(0);
        }
        Ok(h)
    }

    /// History `var` of `p`, with the view it holds.
    pub fn history(&self, p: &ProjectId, var: &str) -> Result<Option<(View, Arc<History>)>> {
        check_history(var)?;
        match cas_version::history::head(&*self.store, &p.var(var))? {
            None => Ok(None),
            Some(v) => Ok(Some((v.clone(), self.history_at(&v)?))),
        }
    }

    pub fn require_history(&self, p: &ProjectId, var: &str) -> Result<(View, Arc<History>)> {
        self.history(p, var)?.ok_or_else(|| Error::missing(format!("no history {var} in {p}")))
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
            let Some(v) = cas_version::history::head(&*self.store, &var)? else { continue };
            let tips = cas_version::derived::heads(&*self.store, &v).map(|h| h.tips).unwrap_or_default();
            out.push(json!({
                "name": rest,
                "view": v.to_string(),
                "tips": tips.keys().map(|t| t.to_string()).collect::<Vec<_>>(),
            }));
        }
        Ok(out)
    }

    /// The node `what` names in `h`: a tag, a node id, a unique key
    /// prefix, or (empty) the tip.
    pub fn node(&self, h: &History, what: &str) -> Result<NodeId> {
        let what = if what.is_empty() { "latest" } else { what };
        h.resolve(what).ok_or_else(|| Error::missing(format!("no node {what:?}")))
    }

    /// What a node's steps say, in one line.
    pub fn line(&self, h: &History, id: &NodeId) -> String {
        let n = &h.nodes[id];
        let lines: Vec<String> = n.after.values().filter_map(|s| s.line(&*self.store).ok().flatten()).collect();
        if lines.is_empty() { n.doc.clone().unwrap_or_default() } else { lines.join(" / ") }
    }

    fn entry(&self, h: &History, id: &NodeId) -> J {
        let n = &h.nodes[id];
        let tags: Vec<&String> = h.tags.iter().filter(|(_, t)| *t == id).map(|(k, _)| k).collect();
        json!({
            "id": id.to_string(),
            "version": id.version().to_string(),
            "line": self.line(h, id),
            "doc": n.doc,
            "after": n.after.iter().map(|(p, s)| json!({
                "id": p.to_string(), "compatible": s.compatible, "changelog": s.changelog,
                "notes": s.notes.as_ref().map(View::to_string),
            })).collect::<Vec<_>>(),
            "tags": tags,
        })
    }

    /// The log of history `var`, newest first: `skip` entries left out,
    /// at most `limit`; only nodes that changed `path` when one is given.
    pub fn log(&self, p: &ProjectId, var: &str, path: Option<&str>, skip: usize, limit: usize) -> Result<J> {
        let (_, h) = self.require_history(p, var)?;
        let ids: Vec<NodeId> = match path.filter(|p| !p.is_empty()) {
            Some(path) => cas_version::derived::path_log(&*self.store, &h, &Path::new(path)?)?,
            None => h.log(),
        };
        let total = ids.len();
        let page: Vec<J> = ids.iter().skip(skip).take(limit).map(|id| self.entry(&h, id)).collect();
        Ok(json!({ "total": total, "entries": page }))
    }

    /// One node: its entry and its steps.
    pub fn node_info(&self, p: &ProjectId, var: &str, what: &str) -> Result<J> {
        let (_, h) = self.require_history(p, var)?;
        let id = self.node(&h, what)?;
        Ok(self.entry(&h, &id))
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

    /// The diff of the step into node `what` from its first predecessor
    /// (from nothing for a root), each line with its note.
    pub fn step(&self, p: &ProjectId, var: &str, what: &str) -> Result<J> {
        let (_, h) = self.require_history(p, var)?;
        let id = self.node(&h, what)?;
        let n = &h.nodes[&id];
        let (before, step) = match n.after.iter().next() {
            Some((b, s)) => (Some(b.version()), Some(s)),
            None => (None, None),
        };
        let after = id.version();
        let d = annotated::diff(&*self.store, before.as_ref(), &after)?;
        let mut note = String::new();
        let mut notes: Vec<String> = Vec::new();
        if let Some(v) = step.and_then(|s| s.notes.as_ref())
            && v.known_format().is_some_and(|f| f.alias == "step-notes")
        {
            let sn = stepnotes::verify(&*self.store, v.key())?;
            note = sn.note.clone();
            notes = stepnotes::line_notes(&sn, &d).into_iter().map(String::from).collect();
        }
        let lines: Vec<J> = annotated::lines(&d)
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let text = String::from_utf8_lossy(l);
                json!({ "text": text.strip_suffix('\n').unwrap_or(&text), "note": notes.get(i).cloned().unwrap_or_default() })
            })
            .collect();
        Ok(json!({
            "entry": self.entry(&h, &id),
            "before": before.as_ref().map(View::to_string),
            "after": after.to_string(),
            "note": note,
            "changelog": step.and_then(|s| s.changelog.clone()),
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
