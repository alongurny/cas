//! `fsck`: check what a store holds (git's `fsck`). From every variable
//! holding a view, walk what it reaches, as gc does (spec/gc.md), and check
//! each blob: that it can be read (the core verifies its hash on every
//! read), that its bytes are valid in its format when the format is known,
//! and that every view inside it names a blob that is there.

use crate::formats::Kind;
use crate::sync::children_of;
use crate::{Error, Result, View, by_id, is_blob_key};
use cas_core::key::{Parsed, classify};
use cas_core::{Span, Store, list_all};
use std::collections::{BTreeSet, HashSet};

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
    let mut r = Report::default();
    let mut todo: Vec<(String, View)> = Vec::new();
    for k in &keys {
        if matches!(classify(k), Some(Parsed::Var))
            && let Some(v) = store.read(k)?
            && let Some(view) = std::str::from_utf8(&v).ok().and_then(|s| View::parse(s).ok())
        {
            todo.push((k.clone(), view));
        }
    }
    let mut seen: HashSet<String> = HashSet::new();
    while let Some((holder, v)) = todo.pop() {
        if !seen.insert(v.key().to_string()) {
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
        if v.known_format().is_some_and(|f| f.alias == "history")
            && let Err(e) = crate::history::read(store, &v)
            && !matches!(e, Error::Missing(_))
        {
            r.bad.push((v.key().to_string(), e.to_string()));
        }
        // The format object itself is a blob too.
        if is_blob_key(v.format()) && by_id(v.format()).is_none() && !blobs.contains(v.format()) {
            r.missing.push((v.key().to_string(), v.format().to_string()));
        }
        todo.extend(kids.into_iter().map(|c| (v.key().to_string(), c)));
    }
    r.unreachable = blobs.iter().filter(|k| !seen.contains(**k)).count();
    Ok(r)
}
