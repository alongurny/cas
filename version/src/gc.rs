//! Reachability, garbage collection and purging (spec/gc.md).
//!
//! The core cannot know what refers to what, so reachability is computed
//! here by format walkers, from roots that are variables. Collection is two
//! phase: a blob is removed only when two runs in a row found it
//! unreachable, so a writer has until the next run to link what it wrote.

use crate::dir::LeafIter;
use crate::formats::{self, METAFORMAT};
use crate::history::{self, NodeId};
use crate::sync::children_light;
use crate::tree;
use crate::uni::Value;
use crate::{Result, View, by_alias, is_blob_key, write_blob};
use cas_core::algo::{self, Algorithm};
use cas_core::key::{Parsed, classify};
use cas_core::keyhash::KeySet;
use cas_core::{Expect, Span, Store, list_all};
use std::collections::{BTreeSet, HashMap, HashSet};

/// The variable holding the blobs the previous run found unreachable.
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

/// Record the ancestry (spec/functions/ancestry.md) of every history a
/// variable holds that has none yet, as git gc writes its commit-graph.
/// Best effort for each history.
pub fn index_histories(store: &dyn Store, alg: &'static Algorithm) -> Result<()> {
    for k in list_all(store, &Span::all())? {
        if !matches!(classify(&k), Some(Parsed::Var)) || k.starts_with("derived") {
            continue;
        }
        if let Some(v) = store.read(&k)?
            && let Some(r) = std::str::from_utf8(&v).ok().and_then(|s| View::parse(s).ok())
            && r.known_format().is_some_and(|f| f.alias == "history")
            && matches!(crate::derived::readable_cache(store, &crate::ancestry::ANCESTRY, &r), Ok(None))
            && let Ok(h) = history::read(store, &r)
        {
            crate::ancestry::record(store, alg, &r, &h);
        }
    }
    Ok(())
}

/// Every blob reachable from the variables in `store`.
pub fn mark(store: &dyn Store) -> Result<KeySet> {
    let keys = list_all(store, &Span::all())?;
    let mut marked = KeySet::default();
    let mut raw: Vec<String> = vec![blake3().key(METAFORMAT.as_bytes())];
    let mut todo: Vec<View> = Vec::new();
    for k in &keys {
        if !matches!(classify(k), Some(Parsed::Var)) {
            continue;
        }
        // A function's object stays alive while its results are cached.
        if let Some(f) = k.strip_prefix("derived/").and_then(|r| r.split('/').next()) {
            raw.push(f.to_string());
        }
        if let Some(v) = store.read(k)?
            && let Some(r) = std::str::from_utf8(&v).ok().and_then(|s| View::parse(s).ok())
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
        match children_light(store, &r, &|k| marked.contains(k)) {
            Ok(c) => {
                todo.extend(c);
                // Versions oldest first, each walked before the next, as
                // fsck does: blobs are deltas on the version before's.
                if r.known_format().is_some_and(|f| f.alias == "history")
                    && let Ok(h) = history::read(store, &r)
                {
                    todo.extend(h.log().into_iter().map(|id| id.version()));
                }
            }
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
        .and_then(|s| View::parse(s).ok())
        .and_then(|r| store.read(r.key()).ok().flatten())
        .and_then(|b| Value::deserialize(&b).ok())
        .and_then(|v| v.as_map().map(|m| m.keys().filter_map(Value::as_str).map(String::from).collect()))
        .unwrap_or_default();
    Ok((Some(v), set))
}

/// One collection run: mark, remove what was unreachable this run and the
/// previous one, and record the rest as pending for the next.
pub fn collect(store: &dyn Store, alg: &'static Algorithm) -> Result<Report> {
    index_histories(store, alg)?;
    let (old_var, old) = pending(store)?;
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
    let r = View { format: by_alias("uni-bin").unwrap().id().to_string(), key };
    let expect = old_var.map_or(Expect::Absent, Expect::Value);
    store.write(PENDING, r.to_string().as_bytes(), &expect)?;
    Ok(Report { reachable: marked.len(), unreachable: unreachable.len(), removed })
}

/// Delta hints for a repack (spec/performance.md, step 2): for every
/// history a variable holds, each node's version is walked beside its
/// predecessor's, and each directory or file that changed at a path is
/// hinted to its old self there. Also an order, oldest node first, so that a
/// base is placed before what is a delta of it. A version `placed` says the
/// last repack placed is skipped with all it holds: its frames are reused.
pub fn delta_hints(store: &dyn Store, placed: &dyn Fn(&str) -> bool) -> Result<(Vec<String>, HashMap<String, String>)> {
    let mut order = Vec::new();
    let mut hints = HashMap::new();
    let mut walked = HashSet::new();
    for k in list_all(store, &Span::all())? {
        if !matches!(classify(&k), Some(Parsed::Var)) {
            continue;
        }
        let Some(v) = store.read(&k)? else { continue };
        let Some(r) = std::str::from_utf8(&v).ok().and_then(|s| View::parse(s).ok()) else { continue };
        if r.known_format().is_none_or(|f| f.alias != "history") {
            continue;
        }
        let Ok(h) = history::read(store, &r) else { continue };
        for id in h.log().into_iter().rev() {
            let new = id.version();
            let old = h.nodes[&id].after.keys().next().map(NodeId::version);
            pair(store, old.as_ref(), &new, &mut order, &mut hints, &mut walked, placed)?;
        }
    }
    Ok((order, hints))
}

/// Record `new` (and, for directories, what changed under it) against `old`.
fn pair(
    store: &dyn Store,
    old: Option<&View>,
    new: &View,
    order: &mut Vec<String>,
    hints: &mut HashMap<String, String>,
    walked: &mut HashSet<String>,
    placed: &dyn Fn(&str) -> bool,
) -> Result<()> {
    if old.is_some_and(|o| o.key() == new.key()) || !walked.insert(new.key().to_string()) || placed(new.key()) {
        return Ok(());
    }
    order.push(new.key().to_string());
    if let Some(o) = old {
        hints.entry(new.key().to_string()).or_insert_with(|| o.key().to_string());
    }
    if !new.is_dir() {
        return Ok(());
    }
    if let Some(o) = old
        && is_tree(o)
        && is_tree(new)
    {
        return pair_trees(store, o, new, order, hints, walked, placed);
    }
    let olds: HashMap<String, View> = match old.filter(|o| o.is_dir()) {
        Some(o) => tree::load(store, o)?.into_iter().map(|i| (i.name, i.child)).collect(),
        None => HashMap::new(),
    };
    for item in tree::load(store, new)? {
        pair(store, olds.get(&item.name), &item.child, order, hints, walked, placed)?;
    }
    Ok(())
}

fn is_tree(v: &View) -> bool {
    v.known_format().is_some_and(|f| f.alias == "tree")
}

/// A `tree` directory's blobs below its top: the inner ones, and the leaves
/// as (greatest name, key), in name order.
/// [`pair`] of two `tree` directories, reading only the leaves that differ:
/// the entries of a leaf both share are the same. Each new blob of the
/// directory is hinted to an old one covering the same names.
fn pair_trees(
    store: &dyn Store,
    old: &View,
    new: &View,
    order: &mut Vec<String>,
    hints: &mut HashMap<String, String>,
    walked: &mut HashSet<String>,
    placed: &dyn Fn(&str) -> bool,
) -> Result<()> {
    let (oi, ol) = crate::dir::tree_parts(store, old.key(), false)?;
    let (ni, nl) = crate::dir::tree_parts(store, new.key(), false)?;
    let old_keys: HashSet<&str> = ol.iter().map(|l| l.1.as_str()).chain(oi.iter().map(String::as_str)).collect();
    let new_keys: HashSet<&str> = nl.iter().map(|l| l.1.as_str()).chain(ni.iter().map(String::as_str)).collect();
    let old_only: Vec<&(String, String)> = ol.iter().filter(|l| !new_keys.contains(l.1.as_str())).collect();
    let new_only: Vec<&(String, String)> = nl.iter().filter(|l| !old_keys.contains(l.1.as_str())).collect();
    // Inner blobs by position; leaves by the names they cover.
    let oi_only: Vec<&String> = oi.iter().filter(|k| !new_keys.contains(k.as_str())).collect();
    for (i, k) in ni.iter().filter(|k| !old_keys.contains(k.as_str())).enumerate() {
        if walked.insert(k.clone()) && !placed(k) {
            order.push(k.clone());
            if !oi_only.is_empty() {
                hints.entry(k.clone()).or_insert_with(|| oi_only[i.min(oi_only.len() - 1)].clone());
            }
        }
    }
    for (last, k) in &new_only {
        if walked.insert(k.clone()) && !placed(k) {
            order.push(k.clone());
            // The first old leaf whose names reach this one's last.
            let j = old_only.partition_point(|o| o.0 < *last).min(old_only.len().saturating_sub(1));
            if let Some(o) = old_only.get(j) {
                hints.entry(k.clone()).or_insert_with(|| o.1.clone());
            }
        }
    }
    // A merge-join of the differing leaves, old and new, by name: only the
    // entries that changed are made into views.
    let read = |k: &str| crate::read_blob(store, k);
    let bad = |k: &str, e: String| crate::malformed(k, e);
    let old_bytes: Vec<(&str, Vec<u8>)> =
        old_only.iter().map(|(_, k)| Ok((k.as_str(), read(k)?))).collect::<Result<_>>()?;
    let mut olds = old_bytes.iter().filter_map(|(k, b)| LeafIter::new(b).map_err(|e| bad(k, e)).transpose());
    let mut cur = olds.next().transpose()?;
    let mut has_old = match cur.as_mut() {
        Some(c) => c.advance().map_err(|e| bad("old leaf", e))?,
        None => false,
    };
    for (_, k) in &new_only {
        let bytes = read(k)?;
        let Some(mut it) = LeafIter::new(&bytes).map_err(|e| bad(k, e))? else { continue };
        while it.advance().map_err(|e| bad(k, e))? {
            // Old entries before this name are gone from the new directory.
            loop {
                if !has_old {
                    match olds.next().transpose()? {
                        Some(mut c) => {
                            has_old = c.advance().map_err(|e| bad("old leaf", e))?;
                            cur = Some(c);
                            continue;
                        }
                        None => break,
                    }
                }
                let c = cur.as_mut().unwrap();
                if c.name() < it.name() {
                    has_old = c.advance().map_err(|e| bad("old leaf", e))?;
                } else {
                    break;
                }
            }
            let same_name = has_old && cur.as_ref().is_some_and(|c| c.name() == it.name());
            if same_name && cur.as_ref().unwrap().same_key(&it) {
                continue;
            }
            let old = same_name.then(|| cur.as_ref().unwrap().view());
            pair(store, old.as_ref(), &it.view(), order, hints, walked, placed)?;
        }
    }
    Ok(())
}
