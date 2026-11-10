//! Sync (Stage 7, spec/sync.md): two stores converge with plain core ops.
//! Tooling walks formats from a view and writes what the other store
//! lacks. The walk goes breadth first, one batch per depth: a round asks
//! the destination which keys it has and reads the rest from the source,
//! so the round trips follow the depth of the trees, not their size.
//!
//! Copies keep the closure invariant: a blob is written only after
//! everything it refers to, so a store that has a blob has all of it, and
//! a walk can stop at the first blob the destination already holds.

use crate::formats::{self, Kind};
use crate::history;
use crate::{Error, Result, View, is_blob_key, read_blob};
use cas_core::keyhash::DigestSet;
use cas_core::keyhash::{KeyMap, KeySet};
use cas_core::store::Put;
use cas_core::{Error as CoreError, Expect, Form, Span, Store, Stored};
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
        Some((Kind::Dir(d), _)) => d.blob_children(r.key(), &read_blob(store, r.key())?),
        Some((_, "history" | "history-part")) => Ok(children_of(r, &read_blob(store, r.key())?)?.unwrap_or_default()),
        Some((_, a @ ("annotated" | "diff"))) => Ok(crate::annotated::children(a, &read_blob(store, r.key())?)),
        Some((_, "step-notes")) => Ok(crate::stepnotes::children(&read_blob(store, r.key())?)),
        _ => Ok(Vec::new()),
    }
}

/// [`children`] without checking a directory blob's encoding, and without
/// those `skip` says to leave out: for gc.
pub(crate) fn children_light(store: &dyn Store, r: &View, skip: &DigestSet) -> Result<Vec<View>> {
    match r.known_format().map(|f| &f.kind) {
        Some(Kind::Dir(d)) => d.blob_children_light(r.key(), &read_blob(store, r.key())?, skip),
        _ => Ok(children(store, r)?.into_iter().filter(|v| !skip.contains(v.key())).collect()),
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
        let (spec, object) = f.described_by(format);
        for bytes in [spec.as_bytes(), object.as_bytes()] {
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
pub(crate) fn children_of(r: &View, bytes: &[u8]) -> Result<Option<Vec<View>>> {
    match r.known_format().map(|f| &f.kind) {
        Some(Kind::Dir(d)) => Ok(Some(d.blob_children(r.key(), bytes)?)),
        _ => children_of_light(r, bytes, &DigestSet::default()),
    }
}

/// [`children_of`] leaving out the views `skip` names; a directory blob is
/// read in one pass that checks bounds, not its canonical form.
fn children_of_light(r: &View, bytes: &[u8], skip: &DigestSet) -> Result<Option<Vec<View>>> {
    match r.known_format().map(|f| (&f.kind, f.alias)) {
        Some((Kind::Dir(d), _)) => Ok(Some(d.blob_children_light(r.key(), bytes, skip)?)),
        Some((_, "history")) => {
            let root = crate::htree::Root::decode(bytes).map_err(|e| crate::malformed(r.key(), e))?;
            Ok(Some(root.nodes.into_iter().collect()))
        }
        Some((_, "history-part")) => Ok(Some(crate::htree::part_children(r.key(), bytes)?)),
        Some((_, a @ ("annotated" | "diff"))) => Ok(Some(crate::annotated::children(a, bytes))),
        Some((_, "step-notes")) => Ok(Some(crate::stepnotes::children(bytes))),
        _ => Ok(None),
    }
}

fn wrong_bytes(key: &str) -> Error {
    Error::Store(CoreError::Unavailable(format!("the source sent wrong bytes for {key}")))
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
    let is_history = root.known_format().is_some_and(|f| f.alias == "history");
    if is_history && !dst.has_many(&[root.key().to_string()])?[0] {
        let h = history::read(src, root)?;
        let versions = h.log().into_iter().rev().map(|id| id.version()).collect();
        return copy_history(src, dst, root, versions, HashSet::new());
    }
    std::thread::scope(|sc| Copier::new(src, dst, HashSet::new(), Writer::start(sc, dst)).copy_root(root))
}

/// Copy the history at `root` in `src` into `dst`: first `versions`
/// (oldest first, in groups of [`ROOTS_PER_ROUND`], each as soon as it is
/// known to be missing), then the history's own blobs, skipping the parts
/// `dst` already holds. Push passes only the versions of nodes the
/// destination's history lacks, and as `known` the versions it is known to
/// hold: its local diff walk, as git's push negotiates against the
/// remote's ref.
fn copy_history(
    src: &dyn Store,
    dst: &dyn Store,
    root: &View,
    versions: Vec<View>,
    known: HashSet<String>,
) -> Result<Copied> {
    std::thread::scope(|sc| {
        copy_history_with(Copier::new(src, dst, known, Writer::start(sc, dst)), dst, root, versions)
    })
}

fn copy_history_with(mut c: Copier, dst: &dyn Store, root: &View, versions: Vec<View>) -> Result<Copied> {
    let mut seen = HashSet::new();
    let versions: Vec<View> = versions.into_iter().filter(|v| seen.insert(v.key().to_string())).collect();
    // One batched check for all of them first: a push of one commit finds
    // every version but the new one present.
    let keys: Vec<String> = versions.iter().map(|v| v.key().to_string()).collect();
    let present = if keys.is_empty() { Vec::new() } else { dst.has_many(&keys)? };
    let mut missing = Vec::new();
    for (v, p) in versions.into_iter().zip(present) {
        if p {
            c.n.present += 1;
            c.done.insert(v.key());
        } else {
            missing.push(v);
        }
    }
    for group in missing.chunks(ROOTS_PER_ROUND) {
        c.copy_many(group)?;
    }
    c.copy_root(root)
}

/// Write `puts` in order: the closure invariant holds at every point, since
/// a parent follows its children in the list.
fn write_puts(dst: &dyn Store, puts: &[Put]) -> Result<()> {
    dst.write_many(puts).map_err(|(i, e)| match e {
        // The values come unchecked from the source.
        CoreError::Invalid(cas_core::Invalid::HashMismatch) => wrong_bytes(&puts[i].key),
        e => Error::from(e),
    })
}

/// A thread that writes a copy's batches to the destination, in the order
/// sent, while the copy reads and decodes the next.
struct Writer {
    to: std::sync::mpsc::Sender<std::sync::Arc<Vec<Put>>>,
    back: std::sync::mpsc::Receiver<Result<()>>,
}

impl Writer {
    /// None on one thread (`cas_core::threads`): the copy writes itself.
    fn start<'s>(sc: &'s std::thread::Scope<'s, '_>, dst: &'s dyn Store) -> Option<Writer> {
        if cas_core::threads::get() < 2 {
            return None;
        }
        let (to, rx) = std::sync::mpsc::channel::<std::sync::Arc<Vec<Put>>>();
        let (tx, back) = std::sync::mpsc::channel();
        sc.spawn(move || {
            for puts in rx {
                if tx.send(write_puts(dst, &puts)).is_err() {
                    break;
                }
            }
        });
        Some(Writer { to, back })
    }
}

fn writer_gone() -> Error {
    Error::Io("the copy's writer thread stopped".into())
}

/// Node versions copied together when a history is copied.
pub const ROOTS_PER_ROUND: usize = 1024;

struct Copier<'a> {
    src: &'a dyn Store,
    dst: &'a dyn Store,
    n: Copied,
    formats_seen: BTreeSet<String>,
    /// Keys known to be in the destination, complete, or written to
    /// `pending` after everything they hold.
    done: DigestSet,
    /// Writes not yet sent, children before parents.
    pending: Vec<Put>,
    pending_bytes: usize,
    /// The size of `pending` as the source keeps it (forms, else values).
    pending_stored: usize,
    /// Where each pending write is in `pending`.
    pending_at: KeyMap<usize>,
    /// The writes sent to the writer thread and not known written yet,
    /// with where each is: their values still serve as delta bases.
    sent: Option<(std::sync::Arc<Vec<Put>>, KeyMap<usize>)>,
    writer: Option<Writer>,
}

/// Writes are gathered into one `write_many` until they hold this many
/// bytes as the source stores them, or [`WRITE_BYTES`] as values: a disk
/// store makes each call one pack, so few large calls spare it merging
/// many small packs (cas_core's `BIG_PACK`).
const WRITE_STORED: usize = 64 << 20;
const WRITE_BYTES: usize = 1 << 30;

/// A container read from the source, waiting for its children.
struct Waiting {
    bytes: Vec<u8>,
    form: Option<Form>,
    /// Children not yet known to be in the destination.
    left: usize,
}

impl<'a> Copier<'a> {
    fn new(src: &'a dyn Store, dst: &'a dyn Store, done: HashSet<String>, writer: Option<Writer>) -> Copier<'a> {
        Copier {
            src,
            dst,
            n: Copied::default(),
            formats_seen: BTreeSet::new(),
            done: done.into_iter().collect(),
            pending: Vec::new(),
            pending_bytes: 0,
            pending_stored: 0,
            pending_at: KeyMap::default(),
            sent: None,
            writer,
        }
    }

    fn copy_root(mut self, root: &View) -> Result<Copied> {
        self.copy_many(std::slice::from_ref(root))?;
        self.flush()?;
        self.wait()?;
        Ok(self.n)
    }

    /// The values of the blobs `keys` from the source, each with the form
    /// the source keeps it in. A delta's base comes from this batch, the
    /// writes not yet sent or still waiting, or the destination; one whose base is in none
    /// of them is read again as a value. Values are not checked against
    /// their keys here: the destination does that as it writes them.
    fn read_values(&self, keys: &[String], waiting: &KeyMap<Waiting>) -> Result<Vec<(Vec<u8>, Option<Form>)>> {
        let mut stored: Vec<Stored> = Vec::with_capacity(keys.len());
        for (k, s) in keys.iter().zip(self.src.read_stored_many(keys)) {
            stored.push(s?.ok_or_else(|| Error::Missing(k.clone()))?);
        }
        let mut values: Vec<Option<Vec<u8>>> = vec![None; keys.len()];
        let at: HashMap<&str, usize, cas_core::keyhash::KeyState> =
            keys.iter().enumerate().map(|(i, k)| (k.as_str(), i)).collect();
        let bad = |k: &str, e: std::io::Error| Error::Store(CoreError::Unavailable(format!("{k}: {e}")));
        let mut todo: Vec<usize> = Vec::new();
        for (i, s) in stored.iter().enumerate() {
            match s.base() {
                None => values[i] = Some(s.value(None).map_err(|e| bad(&keys[i], e))?),
                Some(_) => todo.push(i),
            }
        }
        // Deltas, until no more can be decoded (a base later in the batch,
        // or one that is itself a delta here).
        loop {
            let before = todo.len();
            let mut left = Vec::new();
            for i in todo {
                let base = stored[i].base().unwrap();
                // The base where it is, not copied.
                let near: Option<&[u8]> = match at.get(base) {
                    Some(&j) => values[j].as_deref(),
                    None => match (self.pending_at.get(base), waiting.get(base)) {
                        (Some(&j), _) => Some(&self.pending[j].value),
                        (None, Some(w)) => Some(&w.bytes),
                        (None, None) => {
                            self.sent.as_ref().and_then(|(puts, at)| at.get(base).map(|&j| puts[j].value.as_slice()))
                        }
                    },
                };
                let decoded = match near {
                    Some(b) => Some(stored[i].value(Some(b))),
                    None if at.contains_key(base) => None,
                    None => self.dst.read(base).ok().flatten().map(|b| stored[i].value(Some(&b))),
                };
                match decoded {
                    Some(v) => values[i] = Some(v.map_err(|e| bad(&keys[i], e))?),
                    None => left.push(i),
                }
            }
            todo = left;
            if todo.is_empty() || todo.len() == before {
                break;
            }
        }
        if !todo.is_empty() {
            let again: Vec<String> = todo.iter().map(|&i| keys[i].clone()).collect();
            for (&i, v) in todo.iter().zip(self.src.read_many(&again)) {
                values[i] = Some(v?.ok_or_else(|| Error::Missing(keys[i].clone()))?);
                stored[i] = Stored::Value(Vec::new());
            }
        }
        // Not hashed here: the destination checks each value against its key
        // as it writes it (see `flush`).
        let mut out = Vec::with_capacity(keys.len());
        for (v, s) in values.into_iter().zip(stored) {
            out.push((v.expect("every value decoded or read"), s.into_form()));
        }
        Ok(out)
    }

    /// Send the gathered writes, in order: the closure invariant holds at
    /// every point, since a parent follows its children in the list.
    fn flush(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let puts = std::mem::take(&mut self.pending);
        let at = std::mem::take(&mut self.pending_at);
        self.pending_bytes = 0;
        self.pending_stored = 0;
        match &self.writer {
            // One batch writes while the next is gathered: wait for the one
            // before, then hand this one over.
            Some(_) => {
                self.wait()?;
                let puts = std::sync::Arc::new(puts);
                self.writer.as_ref().unwrap().to.send(puts.clone()).map_err(|_| writer_gone())?;
                self.sent = Some((puts, at));
                Ok(())
            }
            None => write_puts(self.dst, &puts),
        }
    }

    /// Wait until what was sent to the writer thread is written.
    fn wait(&mut self) -> Result<()> {
        if self.sent.take().is_some() {
            self.writer.as_ref().unwrap().back.recv().map_err(|_| writer_gone())??;
        }
        Ok(())
    }

    fn copy_many(&mut self, roots: &[View]) -> Result<()> {
        let (src, dst) = (self.src, self.dst);
        let mut waiting: KeyMap<Waiting> = KeyMap::default();
        // Child key to the containers waiting on it.
        let mut parents: KeyMap<Vec<String>> = KeyMap::default();
        let mut frontier: Vec<View> = roots.to_vec();
        let mut queued = KeySet::default();
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
            let mut forms: KeyMap<Form> = KeyMap::default();
            for (r, (bytes, form)) in want.into_iter().zip(self.read_values(&keys, &waiting)?) {
                if let Some(f) = form {
                    forms.insert(r.key().to_string(), f);
                }
                let kids = children_of_light(&r, &bytes, &self.done).map_err(|e| {
                    // Unreadable as sent: report bytes that are not the key's.
                    match cas_core::key::classify(r.key()) {
                        Some(cas_core::key::Parsed::Blob { alg, .. })
                            if cas_core::algo::lookup(alg).is_some_and(|a| a.key(&bytes) != r.key()) =>
                        {
                            wrong_bytes(r.key())
                        }
                        _ => e,
                    }
                })?;
                let Some(kids) = kids else {
                    ready.push(Put { form: forms.remove(r.key()), ..Put::blob(r.key(), bytes) });
                    continue;
                };
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
                let form = forms.remove(r.key());
                if left == 0 {
                    ready.push(Put { form, ..Put::blob(r.key(), bytes) });
                } else {
                    waiting.insert(r.key().to_string(), Waiting { bytes, form, left });
                }
            }
            // Write what is ready; each write may free the containers above it.
            loop {
                self.n.blobs += ready.len();
                resolved.extend(ready.iter().map(|p| p.key.clone()));
                self.pending_bytes += ready.iter().map(|p| p.value.len()).sum::<usize>();
                self.pending_stored +=
                    ready.iter().map(|p| p.form.as_ref().map_or(p.value.len(), Form::size)).sum::<usize>();
                for p in ready.drain(..) {
                    self.pending_at.insert(p.key.clone(), self.pending.len());
                    self.pending.push(p);
                }
                if self.pending_bytes >= WRITE_BYTES || self.pending_stored >= WRITE_STORED {
                    self.flush()?;
                }
                for k in resolved.drain(..) {
                    for parent in parents.remove(&k).unwrap_or_default() {
                        let w = waiting.get_mut(&parent).expect("a waiting parent");
                        w.left -= 1;
                        if w.left == 0 {
                            let w = waiting.remove(&parent).expect("waiting");
                            ready.push(Put { form: w.form, ..Put::blob(parent, w.bytes) });
                        }
                    }
                    self.done.insert(&k);
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
    let mine = history::Stored::open(src, Some(&head))?;
    // The destination's history, read locally when the source has it (the
    // usual push: the source fetched it before committing on top). Only
    // the leaves that differ are read.
    let added = match &theirs {
        None => None,
        Some(t) => {
            let at: &dyn Store = if src.has_many(&[t.key().to_string()])?[0] { src } else { dst };
            let c = history::Stored::open(at, Some(t))?.changed(&mine)?;
            if c.old.iter().any(|(id, n)| c.added.get(id).is_none_or(|m| m.after != n.after)) {
                return Err(Error::Diverged(to.to_string()));
            }
            Some(c)
        }
    };
    let copied = if dst.has_many(&[head.key().to_string()])?[0] {
        Copied { present: 1, ..Copied::default() }
    } else {
        match added {
            // The destination holds its history whole, so the versions of
            // nodes it shares with ours are there.
            Some(c) => {
                let known = c.same.iter().chain(c.old.keys()).map(|id| id.version().key().to_string()).collect();
                copy_history(src, dst, &head, c.added.keys().map(|id| id.version()).collect(), known)?
            }
            None => copy(src, dst, &head)?,
        }
    };
    let expect = match &theirs {
        Some(t) => Expect::Value(t.to_string().into_bytes()),
        None => Expect::Absent,
    };
    match dst.write(to, head.to_string().as_bytes(), &expect) {
        Ok(()) => {
            // Note what it replaced, so the destination's graph and
            // ancestry of the new history start from the old one's.
            if let Some(t) = &theirs
                && let Some(alg) = cas_core::key::classify(head.key()).and_then(|k| match k {
                    cas_core::key::Parsed::Blob { alg, .. } => cas_core::algo::lookup(alg),
                    cas_core::key::Parsed::Var => None,
                })
            {
                crate::graph::record_after_save(dst, alg, t, &head);
            }
            Ok((if theirs.is_some() { Outcome::FastForward } else { Outcome::Created }, copied))
        }
        // The destination moved while we copied; the copy is still valid.
        Err(CoreError::Conflict) => Err(Error::Diverged(to.to_string())),
        Err(e) => Err(e.into()),
    }
}
