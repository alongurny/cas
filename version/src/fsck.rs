//! `fsck`: check what a store holds (git's `fsck`). From every variable
//! holding a view, walk what it reaches, as gc does (spec/gc.md), and check
//! each blob: that it can be read (the core verifies its hash on every
//! read), that its bytes are valid in its format when the format is known,
//! and that every view inside it names a blob that is there.

use crate::formats::Kind;
use crate::sync::children_of;
use crate::{Error, Result, View, by_id, is_blob_key};
use cas_core::key::{Parsed, classify};
use cas_core::keyhash::DigestSet;
use cas_core::{Span, Store, list_all};
use std::collections::BTreeSet;

/// What a check found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Blobs reached from the variables and checked.
    pub checked: usize,
    /// Blobs in the store that nothing reaches (gc would collect them).
    pub unreachable: usize,
    /// `(holder, missing)`: a view in `holder` (a blob, or a variable) names
    /// a blob the store does not have.
    pub missing: Vec<(String, String)>,
    /// `(key, reason)`: a blob that cannot be read or is not valid in its
    /// format.
    pub bad: Vec<(String, String)>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.missing.is_empty() && self.bad.is_empty()
    }
}

pub fn fsck(store: &dyn Store) -> Result<Report> {
    let keys = list_all(store, &Span::all())?;
    let blobs: BTreeSet<&str> =
        keys.iter().map(String::as_str).filter(|k| matches!(classify(k), Some(Parsed::Blob { .. }))).collect();
    let mut todo: Vec<(String, View)> = Vec::new();
    for k in &keys {
        if matches!(classify(k), Some(Parsed::Var))
            && let Some(v) = store.read(k)?
            && let Some(view) = std::str::from_utf8(&v).ok().and_then(|s| View::parse(s).ok())
        {
            todo.push((k.clone(), view));
        }
    }
    let seen = Seen::default();
    // The variables' views first, on this thread; the versions of the
    // histories they hold are set aside and walked on every thread, and
    // the histories' own parts (which name every version, in id order)
    // after them.
    let mut versions = Vec::new();
    let mut r = Walker { store, blobs: &blobs, seen: &seen, local: DigestSet::default(), r: Report::default() };
    let mut later = Vec::new();
    r.walk(todo, Some((&mut versions, &mut later)));
    let mut r = r.r;
    // Each thread takes a run of consecutive versions, oldest first, so
    // that its decodes find their bases in the cache. A blob is checked by
    // the first thread to reach it.
    let threads = cas_core::threads::get().min(versions.len()).max(1);
    let per = versions.len().div_ceil(threads).max(1);
    let parts: Vec<Report> = std::thread::scope(|sc| {
        let handles: Vec<_> = versions
            .chunks(per)
            .map(|run| {
                let (blobs, seen) = (&blobs, &seen);
                sc.spawn(move || {
                    let mut w = Walker { store, blobs, seen, local: DigestSet::default(), r: Report::default() };
                    // Popped last first: reversed, the oldest goes first.
                    w.walk(run.iter().rev().cloned().collect(), None);
                    w.r
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("fsck thread panicked")).collect()
    });
    let mut w = Walker { store, blobs: &blobs, seen: &seen, local: DigestSet::default(), r: Report::default() };
    w.walk(later, None);
    for p in parts.into_iter().chain([w.r]) {
        r.checked += p.checked;
        r.missing.extend(p.missing);
        r.bad.extend(p.bad);
    }
    r.missing.sort();
    r.bad.sort();
    r.unreachable = blobs.iter().filter(|k| !seen.contains(k)).count();
    Ok(r)
}

/// The keys every thread has reached, in shards so threads seldom wait.
struct Seen([std::sync::Mutex<DigestSet>; 64]);

impl Default for Seen {
    fn default() -> Self {
        Seen(std::array::from_fn(|_| Default::default()))
    }
}

impl Seen {
    fn shard(&self, k: &str) -> &std::sync::Mutex<DigestSet> {
        let b = k.as_bytes();
        let h = b.iter().rev().take(2).fold(0usize, |h, &c| h * 31 + usize::from(c));
        &self.0[h % 64]
    }

    /// Adds `k`; false if a thread had already reached it.
    fn insert(&self, k: &str) -> bool {
        self.shard(k).lock().unwrap().insert(k)
    }

    fn contains(&self, k: &str) -> bool {
        self.shard(k).lock().unwrap().contains(k)
    }
}

struct Walker<'a> {
    store: &'a dyn Store,
    blobs: &'a BTreeSet<&'a str>,
    seen: &'a Seen,
    /// What this thread has met, asked before the shared set.
    local: DigestSet,
    r: Report,
}

impl Walker<'_> {
    /// Check everything reachable from `todo` (popped from the end) that no
    /// thread has reached yet. With `defer`, a history's versions are put in
    /// its first list (oldest first) and its parts in the second, instead of
    /// walked.
    #[allow(clippy::type_complexity)]
    fn walk(
        &mut self,
        mut todo: Vec<(String, View)>,
        mut defer: Option<(&mut Vec<(String, View)>, &mut Vec<(String, View)>)>,
    ) {
        let (store, r) = (self.store, &mut self.r);
        while let Some((holder, v)) = todo.pop() {
            if !self.local.insert(v.key()) || !self.seen.insert(v.key()) {
                continue;
            }
            let bytes = match store.read(v.key()) {
                Ok(Some(b)) => b,
                Ok(None) => {
                    r.missing.push((holder, v.key().to_string()));
                    continue;
                }
                Err(e) => {
                    r.bad.push((v.key().to_string(), e.to_string()));
                    continue;
                }
            };
            r.checked += 1;
            // A directory is decoded once, for its check and its children.
            let kids = match by_id(v.format()).map(|f| &f.kind) {
                Some(Kind::Dir(d)) => d.blob_children(v.key(), &bytes),
                Some(_) => by_id(v.format())
                    .unwrap()
                    .check(v.key(), &bytes)
                    .and_then(|()| children_of(&v, &bytes).map(Option::unwrap_or_default)),
                None => Ok(Vec::new()),
            };
            let kids = match kids {
                Ok(k) => k,
                Err(e) => {
                    r.bad.push((v.key().to_string(), e.to_string()));
                    continue;
                }
            };
            // A history is checked whole too: its tree's shape and its tips.
            // Its versions are walked oldest first, each before the next: a
            // version's blobs are mostly deltas on the one before's, so a
            // walk in that order decodes each blob once.
            let mut vs = Vec::new();
            if v.known_format().is_some_and(|f| f.alias == "history") {
                match crate::history::read(store, &v) {
                    Ok(h) => vs = h.log().into_iter().map(|id| (v.key().to_string(), id.version())).collect(),
                    Err(Error::Missing(_)) => {}
                    Err(e) => r.bad.push((v.key().to_string(), e.to_string())),
                }
            }
            // The format object itself is a blob too.
            if is_blob_key(v.format()) && by_id(v.format()).is_none() && !self.blobs.contains(v.format()) {
                r.missing.push((v.key().to_string(), v.format().to_string()));
            }
            let kids = kids.into_iter().map(|c| (v.key().to_string(), c));
            // The log is newest first: popped from the end, oldest first.
            match defer.as_mut() {
                Some((out, later)) if !vs.is_empty() => {
                    out.extend(vs.into_iter().rev());
                    later.extend(kids);
                }
                _ => {
                    todo.extend(kids);
                    todo.extend(vs);
                }
            }
        }
    }
}
