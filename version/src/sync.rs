//! Sync (Stage 7, spec/sync.md): two stores converge with plain core ops.
//! Tooling walks formats from a view and writes what the other store
//! lacks. The walk goes breadth first, one batch per depth: a round asks
//! the destination which keys it has and reads the rest from the source,
//! so the round trips follow the depth of the trees, not their size.
//!
//! Copies keep the closure invariant: a blob is written only after
//! everything it refers to, so a store that has a blob has all of it, and
//! a walk can stop at the first blob the destination already holds.

use crate::dir::Item;
use crate::formats::{self, Kind};
use crate::history;
use crate::{Error, Result, View, is_blob_key, read_blob, tree};
use cas_core::store::Put;
use cas_core::{Error as CoreError, Expect, Span, Store};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Whether `store` holds the blob `key`, without reading it.
pub fn has(store: &dyn Store, key: &str) -> Result<bool> {
    let mut end = key.as_bytes().to_vec();
    end.push(0);
    let page = store.list(&Span { from: Some(key.as_bytes().to_vec()), to: Some(end) })?;
    Ok(page.keys.iter().any(|k| k == key))
}

/// What a copy did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Copied {
    /// Blobs written to the destination.
    pub blobs: usize,
    /// Blobs skipped because the destination already had them.
    pub present: usize,
}

/// The views inside a blob, by its format. Unknown formats are
/// opaque, so their blobs are copied without following anything.
pub(crate) fn children(store: &dyn Store, r: &View) -> Result<Vec<View>> {
    match r.known_format().map(|f| (&f.kind, f.alias)) {
        Some((Kind::Dir(_), _)) => Ok(tree::load(store, r)?.into_iter().map(|Item { child, .. }| child).collect()),
        Some((_, "history")) => Ok(history::read(store, r)?.nodes.keys().map(|id| id.version()).collect()),
        _ => Ok(Vec::new()),
    }
}

/// Copy one raw blob unless the destination has it.
fn copy_blob(src: &dyn Store, dst: &dyn Store, key: &str, n: &mut Copied) -> Result<()> {
    if has(dst, key)? {
        n.present += 1;
    } else {
        dst.write(key, &read_blob(src, key)?, &Expect::Absent)?;
        n.blobs += 1;
    }
    Ok(())
}

/// Copy the format object and spec of `format`, so the destination can
/// describe what it holds. Formats this crate does not know are skipped.
fn copy_format(src: &dyn Store, dst: &dyn Store, format: &str, n: &mut Copied) -> Result<()> {
    if let Some(f) = formats::by_id(format) {
        for bytes in [f.spec.as_bytes(), f.object().as_bytes()] {
            let key = cas_core::algo::lookup("blake3").unwrap().key(bytes);
            if has(dst, &key)? {
                n.present += 1;
            } else {
                dst.write(&key, bytes, &Expect::Absent)?;
                n.blobs += 1;
            }
        }
    } else if is_blob_key(format) && has(src, format)? {
        copy_blob(src, dst, format, n)?;
    }
    Ok(())
}

/// The views inside `bytes`, the value of `r`, if its format holds any.
/// `None` for formats that hold no views (files, unknown formats).
fn children_of(r: &View, bytes: &[u8]) -> Result<Option<Vec<View>>> {
    match r.known_format().map(|f| (&f.kind, f.alias)) {
        Some((Kind::Dir(d), _)) => {
            Ok(Some(d.decode(r.key(), bytes)?.into_iter().map(|Item { child, .. }| child).collect()))
        }
        Some((_, "history")) => Ok(Some(history::parse(r.key(), bytes)?.nodes.keys().map(|id| id.version()).collect())),
        _ => Ok(None),
    }
}

/// Copy everything reachable from `root` that `dst` lacks, children before
/// parents (the closure invariant). Breadth first: each depth is one
/// `has_many` on `dst` and one `read_many` on `src`. A blob is written as
/// soon as everything under it is in the destination, so only containers
/// still waiting on their subtrees are held in memory. A history's nodes
/// are copied in groups of [`ROOTS_PER_ROUND`], oldest first, which bounds
/// that to a group's subtrees: later groups find most of their trees
/// present already.
pub fn copy(src: &dyn Store, dst: &dyn Store, root: &View) -> Result<Copied> {
    let mut c = Copier { src, dst, n: Copied::default(), formats_seen: BTreeSet::new(), done: HashSet::new() };
    let is_history = root.known_format().is_some_and(|f| f.alias == "history");
    if is_history && !dst.has_many(&[root.key().to_string()])?[0] {
        let h = history::read(src, root)?;
        let mut seen = HashSet::new();
        let versions: Vec<View> =
            h.log().into_iter().rev().map(|id| id.version()).filter(|v| seen.insert(v.key().to_string())).collect();
        // One batched check for all of them first: a push of one commit
        // finds every version but the new one present.
        let keys: Vec<String> = versions.iter().map(|v| v.key().to_string()).collect();
        let present = dst.has_many(&keys)?;
        let mut missing = Vec::new();
        for (v, p) in versions.into_iter().zip(present) {
            if p {
                c.n.present += 1;
                c.done.insert(v.key().to_string());
            } else {
                missing.push(v);
            }
        }
        for group in missing.chunks(ROOTS_PER_ROUND) {
            c.copy_many(group)?;
        }
    }
    c.copy_many(std::slice::from_ref(root))?;
    Ok(c.n)
}

/// Node versions copied together when a history is copied.
pub const ROOTS_PER_ROUND: usize = 256;

struct Copier<'a> {
    src: &'a dyn Store,
    dst: &'a dyn Store,
    n: Copied,
    formats_seen: BTreeSet<String>,
    /// Keys known to be in the destination, complete.
    done: HashSet<String>,
}

/// A container read from the source, waiting for its children.
struct Waiting {
    bytes: Vec<u8>,
    /// Children not yet known to be in the destination.
    left: usize,
}

impl Copier<'_> {
    fn copy_many(&mut self, roots: &[View]) -> Result<()> {
        let (src, dst) = (self.src, self.dst);
        let mut waiting: HashMap<String, Waiting> = HashMap::new();
        // Child key to the containers waiting on it.
        let mut parents: HashMap<String, Vec<String>> = HashMap::new();
        let mut frontier: Vec<View> = roots.to_vec();
        let mut queued: HashSet<String> = HashSet::new();
        while !frontier.is_empty() {
            let mut level: Vec<View> = Vec::new();
            for r in frontier.drain(..) {
                if self.formats_seen.insert(r.format().to_string()) {
                    copy_format(src, dst, r.format(), &mut self.n)?;
                }
                if !self.done.contains(r.key()) && queued.insert(r.key().to_string()) {
                    level.push(r);
                }
            }
            let keys: Vec<String> = level.iter().map(|r| r.key().to_string()).collect();
            let present = dst.has_many(&keys)?;
            let mut resolved: Vec<String> = Vec::new();
            let mut want = Vec::new();
            for (r, p) in level.into_iter().zip(present) {
                if p {
                    self.n.present += 1;
                    resolved.push(r.key().to_string());
                } else {
                    want.push(r);
                }
            }
            let keys: Vec<String> = want.iter().map(|r| r.key().to_string()).collect();
            let mut ready: Vec<Put> = Vec::new();
            for (r, value) in want.into_iter().zip(src.read_many(&keys)) {
                let bytes = value?.ok_or_else(|| Error::Missing(r.key().to_string()))?;
                let Some(kids) = children_of(&r, &bytes)? else {
                    ready.push(Put::blob(r.key(), bytes));
                    continue;
                };
                let kids: BTreeSet<View> = kids.into_iter().collect();
                let mut left = 0;
                for k in kids {
                    if self.done.contains(k.key()) {
                        continue;
                    }
                    left += 1;
                    parents.entry(k.key().to_string()).or_default().push(r.key().to_string());
                    if !queued.contains(k.key()) {
                        frontier.push(k);
                    }
                }
                if left == 0 {
                    ready.push(Put::blob(r.key(), bytes));
                } else {
                    waiting.insert(r.key().to_string(), Waiting { bytes, left });
                }
            }
            // Write what is ready; each write may free the containers above it.
            loop {
                self.n.blobs += ready.len();
                dst.write_many(&ready).map_err(|(_, e)| Error::from(e))?;
                resolved.extend(ready.drain(..).map(|p| p.key));
                for k in resolved.drain(..) {
                    for parent in parents.remove(&k).unwrap_or_default() {
                        let w = waiting.get_mut(&parent).expect("a waiting parent");
                        w.left -= 1;
                        if w.left == 0 {
                            let w = waiting.remove(&parent).expect("waiting");
                            ready.push(Put::blob(parent, w.bytes));
                        }
                    }
                    self.done.insert(k);
                }
                if ready.is_empty() {
                    break;
                }
            }
        }
        debug_assert!(waiting.is_empty(), "every container was written");
        Ok(())
    }
}

/// How a history name was synced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The destination already held the same history.
    UpToDate,
    /// The destination had no such name; it now holds the source's history.
    Created,
    /// The source's history extends the destination's; the name moved.
    FastForward,
}

/// Make `to` in `dst` hold the history `from` names in `src`, copying what
/// is needed. Fast-forward only: the name moves when the destination has
/// none or the new history extends its own. Otherwise `Diverged`; rebase
/// or merge is the caller's choice.
pub fn push(src: &dyn Store, dst: &dyn Store, from: &str, to: &str) -> Result<(Outcome, Copied)> {
    let head = history::head(src, from)?.ok_or_else(|| Error::Missing(format!("history {from} in the source")))?;
    let theirs = history::head(dst, to)?;
    if theirs.as_ref() == Some(&head) {
        return Ok((Outcome::UpToDate, Copied::default()));
    }
    if let Some(t) = &theirs
        && !history::read(src, &head)?.extends(&history::read(dst, t)?)
    {
        return Err(Error::Diverged(to.to_string()));
    }
    let copied = copy(src, dst, &head)?;
    let expect = match &theirs {
        Some(t) => Expect::Value(t.to_string().into_bytes()),
        None => Expect::Absent,
    };
    match dst.write(to, head.to_string().as_bytes(), &expect) {
        Ok(()) => Ok((if theirs.is_some() { Outcome::FastForward } else { Outcome::Created }, copied)),
        // The destination moved while we copied; the copy is still valid.
        Err(CoreError::Conflict) => Err(Error::Diverged(to.to_string())),
        Err(e) => Err(e.into()),
    }
}
