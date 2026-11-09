//! The stored form of a history (spec/formats/history.md): a small root
//! (doc, tags, tips) over a tree of `history-part` blobs that hold the
//! nodes. The tree is chunked by node id, with chunk ends chosen by a hash
//! of the id alone, so it is a function of the nodes: two histories with
//! the same content have the same key however they were built. A commit
//! rewrites one leaf and the parts above it, not the whole history.

use crate::history::{HistNode, History, NodeId};
use crate::uni::{Cursor, Value};
use crate::{Error, Result, View, by_alias, malformed, read_blob};
use cas_core::algo::Algorithm;
use cas_core::store::Put;
use cas_core::{Expect, Store};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

/// Bits of the id hash per level: chunks hold 2^6 = 64 entries on average.
const BITS: u32 = 6;

/// Reads of parts per batch.
const BATCH: usize = 256;

/// How many levels a node id ends a chunk at: an entry ends its chunk at
/// level `l` when its height is greater than `l`.
pub fn height(id: &NodeId) -> u32 {
    let h = cas_core::algo::lookup("blake3").unwrap().hash(id.as_str().as_bytes());
    u64::from_le_bytes(h[..8].try_into().unwrap()).trailing_zeros() / BITS
}

pub fn root_format() -> &'static str {
    by_alias("history").unwrap().id()
}

pub fn part_format() -> &'static str {
    by_alias("history-part").unwrap().id()
}

fn map_entries<'a>(v: &'a Value, what: &str) -> std::result::Result<Vec<(&'a str, &'a Value)>, String> {
    let m = v.as_map().ok_or(format!("{what} is not a map"))?;
    m.iter().map(|(k, v)| Ok((k.as_str().ok_or(format!("{what} has a map key"))?, v))).collect()
}

/// The root blob of a history.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Root {
    pub doc: Option<String>,
    pub tags: BTreeMap<String, NodeId>,
    /// The nodes no other node follows.
    pub tips: BTreeSet<NodeId>,
    /// The top part of the node tree; `None` for no nodes.
    pub nodes: Option<View>,
}

impl Root {
    pub fn encode(&self) -> Vec<u8> {
        let mut top = BTreeMap::new();
        if let Some(d) = &self.doc {
            top.insert(Value::str("doc"), Value::str(d));
        }
        if let Some(n) = &self.nodes {
            top.insert(Value::str("nodes"), Value::str(n.to_string()));
        }
        if !self.tags.is_empty() {
            let tags = self.tags.iter().map(|(t, n)| (Value::str(t), Value::str(n.as_str())));
            top.insert(Value::str("tags"), Value::Map(tags.collect()));
        }
        if !self.tips.is_empty() {
            let tips = self.tips.iter().map(|n| (Value::str(n.as_str()), Value::empty()));
            top.insert(Value::str("tips"), Value::Map(tips.collect()));
        }
        Value::Map(top).serialize()
    }

    pub fn decode(b: &[u8]) -> std::result::Result<Root, String> {
        let v = Value::deserialize(b)?;
        let mut r = Root::default();
        for (k, v) in map_entries(&v, "a history")? {
            match k {
                "doc" => r.doc = Some(v.as_str().ok_or("doc is not a string")?.to_string()),
                "nodes" => {
                    let s = v.as_str().ok_or("nodes is not a string")?;
                    let n = View::parse(s).map_err(|_| "nodes is not a view")?;
                    if n.format() != part_format() {
                        return Err("nodes is not a history-part".into());
                    }
                    r.nodes = Some(n);
                }
                "tags" => {
                    for (t, n) in map_entries(v, "tags")? {
                        if t.is_empty() {
                            return Err("empty tag name".into());
                        }
                        r.tags.insert(t.to_string(), NodeId::parse(n.as_str().ok_or("a tag names a node id")?)?);
                    }
                }
                "tips" => {
                    for (t, x) in map_entries(v, "tips")? {
                        if *x != Value::empty() {
                            return Err("a tip maps to {}".into());
                        }
                        r.tips.insert(NodeId::parse(t)?);
                    }
                }
                k => return Err(format!("a history has unknown key {k:?}")),
            }
        }
        if r.nodes.is_none() && !(r.tips.is_empty() && r.tags.is_empty()) {
            return Err("tips or tags without nodes".into());
        }
        if r.nodes.is_some() && r.tips.is_empty() {
            return Err("nodes without tips".into());
        }
        Ok(r)
    }
}

/// A node as a leaf stores it: with its generation, one more than the
/// greatest generation of its predecessors (1 for a root), as git's
/// commit-graph keeps. An ancestor always has a smaller generation, so a
/// walk newest first can stop early.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub node: HistNode,
    pub generation: u64,
}

/// The generation of each node of `h`.
pub fn generations(h: &History) -> HashMap<NodeId, u64> {
    let mut g: HashMap<NodeId, u64> = HashMap::with_capacity(h.nodes.len());
    for id in h.log().into_iter().rev() {
        let n = 1 + h.nodes[&id].after.keys().filter_map(|p| g.get(p)).max().copied().unwrap_or(0);
        g.insert(id, n);
    }
    g
}

/// One `history-part` blob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    /// Nodes by id.
    Leaf(BTreeMap<NodeId, Entry>),
    /// Parts one level down, each by the last node id under it.
    Inner { level: u32, parts: BTreeMap<NodeId, View> },
}

impl Part {
    pub fn encode(&self) -> Vec<u8> {
        let mut top = BTreeMap::new();
        match self {
            Part::Leaf(nodes) => {
                let m = nodes.iter().map(|(id, e)| {
                    let mut v = e.node.to_value();
                    if let Value::Map(m) = &mut v {
                        m.insert(Value::str("generation"), Value::str(e.generation.to_string()));
                    }
                    (Value::str(id.as_str()), v)
                });
                top.insert(Value::str("nodes"), Value::Map(m.collect()));
            }
            Part::Inner { level, parts } => {
                top.insert(Value::str("level"), Value::str(level.to_string()));
                let m = parts.iter().map(|(id, v)| (Value::str(id.as_str()), Value::str(v.to_string())));
                top.insert(Value::str("parts"), Value::Map(m.collect()));
            }
        }
        Value::Map(top).serialize()
    }

    pub fn decode(b: &[u8]) -> std::result::Result<Part, String> {
        let mut c = Cursor::new(b);
        let (mut level, mut nodes, mut parts) = (None, None, None);
        let mut top = c.map()?;
        while let Some(k) = c.key(&mut top, "a history part")? {
            match k {
                "level" => {
                    let s = c.str().map_err(|_| "level is not a string")?;
                    if s.starts_with('0') || !s.bytes().all(|b| b.is_ascii_digit()) {
                        return Err("level is a decimal number from 1".into());
                    }
                    level = Some(s.parse::<u32>().map_err(|_| "level is too large")?);
                }
                "nodes" => {
                    let mut m = c.map().map_err(|_| "nodes is not a map")?;
                    let mut out = BTreeMap::new();
                    while let Some(id) = c.key(&mut m, "nodes")? {
                        let id = NodeId::parse(id)?;
                        let mut generation = None;
                        let node = HistNode::read_with(&mut c, Some(&mut generation))?;
                        let generation = generation.ok_or("a leaf's node has no generation")?;
                        out.insert(id, Entry { node, generation });
                    }
                    nodes = Some(out);
                }
                "parts" => {
                    let mut m = c.map().map_err(|_| "parts is not a map")?;
                    let mut out = BTreeMap::new();
                    while let Some(id) = c.key(&mut m, "parts")? {
                        let id = NodeId::parse(id)?;
                        let v = View::parse(c.str().map_err(|_| "a part is not a view")?)
                            .map_err(|_| "a part is not a view")?;
                        if v.format() != part_format() {
                            return Err("a part is not a history-part".into());
                        }
                        out.insert(id, v);
                    }
                    parts = Some(out);
                }
                k => return Err(format!("a history part has unknown key {k:?}")),
            }
        }
        c.done()?;
        match (level, nodes, parts) {
            (None, Some(n), None) if !n.is_empty() => Ok(Part::Leaf(n)),
            (Some(level), None, Some(parts)) if !parts.is_empty() => Ok(Part::Inner { level, parts }),
            _ => Err("a part has non-empty nodes, or a level and non-empty parts".into()),
        }
    }

    fn last(&self) -> &NodeId {
        match self {
            Part::Leaf(n) => n.keys().next_back().unwrap(),
            Part::Inner { parts, .. } => parts.keys().next_back().unwrap(),
        }
    }

    fn level(&self) -> u32 {
        match self {
            Part::Leaf(_) => 0,
            Part::Inner { level, .. } => *level,
        }
    }

    /// The chunking rule for a part at its place: every id but the last is
    /// no chunk end at this level, the last is one unless the part is the
    /// last of its level, and the part follows `after` (the last id of the
    /// part before it).
    fn check_place(&self, after: Option<&NodeId>, last_of_level: bool) -> std::result::Result<(), String> {
        let level = self.level();
        let ids: Vec<&NodeId> = match self {
            Part::Leaf(n) => n.keys().collect(),
            Part::Inner { parts, .. } => parts.keys().collect(),
        };
        if after.is_some_and(|a| ids[0] <= a) {
            return Err("parts out of order".into());
        }
        let (last, rest) = ids.split_last().unwrap();
        if rest.iter().any(|id| height(id) > level) || (!last_of_level && height(last) <= level) {
            return Err(format!("a part at level {level} is not chunked by node id"));
        }
        Ok(())
    }
}

/// Check the bytes of a `history` blob (the root only).
pub(crate) fn check_root(b: &[u8]) -> std::result::Result<(), String> {
    Root::decode(b).map(|_| ())
}

/// Check the bytes of a `history-part` blob on its own.
pub(crate) fn check_part(b: &[u8]) -> std::result::Result<(), String> {
    Part::decode(b).map(|_| ())
}

/// The views a part refers to: versions of a leaf's nodes, or child parts.
pub(crate) fn part_children(key: &str, b: &[u8]) -> Result<Vec<View>> {
    Ok(match Part::decode(b).map_err(|e| malformed(key, e))? {
        Part::Leaf(n) => n.keys().map(NodeId::version).collect(),
        Part::Inner { parts, .. } => parts.into_values().collect(),
    })
}

/// Split `items` (sorted by id) into chunks ending at ids higher than `level`.
fn chunk<T>(items: impl IntoIterator<Item = (NodeId, T)>, level: u32) -> Vec<Vec<(NodeId, T)>> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    for (id, t) in items {
        let end = height(&id) > level;
        cur.push((id, t));
        if end {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Parts being written.
struct Out {
    alg: &'static Algorithm,
    puts: Vec<Put>,
}

impl Out {
    fn part(&mut self, p: &Part) -> View {
        let bytes = p.encode();
        let key = self.alg.key(&bytes);
        self.puts.push(Put::blob(key.clone(), bytes));
        View { format: part_format().to_string(), key }
    }

    fn leaves(&mut self, entries: impl IntoIterator<Item = (NodeId, Entry)>) -> Vec<(NodeId, View)> {
        chunk(entries, 0)
            .into_iter()
            .map(|c| {
                let p = Part::Leaf(c.into_iter().collect());
                (p.last().clone(), self.part(&p))
            })
            .collect()
    }

    /// The parts above `list` (one level's parts, by last id), up to the top.
    fn upper(&mut self, mut list: Vec<(NodeId, View)>) -> Option<View> {
        let mut level = 1;
        loop {
            match list.len() {
                0 => return None,
                1 => return list.pop().map(|(_, v)| v),
                _ => {}
            }
            list = chunk(list, level)
                .into_iter()
                .map(|c| {
                    let p = Part::Inner { level, parts: c.into_iter().collect() };
                    (p.last().clone(), self.part(&p))
                })
                .collect();
            level += 1;
        }
    }

    /// Write the parts the store lacks, then the root.
    fn finish(self, store: &dyn Store, root: &Root) -> Result<View> {
        let keys: Vec<String> = self.puts.iter().map(|p| p.key.clone()).collect();
        let have = if keys.is_empty() { Vec::new() } else { store.has_many(&keys)? };
        let missing: Vec<Put> = self.puts.into_iter().zip(have).filter(|(_, h)| !h).map(|(p, _)| p).collect();
        for batch in missing.chunks(BATCH) {
            store.write_many(batch).map_err(|(_, e)| Error::Store(e))?;
        }
        let bytes = root.encode();
        let key = self.alg.key(&bytes);
        store.write(&key, &bytes, &Expect::Absent)?;
        Ok(View { format: root_format().to_string(), key })
    }
}

/// Write the whole of `h`.
pub fn write(store: &dyn Store, alg: &'static Algorithm, h: &History) -> Result<View> {
    let mut out = Out { alg, puts: Vec::new() };
    let g = generations(h);
    let leaves = out.leaves(
        h.nodes
            .iter()
            .map(|(id, n)| (id.clone(), Entry { node: n.clone(), generation: g.get(id).copied().unwrap_or(1) })),
    );
    let nodes = out.upper(leaves);
    let root = Root { doc: h.doc.clone(), tags: h.tags.clone(), tips: h.tips(), nodes };
    out.finish(store, &root)
}

fn read_parts(store: &dyn Store, views: &[&View]) -> Result<Vec<Part>> {
    let mut out = Vec::with_capacity(views.len());
    for batch in views.chunks(BATCH) {
        let keys: Vec<String> = batch.iter().map(|v| v.key().to_string()).collect();
        for (k, r) in keys.iter().zip(store.read_many(&keys)) {
            let bytes = r?.ok_or_else(|| Error::Missing(k.clone()))?;
            out.push(Part::decode(&bytes).map_err(|e| malformed(k, e))?);
        }
    }
    Ok(out)
}

type Leaf = Arc<BTreeMap<NodeId, Entry>>;

/// What [`Stored::changed`] finds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changed {
    pub old: BTreeMap<NodeId, HistNode>,
    pub added: BTreeMap<NodeId, HistNode>,
    pub same: Vec<NodeId>,
}

/// A stored history opened for reading single nodes and for small updates:
/// the root and the parts above the leaves are read, the leaves when asked.
pub struct Stored<'s> {
    store: &'s dyn Store,
    pub root: Root,
    /// The leaves in order, each by its last node id.
    leaves: Vec<(NodeId, View)>,
    cache: Mutex<HashMap<String, Leaf>>,
}

impl<'s> Stored<'s> {
    /// Open the history at `r` (`None`: the empty history).
    pub fn open(store: &'s dyn Store, r: Option<&View>) -> Result<Stored<'s>> {
        let mut s = Stored { store, root: Root::default(), leaves: Vec::new(), cache: Mutex::new(HashMap::new()) };
        let Some(r) = r else { return Ok(s) };
        if r.format() != root_format() {
            return Err(Error::UnknownFormat(format!("{} is not the history format", r.format())));
        }
        s.root = Root::decode(&read_blob(store, r.key())?).map_err(|e| malformed(r.key(), e))?;
        let Some(top) = s.root.nodes.clone() else { return Ok(s) };
        let part = read_parts(store, &[&top])?.pop().unwrap();
        part.check_place(None, true).map_err(|e| malformed(top.key(), e))?;
        if let Part::Inner { parts, .. } = &part
            && parts.len() < 2
        {
            return Err(malformed(top.key(), "the top part has one child"));
        }
        // One level at a time down to the parts right above the leaves.
        let mut level: Vec<(NodeId, View, Part)> = vec![(part.last().clone(), top, part)];
        loop {
            let l = level[0].2.level();
            if l == 0 {
                let (last, view, part) = level.pop().unwrap();
                let Part::Leaf(n) = part else { unreachable!() };
                s.cache.lock().unwrap().insert(view.key().to_string(), Arc::new(n));
                s.leaves = vec![(last, view)];
                return Ok(s);
            }
            let children: Vec<(NodeId, View)> = level
                .iter()
                .flat_map(|(_, _, p)| match p {
                    Part::Inner { parts, .. } => parts.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                    Part::Leaf(_) => Vec::new(),
                })
                .collect();
            if l == 1 {
                s.leaves = children;
                return Ok(s);
            }
            let views: Vec<&View> = children.iter().map(|(_, v)| v).collect();
            let parts = read_parts(store, &views)?;
            let n = children.len();
            let mut next = Vec::with_capacity(n);
            let mut prev: Option<NodeId> = None;
            for (i, ((key, view), part)) in children.into_iter().zip(parts).enumerate() {
                if part.level() != l - 1 || *part.last() != key {
                    return Err(malformed(view.key(), "a part is not where its parent says"));
                }
                part.check_place(prev.as_ref(), i + 1 == n).map_err(|e| malformed(view.key(), e))?;
                prev = Some(key.clone());
                next.push((key, view, part));
            }
            level = next;
        }
    }

    /// The number of leaves.
    pub fn leaf_count(&self) -> usize {
        self.leaves.len()
    }

    fn leaf(&self, i: usize) -> Result<Leaf> {
        let (key, view) = &self.leaves[i];
        if let Some(l) = self.cache.lock().unwrap().get(view.key()) {
            return Ok(l.clone());
        }
        let part = read_parts(self.store, &[view])?.pop().unwrap();
        self.place(i, key, view, part)
    }

    /// Check a leaf read for position `i` and cache it.
    fn place(&self, i: usize, key: &NodeId, view: &View, part: Part) -> Result<Leaf> {
        let prev = i.checked_sub(1).map(|j| &self.leaves[j].0);
        if part.level() != 0 || part.last() != key {
            return Err(malformed(view.key(), "a part is not where its parent says"));
        }
        part.check_place(prev, i + 1 == self.leaves.len()).map_err(|e| malformed(view.key(), e))?;
        let Part::Leaf(n) = part else { unreachable!() };
        let l = Arc::new(n);
        self.cache.lock().unwrap().insert(view.key().to_string(), l.clone());
        Ok(l)
    }

    /// The leaf that holds `id` if any node does.
    fn leaf_of(&self, id: &NodeId) -> Option<usize> {
        let i = self.leaves.partition_point(|(k, _)| k < id);
        (i < self.leaves.len()).then_some(i)
    }

    pub fn get(&self, id: &NodeId) -> Result<Option<HistNode>> {
        match self.leaf_of(id) {
            None => Ok(None),
            Some(i) => Ok(self.leaf(i)?.get(id).map(|e| e.node.clone())),
        }
    }

    /// A node with its generation.
    pub fn entry(&self, id: &NodeId) -> Result<Option<Entry>> {
        match self.leaf_of(id) {
            None => Ok(None),
            Some(i) => Ok(self.leaf(i)?.get(id).cloned()),
        }
    }

    /// The best common ancestors of `a` and `b` ([`History::merge_bases`]),
    /// reading only the nodes the walk reaches: git's paint-down by
    /// generation. Nodes are taken newest (greatest generation) first; each
    /// carries which of `a` and `b` reach it; one both reach is a base, and
    /// everything below it is stale. The walk ends when only stale nodes
    /// are left.
    pub fn merge_bases(&self, a: &NodeId, b: &NodeId) -> Result<BTreeSet<NodeId>> {
        const A: u8 = 1;
        const B: u8 = 2;
        const STALE: u8 = 4;
        let mut flags: HashMap<NodeId, u8> = HashMap::new();
        let mut queue = std::collections::BinaryHeap::new();
        let mut live = 0usize;
        let mut out = BTreeSet::new();
        for (id, f) in [(a, A), (b, B)] {
            let e = self.entry(id)?.ok_or_else(|| Error::Missing(format!("node {id}")))?;
            let old = flags.get(id).copied().unwrap_or(0);
            flags.insert(id.clone(), old | f);
            if old == 0 {
                queue.push((e.generation, id.clone()));
                live += 1;
            }
        }
        while live > 0 {
            let Some((_, id)) = queue.pop() else { break };
            let mut f = flags[&id];
            if f & STALE == 0 {
                live -= 1;
            }
            if f & (A | B) == A | B && f & STALE == 0 {
                out.insert(id.clone());
                f |= STALE;
                flags.insert(id.clone(), f);
            }
            let e = self.entry(&id)?.ok_or_else(|| Error::Missing(format!("node {id}")))?;
            for p in e.node.after.keys() {
                let old = flags.get(p).copied();
                let new = old.unwrap_or(0) | f;
                if old == Some(new) {
                    continue;
                }
                flags.insert(p.clone(), new);
                match old {
                    None => {
                        let g = self.entry(p)?.ok_or_else(|| Error::Missing(format!("node {p}")))?.generation;
                        queue.push((g, p.clone()));
                        if new & STALE == 0 {
                            live += 1;
                        }
                    }
                    // Queued and not yet taken (an ancestor is taken after
                    // every node above it): it may just have become stale.
                    Some(o) if o & STALE == 0 && new & STALE != 0 => live -= 1,
                    Some(_) => {}
                }
            }
        }
        Ok(out)
    }

    pub fn contains(&self, id: &NodeId) -> Result<bool> {
        Ok(self.get(id)?.is_some())
    }

    /// A fresh id for `version`, as [`History::new_id`].
    pub fn new_id(&self, version: &View) -> Result<NodeId> {
        let base = NodeId::of(version);
        if !self.contains(&base)? {
            return Ok(base);
        }
        for n in 2.. {
            let id = NodeId::parse(&format!("{base}#{n}")).unwrap();
            if !self.contains(&id)? {
                return Ok(id);
            }
        }
        unreachable!()
    }

    /// Every leaf's nodes, in order.
    fn all_leaves(&self) -> Result<Vec<Leaf>> {
        let cached: Vec<Option<Leaf>> = {
            let c = self.cache.lock().unwrap();
            self.leaves.iter().map(|(_, v)| c.get(v.key()).cloned()).collect()
        };
        let todo: Vec<usize> = (0..self.leaves.len()).filter(|&i| cached[i].is_none()).collect();
        let views: Vec<&View> = todo.iter().map(|&i| &self.leaves[i].1).collect();
        let parts = read_parts(self.store, &views)?;
        let mut out = cached;
        for (i, part) in todo.into_iter().zip(parts) {
            let (key, view) = &self.leaves[i];
            out[i] = Some(self.place(i, key, view, part)?);
        }
        Ok(out.into_iter().map(Option::unwrap).collect())
    }

    /// The whole history, checked: every part in place, and the root's tips
    /// the history's tips.
    pub fn read_all(&self) -> Result<History> {
        self.read_all_with_log().map(|(h, _)| h)
    }

    /// [`Stored::read_all`] and the history's log, computed by the check.
    pub fn read_all_with_log(&self) -> Result<(History, Vec<NodeId>)> {
        let mut h = History { doc: self.root.doc.clone(), tags: self.root.tags.clone(), ..History::default() };
        for l in self.all_leaves()? {
            h.nodes.extend(l.iter().map(|(k, e)| (k.clone(), e.node.clone())));
        }
        let key = self.root.nodes.as_ref().map_or("", |v| v.key());
        let log = h.checked_log().map_err(|e| malformed(key, e))?;
        // Every stored generation is right.
        let mut generation: HashMap<&NodeId, u64> = HashMap::with_capacity(h.nodes.len());
        for l in self.all_leaves()? {
            for (k, e) in l.iter() {
                generation.insert(h.nodes.get_key_value(k).unwrap().0, e.generation);
            }
        }
        for (id, n) in &h.nodes {
            let want = 1 + n.after.keys().map(|p| generation[p]).max().unwrap_or(0);
            if generation[id] != want {
                return Err(malformed(key, format!("{id} has generation {}, not {want}", generation[id])));
            }
        }
        // The tips are the nodes no other follows; the log puts them first.
        let tips_ok = {
            let mut followed = std::collections::HashSet::new();
            for n in h.nodes.values() {
                followed.extend(n.after.keys());
            }
            let tips: BTreeSet<&NodeId> = h.nodes.keys().filter(|id| !followed.contains(id)).collect();
            tips.len() == self.root.tips.len() && tips.into_iter().eq(self.root.tips.iter())
        };
        if !tips_ok {
            return Err(malformed(key, "the root's tips are not the history's tips"));
        }
        Ok((h, log))
    }

    /// Write a new history: this one with `changes` (a node, or `None` to
    /// remove it) and the doc, tags and tips of `root`. Only the leaves
    /// that change are read and rewritten, and the parts above them.
    pub fn update(
        &self,
        alg: &'static Algorithm,
        mut root: Root,
        changes: &BTreeMap<NodeId, Option<HistNode>>,
    ) -> Result<View> {
        let mut out = Out { alg, puts: Vec::new() };
        let n = self.leaves.len();
        // Generations of the changed nodes, from their predecessors: other
        // changed nodes first (in an order where each follows those it is
        // after), then stored ones. A changed node must not change the
        // generation of nodes that follow it; a whole read checks that.
        let mut gens: HashMap<&NodeId, u64> = HashMap::new();
        let mut todo: Vec<(&NodeId, &HistNode)> =
            changes.iter().filter_map(|(id, c)| c.as_ref().map(|n| (id, n))).collect();
        while !todo.is_empty() {
            let before = todo.len();
            let mut later = Vec::new();
            for (id, node) in todo {
                let mut g = 0;
                let mut ready = true;
                for p in node.after.keys() {
                    if let Some(&x) = gens.get(p) {
                        g = g.max(x);
                    } else if changes.get(p).is_some_and(Option::is_some) && p != id {
                        ready = false;
                    } else if let Some(e) = self.entry(p)? {
                        g = g.max(e.generation);
                    }
                }
                if ready {
                    gens.insert(id, g + 1);
                } else {
                    later.push((id, node));
                }
            }
            if later.len() == before {
                // A cycle among the changes: an invalid history either way.
                for (id, _) in &later {
                    gens.insert(id, 1);
                }
                break;
            }
            todo = later;
        }
        let changes: BTreeMap<&NodeId, Option<Entry>> = changes
            .iter()
            .map(|(id, c)| (id, c.as_ref().map(|node| Entry { node: node.clone(), generation: gens[id] })))
            .collect();
        let mut by_leaf: BTreeMap<usize, Vec<(&NodeId, &Option<Entry>)>> = BTreeMap::new();
        let mut fresh = BTreeMap::new();
        for (id, c) in &changes {
            let id: &NodeId = id;
            if n == 0 {
                if let Some(e) = c {
                    fresh.insert(id.clone(), e.clone());
                }
            } else {
                by_leaf.entry(self.leaf_of(id).unwrap_or(n - 1)).or_default().push((id, c));
            }
        }
        let mut leaves = if n == 0 { out.leaves(fresh) } else { Vec::new() };
        let mut i = 0;
        while i < n {
            if !by_leaf.contains_key(&i) {
                leaves.push(self.leaves[i].clone());
                i += 1;
                continue;
            }
            // A run of leaves to rechunk: from this one up to one that still
            // ends at a chunk end (or the last).
            let mut entries: BTreeMap<NodeId, Entry> = BTreeMap::new();
            let mut end = i;
            loop {
                entries.extend(self.leaf(end)?.iter().map(|(k, v)| (k.clone(), v.clone())));
                for (id, c) in by_leaf.get(&end).into_iter().flatten() {
                    match c {
                        Some(node) => entries.insert((*id).clone(), node.clone()),
                        None => entries.remove(*id),
                    };
                }
                let ends = entries.keys().next_back().is_some_and(|k| height(k) > 0);
                if ends || end + 1 == n {
                    break;
                }
                end += 1;
            }
            leaves.extend(out.leaves(entries));
            i = end + 1;
        }
        root.nodes = out.upper(leaves);
        out.finish(self.store, &root)
    }

    /// The nodes that differ between `self` and `new`: those of `self` not
    /// in `new` as they are, those of `new` not in `self` as they are, and
    /// the nodes the differing leaves share unchanged. Only the leaves that
    /// differ are read.
    pub fn changed(&self, new: &Stored) -> Result<Changed> {
        let mine: BTreeSet<&str> = self.leaves.iter().map(|(_, v)| v.key()).collect();
        let theirs: BTreeSet<&str> = new.leaves.iter().map(|(_, v)| v.key()).collect();
        let collect = |s: &Stored, other: &BTreeSet<&str>| -> Result<BTreeMap<NodeId, HistNode>> {
            let mut out = BTreeMap::new();
            for (i, (_, v)) in s.leaves.iter().enumerate() {
                if !other.contains(v.key()) {
                    out.extend(s.leaf(i)?.iter().map(|(k, e)| (k.clone(), e.node.clone())));
                }
            }
            Ok(out)
        };
        let mut old = collect(self, &theirs)?;
        let mut added = collect(new, &mine)?;
        let same: Vec<NodeId> = old.iter().filter(|(k, v)| added.get(*k) == Some(*v)).map(|(k, _)| k.clone()).collect();
        for k in &same {
            old.remove(k);
            added.remove(k);
        }
        Ok(Changed { old, added, same })
    }
}
