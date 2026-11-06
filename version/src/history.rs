//! Histories (spec/history.md, spec/formats/history.md): versions in an
//! order, with changelog, compatibility, tags and docs. A node is a
//! version; versions have no parents. A history object never changes; a
//! store name (a mutable, compare-and-set value) points at the current one.

use crate::uni::Value;
use crate::{Error, Ref, Result, by_alias, malformed, read_blob, write_blob};
use cas_core::algo::Algorithm;
use cas_core::{Error as CoreError, Expect, Store};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// A node of a history: a version, plus `#n` when it occurs again.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(String);

impl NodeId {
    pub fn parse(s: &str) -> std::result::Result<NodeId, String> {
        let (v, n) = match s.rsplit_once('#') {
            Some((v, n)) => (v, Some(n)),
            None => (s, None),
        };
        Ref::parse(v).map_err(|_| format!("node id {s:?}: not a reference"))?;
        if let Some(n) = n
            && (n.starts_with('0')
                || n.parse::<u64>().map_or(true, |n| n < 2)
                || !n.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(format!("node id {s:?}: bad occurrence number"));
        }
        Ok(NodeId(s.to_string()))
    }

    pub fn of(version: &Ref) -> NodeId {
        NodeId(version.to_string())
    }

    pub fn version(&self) -> Ref {
        Ref::parse(self.0.rsplit_once('#').map_or(&self.0[..], |(v, _)| v)).unwrap()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The change from a predecessor to a node.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Step {
    pub changelog: Option<String>,
    /// Declared a compatible upgrade. Default: breaking.
    pub compatible: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistNode {
    pub after: BTreeMap<NodeId, Step>,
    pub doc: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct History {
    pub doc: Option<String>,
    pub nodes: BTreeMap<NodeId, HistNode>,
    pub tags: BTreeMap<String, NodeId>,
}

fn history_format() -> &'static str {
    by_alias("history").unwrap().id()
}

fn opt_str(m: &mut BTreeMap<Value, Value>, k: &str, v: &Option<String>) {
    if let Some(v) = v {
        m.insert(Value::str(k), Value::str(v));
    }
}

fn map_of(v: &Value, what: &str) -> std::result::Result<BTreeMap<String, Value>, String> {
    let m = v.as_map().ok_or(format!("{what} is not a map"))?;
    m.iter().map(|(k, v)| Ok((k.as_str().ok_or(format!("{what} has a map key"))?.to_string(), v.clone()))).collect()
}

/// Read the optional fields of `m`, refusing any other key.
fn fields(v: &Value, what: &str, allowed: &[&str]) -> std::result::Result<BTreeMap<String, Value>, String> {
    let m = map_of(v, what)?;
    if let Some(k) = m.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(format!("{what} has unknown key {k:?}"));
    }
    Ok(m)
}

fn string(v: Option<&Value>, what: &str) -> std::result::Result<Option<String>, String> {
    v.map(|v| v.as_str().map(String::from).ok_or(format!("{what} is not a string"))).transpose()
}

impl History {
    pub fn to_value(&self) -> Value {
        let mut top = BTreeMap::new();
        opt_str(&mut top, "doc", &self.doc);
        if !self.nodes.is_empty() {
            let nodes = self.nodes.iter().map(|(id, n)| {
                let mut m = BTreeMap::new();
                opt_str(&mut m, "doc", &n.doc);
                if !n.after.is_empty() {
                    let after = n.after.iter().map(|(p, s)| {
                        let mut sm = BTreeMap::new();
                        opt_str(&mut sm, "changelog", &s.changelog);
                        if s.compatible {
                            sm.insert(Value::str("compatible"), Value::empty());
                        }
                        (Value::str(p.as_str()), Value::Map(sm))
                    });
                    m.insert(Value::str("after"), Value::Map(after.collect()));
                }
                (Value::str(id.as_str()), Value::Map(m))
            });
            top.insert(Value::str("nodes"), Value::Map(nodes.collect()));
        }
        if !self.tags.is_empty() {
            let tags = self.tags.iter().map(|(t, n)| (Value::str(t), Value::str(n.as_str())));
            top.insert(Value::str("tags"), Value::Map(tags.collect()));
        }
        Value::Map(top)
    }

    pub fn from_value(v: &Value) -> std::result::Result<History, String> {
        let top = fields(v, "a history", &["doc", "nodes", "tags"])?;
        let mut h = History { doc: string(top.get("doc"), "doc")?, ..History::default() };
        for (id, n) in top.get("nodes").map_or(Ok(BTreeMap::new()), |v| map_of(v, "nodes"))? {
            let id = NodeId::parse(&id)?;
            let f = fields(&n, "a node", &["after", "doc"])?;
            let mut node = HistNode { doc: string(f.get("doc"), "node doc")?, after: BTreeMap::new() };
            for (p, s) in f.get("after").map_or(Ok(BTreeMap::new()), |v| map_of(v, "after"))? {
                let sf = fields(&s, "a step", &["changelog", "compatible"])?;
                let compatible = match sf.get("compatible") {
                    None => false,
                    Some(v) if *v == Value::empty() => true,
                    Some(_) => return Err("compatible is {} when present".into()),
                };
                node.after.insert(
                    NodeId::parse(&p)?,
                    Step { changelog: string(sf.get("changelog"), "changelog")?, compatible },
                );
            }
            h.nodes.insert(id, node);
        }
        for (t, n) in top.get("tags").map_or(Ok(BTreeMap::new()), |v| map_of(v, "tags"))? {
            if t.is_empty() {
                return Err("empty tag name".into());
            }
            let n = NodeId::parse(n.as_str().ok_or("a tag names a node id")?)?;
            h.tags.insert(t, n);
        }
        h.validate()?;
        Ok(h)
    }

    fn validate(&self) -> std::result::Result<(), String> {
        for (id, n) in &self.nodes {
            if let Some(p) = n.after.keys().find(|p| !self.nodes.contains_key(*p)) {
                return Err(format!("{id} follows {p}, which is not a node"));
            }
        }
        if let Some((t, n)) = self.tags.iter().find(|(_, n)| !self.nodes.contains_key(*n)) {
            return Err(format!("tag {t} names {n}, which is not a node"));
        }
        if self.log().len() != self.nodes.len() {
            return Err("the order has a cycle".into());
        }
        Ok(())
    }

    /// Nodes no other node follows.
    pub fn tips(&self) -> BTreeSet<NodeId> {
        let followed: BTreeSet<&NodeId> = self.nodes.values().flat_map(|n| n.after.keys()).collect();
        self.nodes.keys().filter(|id| !followed.contains(id)).cloned().collect()
    }

    /// The single tip, if the history has exactly one.
    pub fn tip(&self) -> Option<NodeId> {
        let t = self.tips();
        if t.len() == 1 { t.into_iter().next() } else { None }
    }

    /// Every node, newest first: each before its predecessors, ties going
    /// to the smallest id. Nodes on a cycle are left out.
    pub fn log(&self) -> Vec<NodeId> {
        let mut followers: BTreeMap<&NodeId, usize> = self.nodes.keys().map(|id| (id, 0)).collect();
        for n in self.nodes.values() {
            for p in n.after.keys() {
                if let Some(c) = followers.get_mut(p) {
                    *c += 1;
                }
            }
        }
        let mut ready: BTreeSet<&NodeId> = followers.iter().filter(|(_, c)| **c == 0).map(|(id, _)| *id).collect();
        let mut out = Vec::new();
        while let Some(id) = ready.pop_first() {
            for p in self.nodes[id].after.keys() {
                let c = followers.get_mut(p).unwrap();
                *c -= 1;
                if *c == 0 {
                    ready.insert(p);
                }
            }
            out.push(id.clone());
        }
        out
    }

    /// `id` and every node it follows, transitively.
    pub fn ancestors(&self, id: &NodeId) -> BTreeSet<NodeId> {
        let mut seen = BTreeSet::new();
        let mut todo = vec![id.clone()];
        while let Some(n) = todo.pop() {
            if let Some(node) = self.nodes.get(&n)
                && seen.insert(n)
            {
                todo.extend(node.after.keys().cloned());
            }
        }
        seen
    }

    /// Whether `b` follows `a` (or is `a`).
    pub fn follows(&self, b: &NodeId, a: &NodeId) -> bool {
        self.ancestors(b).contains(a)
    }

    /// The best common ancestors of `a` and `b`.
    pub fn merge_bases(&self, a: &NodeId, b: &NodeId) -> BTreeSet<NodeId> {
        let common: BTreeSet<NodeId> = self.ancestors(a).intersection(&self.ancestors(b)).cloned().collect();
        let beaten: BTreeSet<NodeId> = common
            .iter()
            .flat_map(|c| self.nodes[c].after.keys().flat_map(|p| self.ancestors(p)).collect::<Vec<_>>())
            .collect();
        common.into_iter().filter(|c| !beaten.contains(c)).collect()
    }

    /// Whether upgrading from `a` to `b` is compatible: some chain of steps
    /// from `a` to `b` is compatible throughout. `a` to itself is.
    pub fn compatible(&self, a: &NodeId, b: &NodeId) -> bool {
        let mut seen = BTreeSet::new();
        let mut todo = vec![b.clone()];
        while let Some(n) = todo.pop() {
            if n == *a {
                return true;
            }
            if seen.insert(n.clone())
                && let Some(node) = self.nodes.get(&n)
            {
                todo.extend(node.after.iter().filter(|(_, s)| s.compatible).map(|(p, _)| p.clone()));
            }
        }
        false
    }

    /// Whether `self` extends `old`: holds every node of `old` with the
    /// same steps. Tags and docs may differ.
    pub fn extends(&self, old: &History) -> bool {
        old.nodes.iter().all(|(id, n)| self.nodes.get(id).is_some_and(|m| m.after == n.after))
    }

    /// A fresh id for `version`: the plain reference, or `#n` for its n-th
    /// occurrence.
    pub fn new_id(&self, version: &Ref) -> NodeId {
        let base = NodeId::of(version);
        if !self.nodes.contains_key(&base) {
            return base;
        }
        (2..).map(|n| NodeId(format!("{base}#{n}"))).find(|id| !self.nodes.contains_key(id)).unwrap()
    }

    /// Add `version` after `after` (each with its step), returning its id.
    pub fn add(&mut self, version: &Ref, after: BTreeMap<NodeId, Step>, doc: Option<String>) -> Result<NodeId> {
        if let Some(p) = after.keys().find(|p| !self.nodes.contains_key(*p)) {
            return Err(Error::Missing(format!("node {p}")));
        }
        let id = self.new_id(version);
        self.nodes.insert(id.clone(), HistNode { after, doc });
        Ok(id)
    }

    /// Add `version` after the single tip (or as the first node), with
    /// `step` from it.
    pub fn append(&mut self, version: &Ref, step: Step, doc: Option<String>) -> Result<NodeId> {
        let after = match (self.nodes.is_empty(), self.tip()) {
            (true, _) => BTreeMap::new(),
            (false, Some(t)) => BTreeMap::from([(t, step)]),
            (false, None) => return Err(Error::Diverged("the history has several tips; merge them first".into())),
        };
        self.add(version, after, doc)
    }

    /// Resolve a tag name, a node id, or `latest` (the single tip).
    pub fn resolve(&self, what: &str) -> Option<NodeId> {
        if let Some(n) = self.tags.get(what) {
            return Some(n.clone());
        }
        if what == "latest" {
            return self.tip();
        }
        NodeId::parse(what).ok().filter(|id| self.nodes.contains_key(id)).or_else(|| {
            // A unique key prefix of a version, for convenience.
            let hits: Vec<&NodeId> = self.nodes.keys().filter(|id| id.version().key().contains(what)).collect();
            if hits.len() == 1 { Some(hits[0].clone()) } else { None }
        })
    }
}

/// Check the bytes of a `history` blob.
pub(crate) fn check(bytes: &[u8]) -> std::result::Result<(), String> {
    History::from_value(&Value::deserialize(bytes)?).map(|_| ())
}

pub fn write(store: &dyn Store, alg: &'static Algorithm, h: &History) -> Result<Ref> {
    let key = write_blob(store, alg, &h.to_value().serialize())?;
    Ok(Ref { format: history_format().to_string(), key })
}

pub fn read(store: &dyn Store, r: &Ref) -> Result<History> {
    if r.format() != history_format() {
        return Err(Error::UnknownFormat(format!("{} is not the history format", r.format())));
    }
    let bytes = read_blob(store, r.key())?;
    let value = Value::deserialize(&bytes).map_err(|e| malformed(r.key(), e))?;
    History::from_value(&value).map_err(|e| malformed(r.key(), e))
}

/// The history object `name` points at, if `name` is set.
pub fn head(store: &dyn Store, name: &str) -> Result<Option<Ref>> {
    match store.read(name)? {
        None => Ok(None),
        Some(v) => {
            let s = String::from_utf8(v).map_err(|_| malformed(name, "value is not UTF-8"))?;
            Ref::parse(&s).map(Some).map_err(|_| malformed(name, "value is not a reference"))
        }
    }
}

/// The history `name` holds (empty if unset) and the reference it was read
/// from, for [`save`].
pub fn load(store: &dyn Store, name: &str) -> Result<(Option<Ref>, History)> {
    match head(store, name)? {
        None => Ok((None, History::default())),
        Some(r) => Ok((Some(r.clone()), read(store, &r)?)),
    }
}

/// Write `h` and move `name` to it, only if `name` still holds `old`.
pub fn save(store: &dyn Store, alg: &'static Algorithm, name: &str, old: Option<&Ref>, h: &History) -> Result<Ref> {
    let new = write(store, alg, h)?;
    let expect = old.map_or(Expect::Absent, |r| Expect::Value(r.to_string().into_bytes()));
    if old != Some(&new) {
        store.write(name, new.to_string().as_bytes(), &expect)?;
    }
    Ok(new)
}

/// Load `name`, apply `f`, save; retried while other writers win the race.
pub fn record<T>(
    store: &dyn Store,
    alg: &'static Algorithm,
    name: &str,
    mut f: impl FnMut(&mut History) -> Result<T>,
) -> Result<(Ref, T)> {
    loop {
        let (old, mut h) = load(store, name)?;
        let out = f(&mut h)?;
        match save(store, alg, name, old.as_ref(), &h) {
            Err(Error::Store(CoreError::Conflict)) => continue,
            r => return r.map(|r| (r, out)),
        }
    }
}

/// Append `version` to history `name` after its tip.
pub fn commit(
    store: &dyn Store,
    alg: &'static Algorithm,
    name: &str,
    version: &Ref,
    step: Step,
    doc: Option<String>,
) -> Result<NodeId> {
    record(store, alg, name, |h| h.append(version, step.clone(), doc.clone())).map(|(_, id)| id)
}
