//! Blame for any version (spec/forge/checks.md, last part): a derived,
//! incremental function. The result for a node is an annotated file of
//! the path's text there, each line noting the node that last changed it,
//! computed from the result for the node before it along the first
//! predecessors.

use crate::names::ProjectId;
use crate::{Error, Forge, Result};
use cas_core::Expect;
use cas_version::history::{History, NodeId};
use cas_version::uni::Value;
use cas_version::{Path, View, annotated, object_at, put, read_blob};
use serde_json::{Value as J, json};

const SPEC: &str = "blame: an annotated file of a path's text at a node, each line noting the node id that last changed it, from the blame of the node's first predecessor that changed the path";

fn function(f: &Forge) -> String {
    let obj = Value::map([
        ("name", Value::str("blame")),
        ("input", Value::str(cas_version::by_alias("uni-bin").unwrap().id())),
        ("output", Value::str(cas_version::by_alias("annotated").unwrap().id())),
        ("spec", Value::str(f.alg.key(SPEC.as_bytes()))),
    ]);
    f.alg.key(&obj.serialize())
}

/// The nodes from `at` back along first predecessors whose file at `path`
/// differs from their predecessor's, oldest first, with the file at each.
fn changes(f: &Forge, h: &History, at: &NodeId, path: &Path) -> Result<Vec<(NodeId, View)>> {
    let mut out = Vec::new();
    let mut cur = Some(at.clone());
    let mut file = object_at(&*f.store, &at.version(), path)?;
    while let (Some(id), Some(here)) = (cur.clone(), file.clone()) {
        let pred = h.nodes.get(&id).and_then(|n| n.after.keys().next().cloned());
        let before = match &pred {
            Some(p) => object_at(&*f.store, &p.version(), path)?,
            None => None,
        };
        if before.as_ref() != Some(&here) {
            out.push((id, here));
        }
        cur = pred;
        file = before;
    }
    out.reverse();
    Ok(out)
}

fn lines_of(f: &Forge, v: &View) -> Result<Vec<String>> {
    let b = read_blob(&*f.store, v.key())?;
    Ok(annotated::lines(&b).iter().map(|l| String::from_utf8_lossy(l).into_owned()).collect())
}

impl Forge {
    /// Blame of `path` at node `what` of history `var`: each line's text and
    /// the node that last changed it.
    pub fn blame(&self, p: &ProjectId, var: &str, what: &str, path: &str) -> Result<J> {
        let (_, h) = self.require_history(p, var)?;
        let at = self.node(&h, what)?;
        let path = Path::new(path)?;
        let chain = changes(self, &h, &at, &path)?;
        if chain.is_empty() {
            return Err(Error::missing(format!("no file {path} at that node")));
        }
        let fid = function(self);
        let mut prev_q = String::new();
        let mut prev: Option<(Vec<String>, Vec<String>)> = None;
        let mut last = None;
        for (id, file) in &chain {
            if file.is_dir_view() {
                return Err(Error::bad(format!("{path} is a directory")));
            }
            let q = Value::map([
                ("after", Value::str(prev_q.clone())),
                ("node", Value::str(id.to_string())),
                ("path", Value::str(path.as_str())),
            ]);
            let qv = put(&*self.store, self.alg, "uni-bin", &q.serialize())?;
            let var = format!("derived/{fid}/{}", qv.key());
            let text = lines_of(self, file)?;
            let notes = match cas_version::history::head(&*self.store, &var)? {
                Some(r) => annotated::verify(&*self.store, r.key())?.notes().into_iter().map(String::from).collect(),
                None => {
                    let notes: Vec<String> = match &prev {
                        None => vec![id.to_string(); text.len()],
                        Some((old_text, old_notes)) => {
                            let a: Vec<&str> = old_text.iter().map(String::as_str).collect();
                            let b: Vec<&str> = text.iter().map(String::as_str).collect();
                            let mut notes = vec![id.to_string(); text.len()];
                            let d = similar::capture_diff_slices(similar::Algorithm::Myers, &a, &b);
                            for op in d {
                                if let similar::DiffOp::Equal { old_index, new_index, len } = op {
                                    notes[new_index..new_index + len]
                                        .clone_from_slice(&old_notes[old_index..old_index + len]);
                                }
                            }
                            notes
                        }
                    };
                    let r = annotated::write(&*self.store, self.alg, file, &notes)?;
                    match self.store.write(&var, r.to_string().as_bytes(), &Expect::Absent) {
                        Ok(()) | Err(cas_core::Error::Conflict) => {}
                        Err(e) => return Err(e.into()),
                    }
                    notes
                }
            };
            prev_q = qv.key().to_string();
            last = Some((text.clone(), notes.clone()));
            prev = Some((text, notes));
        }
        let (text, notes) = last.unwrap();
        let mut lines = Vec::new();
        for (t, n) in text.iter().zip(&notes) {
            let line = NodeId::parse(n).ok().filter(|id| h.nodes.contains_key(id)).map(|id| self.line(&h, &id));
            lines.push(json!({ "text": t.trim_end_matches('\n'), "node": n, "line": line }));
        }
        Ok(json!({ "path": path.as_str(), "node": at.to_string(), "lines": lines }))
    }
}
