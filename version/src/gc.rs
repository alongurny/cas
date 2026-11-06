//! Reachability, garbage collection and purging (spec/gc.md).
//!
//! The core cannot know what references what, so reachability is computed
//! here by format walkers, from roots that are tags. Collection is two
//! phase: a blob is removed only when two runs in a row found it
//! unreachable, so a writer has until the next run to link what it wrote.

use crate::formats::{self, METAFORMAT};
use crate::sync::children;
use crate::uni::Value;
use crate::{Ref, Result, by_alias, is_blob_key, write_blob};
use cas_core::algo::{self, Algorithm};
use cas_core::key::{Parsed, classify};
use cas_core::{Expect, Span, Store, list_all};
use std::collections::BTreeSet;

/// The tag holding the blobs the previous run found unreachable.
pub const PENDING: &str = "gc/pending";

fn blake3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

/// What a run saw and did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub reachable: usize,
    pub unreachable: usize,
    pub removed: Vec<String>,
}

/// The `spec <key>` of a format object, or the `'spec'` of a function
/// object: description blobs stay alive with what they describe.
fn spec_of(store: &dyn Store, key: &str) -> Result<Option<String>> {
    let Some(bytes) = store.read(key)? else {
        return Ok(None);
    };
    if let Ok(text) = std::str::from_utf8(&bytes)
        && let Some(line) = text.lines().find_map(|l| l.strip_prefix("spec "))
    {
        return Ok(Some(line.to_string()));
    }
    let v = Value::deserialize(&bytes).ok();
    Ok(v.as_ref()
        .and_then(Value::as_map)
        .and_then(|m| m.get(&Value::str("spec")))
        .and_then(Value::as_str)
        .map(String::from))
}

/// Every blob reachable from the tags in `store`.
pub fn mark(store: &dyn Store) -> Result<BTreeSet<String>> {
    let keys = list_all(store, &Span::all())?;
    let mut marked = BTreeSet::new();
    let mut raw: Vec<String> = vec![blake3().key(METAFORMAT.as_bytes())];
    let mut todo: Vec<Ref> = Vec::new();
    for k in &keys {
        if !matches!(classify(k), Some(Parsed::Tag)) {
            continue;
        }
        // A function's object stays alive while its results are cached.
        if let Some(f) = k.strip_prefix("derived/").and_then(|r| r.split('/').next()) {
            raw.push(f.to_string());
        }
        if let Some(v) = store.read(k)?
            && let Some(r) = std::str::from_utf8(&v).ok().and_then(|s| Ref::parse(s).ok())
        {
            todo.push(r);
        }
    }
    let mut formats_seen = BTreeSet::new();
    while let Some(r) = todo.pop() {
        if formats_seen.insert(r.format().to_string()) {
            raw.push(r.format().to_string());
        }
        if !marked.insert(r.key().to_string()) {
            continue;
        }
        match children(store, &r) {
            Ok(c) => todo.extend(c),
            // A missing blob ends the walk there.
            Err(crate::Error::Missing(_)) => {}
            Err(e) => return Err(e),
        }
    }
    while let Some(k) = raw.pop() {
        if is_blob_key(&k)
            && marked.insert(k.clone())
            && let Some(spec) = spec_of(store, &k)?
        {
            raw.push(spec);
        }
    }
    // Specs of the formats this crate knows, which it may have published.
    for f in formats::all() {
        if marked.contains(f.id()) {
            marked.insert(blake3().key(f.spec.as_bytes()));
        }
    }
    Ok(marked)
}

fn pending(store: &dyn Store) -> Result<(Option<Vec<u8>>, BTreeSet<String>)> {
    let Some(v) = store.read(PENDING)? else {
        return Ok((None, BTreeSet::new()));
    };
    let set = std::str::from_utf8(&v)
        .ok()
        .and_then(|s| Ref::parse(s).ok())
        .and_then(|r| store.read(r.key()).ok().flatten())
        .and_then(|b| Value::deserialize(&b).ok())
        .and_then(|v| v.as_map().map(|m| m.keys().filter_map(Value::as_str).map(String::from).collect()))
        .unwrap_or_default();
    Ok((Some(v), set))
}

/// One collection run: mark, remove what was unreachable this run and the
/// previous one, and record the rest as pending for the next.
pub fn collect(store: &dyn Store, alg: &'static Algorithm) -> Result<Report> {
    let (old_tag, old) = pending(store)?;
    let marked = mark(store)?;
    let blobs: Vec<String> = list_all(store, &Span::all())?
        .into_iter()
        .filter(|k| matches!(classify(k), Some(Parsed::Blob { .. })))
        .collect();
    let unreachable: BTreeSet<String> = blobs.iter().filter(|k| !marked.contains(*k)).cloned().collect();
    let mut removed = Vec::new();
    for k in unreachable.intersection(&old) {
        store.remove(k, &Expect::Absent)?;
        removed.push(k.clone());
    }
    let next: BTreeSet<&String> = unreachable.iter().filter(|k| !old.contains(*k)).collect();
    let set = Value::Map(next.iter().map(|k| (Value::str(k.as_str()), Value::empty())).collect());
    let key = write_blob(store, alg, &set.serialize())?;
    let r = Ref { format: by_alias("uni-bin").unwrap().id().to_string(), key };
    let expect = old_tag.map_or(Expect::Absent, Expect::Value);
    store.write(PENDING, r.to_string().as_bytes(), &expect)?;
    Ok(Report { reachable: marked.len(), unreachable: unreachable.len(), removed })
}
