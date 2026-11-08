//! The history graph (spec/functions/graph.md): nodes in log order with
//! predecessors by position, notes, and changed-path Bloom filters. Built
//! once per history and cached as derived data; a history that extends
//! another reuses its records.

use crate::derived::{self, Function};
use crate::history::{History, NodeId};
use crate::{Path, Result, View, diff, malformed, object_at, read_blob};
use cas_core::Store;
use cas_core::algo::Algorithm;
use std::collections::{BTreeSet, HashMap};

const MAGIC: &[u8; 9] = b"CASGRAPH\x01";
/// Steps that change more paths get no filter. (git stops at 512; a
/// larger cap keeps big merges filtered, at a few kilobytes each.)
pub const MAX_PATHS: usize = 8192;
const COMPATIBLE: u32 = 1 << 31;

/// A step's changed-path filter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Filter {
    /// The node has no predecessor.
    Root,
    Bloom(Vec<u8>),
    /// Too many changed paths: anything may have changed.
    Full,
}

fn bits(path: &str, m: usize) -> [usize; 7] {
    let h = cas_core::algo::lookup("blake3").unwrap().hash(path.as_bytes());
    std::array::from_fn(|i| u32::from_le_bytes(h[4 * i..4 * i + 4].try_into().unwrap()) as usize % (m * 8))
}

impl Filter {
    /// The filter of the paths `changed` (parents are added here).
    pub fn of<'a>(changed: impl IntoIterator<Item = &'a Path>) -> Filter {
        let mut all: BTreeSet<&str> = BTreeSet::new();
        for p in changed {
            let mut s = p.as_str();
            loop {
                if !all.insert(s) {
                    break;
                }
                match s.rsplit_once('/') {
                    Some((parent, _)) => s = parent,
                    None => break,
                }
            }
            if all.len() > MAX_PATHS {
                return Filter::Full;
            }
        }
        let m = (10 * all.len()).div_ceil(8).max(8);
        let mut f = vec![0u8; m];
        for p in all {
            for b in bits(p, m) {
                f[b / 8] |= 1 << (b % 8);
            }
        }
        Filter::Bloom(f)
    }

    /// False only when `path` certainly did not change in the step.
    pub fn may_contain(&self, path: &Path) -> bool {
        match self {
            Filter::Root | Filter::Full => true,
            Filter::Bloom(f) => bits(path.as_str(), f.len()).iter().all(|&b| f[b / 8] & (1 << (b % 8)) != 0),
        }
    }
}

/// One node's record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub id: NodeId,
    /// Positions of the predecessors, with whether each step is compatible.
    pub after: Vec<(usize, bool)>,
    pub notes: String,
    pub filter: Filter,
}

/// A history's graph: records in log order, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Graph {
    pub nodes: Vec<Record>,
}

fn notes(h: &History, id: &NodeId) -> String {
    let n: Vec<&str> = h.nodes[id].after.values().filter_map(|s| s.changelog.as_deref()).collect();
    n.join(" / ")
}

impl Graph {
    /// The graph of `h`, taking filters from `reuse` (by node id) where it
    /// has them and diffing the rest, on up to `threads` threads.
    pub fn build(store: &dyn Store, h: &History, reuse: Option<&Graph>, threads: usize) -> Result<Graph> {
        let log = h.log();
        let pos: HashMap<&NodeId, usize> = log.iter().enumerate().map(|(i, id)| (id, i)).collect();
        let old: HashMap<&NodeId, &Filter> =
            reuse.map(|g| g.nodes.iter().map(|r| (&r.id, &r.filter)).collect()).unwrap_or_default();
        let mut filters: Vec<Option<Filter>> = log.iter().map(|id| old.get(id).map(|f| (*f).clone())).collect();
        let todo: Vec<usize> = (0..log.len()).filter(|&i| filters[i].is_none()).collect();
        let step = |i: usize| -> Result<Filter> {
            let id = &log[i];
            match h.nodes[id].after.keys().next() {
                None => Ok(Filter::Root),
                Some(p) => {
                    let changes = diff(store, &p.version(), &id.version(), None)?;
                    Ok(Filter::of(changes.iter().map(|c| &c.path)))
                }
            }
        };
        let done: Vec<(usize, Result<Filter>)> = if todo.len() < 64 || threads <= 1 {
            todo.iter().map(|&i| (i, step(i))).collect()
        } else {
            let next = std::sync::atomic::AtomicUsize::new(0);
            let (todo, step, next) = (&todo, &step, &next);
            std::thread::scope(|s| {
                let workers: Vec<_> = (0..threads)
                    .map(|_| {
                        s.spawn(move || {
                            let mut out = Vec::new();
                            loop {
                                let k = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                let Some(&i) = todo.get(k) else { return out };
                                out.push((i, step(i)));
                            }
                        })
                    })
                    .collect();
                workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
            })
        };
        for (i, f) in done {
            filters[i] = Some(f?);
        }
        let nodes = log
            .iter()
            .zip(filters)
            .map(|(id, f)| Record {
                id: id.clone(),
                after: h.nodes[id].after.iter().map(|(p, s)| (pos[p], s.compatible)).collect(),
                notes: notes(h, id),
                filter: f.unwrap(),
            })
            .collect();
        Ok(Graph { nodes })
    }

    /// The graph of `h` without diffing: every step's filter is [`Filter::Full`].
    /// For order and notes only; never recorded.
    pub fn unfiltered(h: &History) -> Graph {
        let log = h.log();
        let pos: HashMap<&NodeId, usize> = log.iter().enumerate().map(|(i, id)| (id, i)).collect();
        let nodes = log
            .iter()
            .map(|id| Record {
                id: id.clone(),
                after: h.nodes[id].after.iter().map(|(p, s)| (pos[p], s.compatible)).collect(),
                notes: notes(h, id),
                filter: if h.nodes[id].after.is_empty() { Filter::Root } else { Filter::Full },
            })
            .collect();
        Graph { nodes }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(&(self.nodes.len() as u32).to_be_bytes());
        for r in &self.nodes {
            out.extend_from_slice(&(r.id.as_str().len() as u16).to_be_bytes());
            out.extend_from_slice(r.id.as_str().as_bytes());
            out.push(r.after.len() as u8);
            for &(p, c) in &r.after {
                out.extend_from_slice(&(p as u32 | if c { COMPATIBLE } else { 0 }).to_be_bytes());
            }
            out.extend_from_slice(&(r.notes.len() as u32).to_be_bytes());
            out.extend_from_slice(r.notes.as_bytes());
            match &r.filter {
                Filter::Root => out.push(0),
                Filter::Bloom(f) => {
                    out.push(1);
                    out.extend_from_slice(&(f.len() as u32).to_be_bytes());
                    out.extend_from_slice(f);
                }
                Filter::Full => out.push(2),
            }
        }
        out
    }

    pub fn decode(b: &[u8]) -> std::result::Result<Graph, String> {
        let mut at = 0;
        let mut take = |n: usize| -> std::result::Result<&[u8], String> {
            let s = b.get(at..at + n).ok_or("graph truncated")?;
            at += n;
            Ok(s)
        };
        if take(9)? != MAGIC {
            return Err("not a graph".into());
        }
        let n = u32::from_be_bytes(take(4)?.try_into().unwrap()) as usize;
        let mut nodes = Vec::with_capacity(n.min(b.len()));
        for _ in 0..n {
            let len = u16::from_be_bytes(take(2)?.try_into().unwrap()) as usize;
            let id = NodeId::parse(std::str::from_utf8(take(len)?).map_err(|_| "id not UTF-8")?)?;
            let k = take(1)?[0] as usize;
            let mut after = Vec::with_capacity(k);
            for _ in 0..k {
                let p = u32::from_be_bytes(take(4)?.try_into().unwrap());
                let i = (p & !COMPATIBLE) as usize;
                if i >= n {
                    return Err("predecessor out of range".into());
                }
                after.push((i, p & COMPATIBLE != 0));
            }
            let len = u32::from_be_bytes(take(4)?.try_into().unwrap()) as usize;
            let notes = String::from_utf8(take(len)?.to_vec()).map_err(|_| "notes not UTF-8")?;
            let filter = match take(1)?[0] {
                0 => Filter::Root,
                1 => {
                    let m = u32::from_be_bytes(take(4)?.try_into().unwrap()) as usize;
                    if m == 0 {
                        return Err("empty filter".into());
                    }
                    Filter::Bloom(take(m)?.to_vec())
                }
                2 => Filter::Full,
                t => return Err(format!("filter kind {t}")),
            };
            nodes.push(Record { id, after, notes, filter });
        }
        if at != b.len() {
            return Err("trailing bytes".into());
        }
        Ok(Graph { nodes })
    }

    /// The newest node whose step changed `path`: git's `last-modified`.
    pub fn last_change(&self, store: &dyn Store, path: &Path) -> Result<Option<usize>> {
        self.changes(store, path, true).map(|v| v.first().copied())
    }

    /// The nodes whose step changed `path` (or anything under it), newest
    /// first: the same answer as [`derived::path_log`], reading the trees
    /// only of nodes whose filter may contain `path`.
    pub fn path_log(&self, store: &dyn Store, path: &Path) -> Result<Vec<usize>> {
        self.changes(store, path, false)
    }

    fn changes(&self, store: &dyn Store, path: &Path, first_only: bool) -> Result<Vec<usize>> {
        // Each version is looked at as a node and again as its successor's
        // predecessor; remember the answers by version key.
        let memo = std::cell::RefCell::new(HashMap::new());
        let at = |i: usize| -> Result<Option<View>> {
            let v = self.nodes[i].id.version();
            if let Some(n) = memo.borrow().get(v.key()) {
                return Ok(Clone::clone(n));
            }
            let n = object_at(store, &v, path)?;
            memo.borrow_mut().insert(v.key().to_string(), n.clone());
            Ok(n)
        };
        let mut out = Vec::new();
        for (i, r) in self.nodes.iter().enumerate() {
            if !r.filter.may_contain(path) {
                continue;
            }
            let here = at(i)?;
            let hit = if r.after.is_empty() {
                here.is_some()
            } else {
                let mut changed = true;
                for &(p, _) in &r.after {
                    if at(p)? == here {
                        changed = false;
                        break;
                    }
                }
                changed
            };
            if hit {
                out.push(i);
                if first_only {
                    break;
                }
            }
        }
        Ok(out)
    }
}

fn compute(store: &dyn Store, _: &'static Algorithm, input: &View) -> Result<Vec<u8>> {
    let h = crate::history::read(store, input)?;
    Ok(Graph::build(store, &h, None, threads())?.encode())
}

fn threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get()).min(8)
}

pub static GRAPH: Function = Function {
    name: "graph",
    input: "history",
    output: "raw",
    spec: include_str!("../../spec/functions/graph.md"),
    compute,
};

fn read(store: &dyn Store, r: &View) -> Result<Graph> {
    Graph::decode(&read_blob(store, r.key())?).map_err(|e| malformed(r.key(), e))
}

/// The variable naming the history `input` replaced when it was saved, so
/// a graph can be built from the replaced history's graph. A variable holds
/// a view, so it holds a `raw` blob whose text is that history's key: the
/// note keeps the old history from being reachable (spec/gc.md).
fn hint_var(input: &View) -> String {
    format!("derived-hint/{}/{}", GRAPH.id(), input.key())
}

/// The history `input` replaced when it was saved, if that was noted.
pub fn replaced(store: &dyn Store, input: &View) -> Option<View> {
    let note = store.read(&hint_var(input)).ok()??;
    let note = View::parse(std::str::from_utf8(&note).ok()?).ok()?;
    let prev = read_blob(store, note.key()).ok()?;
    let key = std::str::from_utf8(&prev).ok()?;
    crate::is_blob_key(key).then(|| View { format: input.format().to_string(), key: key.to_string() })
}

/// The newest graph along the saved-over chain from `input`, within
/// `limit` steps.
fn reusable(store: &dyn Store, input: &View, limit: usize) -> Option<Graph> {
    let mut at = input.clone();
    for _ in 0..limit {
        at = replaced(store, &at)?;
        if let Ok(Some(r)) = derived::cached(store, &GRAPH, &at) {
            return read(store, &r).ok();
        }
    }
    None
}

/// The graph of the history at `input` if one is cached, else its order
/// and notes from the history itself (no filters, nothing diffed).
pub fn graph_or_order(store: &dyn Store, input: &View) -> Result<Graph> {
    match derived::readable_cache(store, &GRAPH, input)? {
        Some(r) => read(store, &r),
        None => Ok(Graph::unfiltered(&crate::history::read(store, input)?)),
    }
}

/// The graph of the history at `input`: cached, or built (from the graph
/// of a history it replaced, when one is cached) and recorded when the
/// store allows.
pub fn graph(store: &dyn Store, alg: &'static Algorithm, input: &View) -> Result<Graph> {
    if let Some(r) = derived::readable_cache(store, &GRAPH, input)? {
        return read(store, &r);
    }
    let h = crate::history::read(store, input)?;
    let g = Graph::build(store, &h, reusable(store, input, 1000).as_ref(), threads())?;
    derived::record(store, alg, &GRAPH, input, &g.encode());
    Ok(g)
}

/// After a history was saved at `new`, replacing `old`: note `old`, so the
/// next graph of `new` starts from `old`'s instead of from nothing. A save
/// stays cheap; the diffs happen when a graph is asked for. Best effort.
pub fn record_after_save(store: &dyn Store, alg: &'static Algorithm, old: &View, new: &View) {
    if old.key() == new.key() {
        return;
    }
    let Ok(key) = crate::write_blob(store, alg, old.key().as_bytes()) else { return };
    let note = View { format: crate::by_alias("raw").unwrap().id().to_string(), key };
    let _ = store.write(&hint_var(new), note.to_string().as_bytes(), &cas_core::Expect::Absent);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Path {
        Path::new(s).unwrap()
    }

    #[test]
    fn a_filter_holds_changed_paths_and_their_parents() {
        let f = Filter::of([&p("a/b/c"), &p("d")]);
        for q in ["a/b/c", "a/b", "a", "d"] {
            assert!(f.may_contain(&p(q)), "{q}");
        }
        // No false positives among these few (a 1% rate makes 0 of 5 likely;
        // the bits are fixed, so this does not flake).
        let misses = ["x", "a/c", "a/b/d", "e", "d/e"].iter().filter(|q| f.may_contain(&p(q))).count();
        assert_eq!(misses, 0);
        let many: Vec<Path> = (0..=MAX_PATHS).map(|i| p(&format!("f{i}"))).collect();
        assert_eq!(Filter::of(&many), Filter::Full);
    }

    #[test]
    fn graphs_round_trip() {
        let id = |k: &str| {
            NodeId::of(&View { format: format!("blake3:{}", "0".repeat(64)), key: format!("blake3:{k:0>64}") })
        };
        let g = Graph {
            nodes: vec![
                Record { id: id("02"), after: vec![(1, true)], notes: "two".into(), filter: Filter::of([&p("x")]) },
                Record { id: id("01"), after: vec![], notes: String::new(), filter: Filter::Root },
            ],
        };
        assert_eq!(Graph::decode(&g.encode()).unwrap(), g);
        let mut bad = g.encode();
        bad.truncate(bad.len() - 1);
        assert!(Graph::decode(&bad).is_err());
    }
}
