//! `fsck`: check what a store holds (git's `fsck`). From every variable
//! holding a view, walk what it reaches, as gc does (spec/gc.md), and check
//! each blob: that it can be read (the core verifies its hash on every
//! read), that its bytes are valid in its format when the format is known,
//! and that every view inside it names a blob that is there.

use crate::formats::Kind;
use crate::sync::children_of;
use crate::{Error, Result, View, by_id, is_blob_key};
use cas_core::key::{Parsed, classify};
use cas_core::keyhash::{DigestSet, SharedDigestSet};
use cas_core::{Span, Store, list_all};
use std::collections::{BTreeSet, HashMap};

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
    let seen = SharedDigestSet::default();
    // The variables' views first, on this thread; the versions of the
    // histories they hold are set aside and walked on every thread, and
    // the histories' own parts (which name every version, in id order)
    // after them.
    let mut versions = Vec::new();
    let mut r = Walker::new(store, &blobs, &seen);
    let mut later = Vec::new();
    r.walk(todo, Some((&mut versions, &mut later)));
    let (mut r, mut notes, mut lens) = (r.r, r.notes, r.lens);
    // Each thread takes a run of consecutive versions, oldest first, so
    // that its decodes find their bases in the cache. A blob is checked by
    // the first thread to reach it.
    let threads = cas_core::threads::get().min(versions.len()).max(1);
    let per = versions.len().div_ceil(threads).max(1);
    let parts: Vec<(Report, Vec<View>, HashMap<String, usize>)> = std::thread::scope(|sc| {
        let handles: Vec<_> = versions
            .chunks(per)
            .map(|run| {
                let (blobs, seen) = (&blobs, &seen);
                sc.spawn(move || {
                    let mut w = Walker::new(store, blobs, seen);
                    // Popped last first: reversed, the oldest goes first.
                    w.walk(run.iter().rev().cloned().collect(), None);
                    (w.r, w.notes, w.lens)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("fsck thread panicked")).collect()
    });
    let mut w = Walker::new(store, &blobs, &seen);
    w.walk(later, None);
    for (p, n, l) in parts.into_iter().chain([(w.r, w.notes, w.lens)]) {
        r.checked += p.checked;
        r.missing.extend(p.missing);
        r.bad.extend(p.bad);
        notes.extend(n);
        lens.extend(l);
    }
    r.bad.extend(check_notes(store, &notes, &lens));
    r.missing.sort();
    r.bad.sort();
    // What gc would collect: it also keeps the format objects and their
    // specs, which no view inside a blob names.
    r.unreachable = match crate::gc::mark(store) {
        Ok(kept) => blobs.iter().filter(|k| !seen.contains(k) && !kept.contains(k)).count(),
        Err(_) => blobs.iter().filter(|k| !seen.contains(k)).count(),
    };
    Ok(r)
}

struct Walker<'a> {
    store: &'a dyn Store,
    blobs: &'a BTreeSet<&'a str>,
    seen: &'a SharedDigestSet,
    /// What this thread has met, asked before the shared set.
    local: DigestSet,
    r: Report,
    /// Notes met, to check once every file's lines are counted.
    notes: Vec<View>,
    /// Lines of the text files read, by key.
    lens: HashMap<String, usize>,
}

impl<'a> Walker<'a> {
    fn new(store: &'a dyn Store, blobs: &'a BTreeSet<&'a str>, seen: &'a SharedDigestSet) -> Walker<'a> {
        let (notes, lens) = Default::default();
        Walker { store, blobs, seen, local: DigestSet::default(), r: Report::default(), notes, lens }
    }

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
            // Notes are checked against their files after the walk, which
            // counts the lines of every text file it reads.
            match v.known_format().map(|f| f.alias) {
                Some("annotated" | "step-notes") => self.notes.push(v.clone()),
                Some(_) if crate::annotated::is_text_format(&v) && !bytes.contains(&0) => {
                    self.lens.insert(v.key().to_string(), crate::annotated::lines(&bytes).len());
                }
                _ => {}
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

/// Notes that do not cover their files' lines exactly, on every thread.
/// Lines come from `lens` (the walk's count), or a read for a file it missed.
fn check_notes(store: &dyn Store, notes: &[View], lens: &HashMap<String, usize>) -> Vec<(String, String)> {
    let count = |f: &View| -> Result<Option<usize>> {
        match lens.get(f.key()) {
            Some(&n) => Ok(Some(n)),
            None => {
                let b = crate::read_blob(store, f.key())?;
                Ok((!b.contains(&0)).then(|| crate::annotated::lines(&b).len()))
            }
        }
    };
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = cas_core::threads::get().clamp(1, notes.len().max(1));
    std::thread::scope(|sc| {
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                sc.spawn(|| {
                    let mut memo = crate::dir::FindMemo::default();
                    let mut bad = Vec::new();
                    // Runs of nodes in order, so the memo meets neighbours.
                    loop {
                        let k = next.fetch_add(64, std::sync::atomic::Ordering::Relaxed);
                        let Some(run) = notes.get(k..(k + 64).min(notes.len())).filter(|r| !r.is_empty()) else {
                            break;
                        };
                        for v in run {
                            let res = match v.known_format().map(|f| f.alias) {
                                Some("step-notes") => crate::stepnotes::verify_with(store, v.key(), &mut memo, &count),
                                _ => crate::annotated::Annotated::decode(
                                    &crate::read_blob(store, v.key()).unwrap_or_default(),
                                )
                                .map_err(|e| crate::malformed(v.key(), e))
                                .and_then(|a| match count(&a.target)? {
                                    Some(n) if n == a.len() => Ok(()),
                                    n => Err(crate::malformed(
                                        v.key(),
                                        format!("notes for {} lines, the text has {n:?}", a.len()),
                                    )),
                                }),
                            };
                            match res {
                                Ok(()) | Err(Error::Missing(_)) => {}
                                Err(e) => bad.push((v.key().to_string(), e.to_string())),
                            }
                        }
                    }
                    bad
                })
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().expect("fsck thread panicked")).collect()
    })
}
