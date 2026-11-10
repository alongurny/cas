//! Blob reads scoped by reachability (spec/forge/store.md): for each
//! project, the blobs its readable variables reach. A set is built when
//! first asked for and grows as the project's variables move; it is only
//! ever too large by blobs a variable reached before it moved, which the
//! project's readers could read then.

use crate::names::ProjectId;
use cas_core::Span;
use cas_core::Store;
use cas_version::View;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex, RwLock};

/// A blob key, shortened: its algorithm's tag and the first 15 bytes of its
/// digest.
type Short = u128;

fn short(key: &str) -> Option<Short> {
    let (alg, hex) = key.split_once(':')?;
    let tag = alg.bytes().fold(0u8, |a, b| a.wrapping_mul(31).wrapping_add(b));
    let h = hex.get(..30)?;
    Some(u128::from_str_radix(h, 16).ok()? << 8 | tag as u128)
}

/// Format objects and specs: readable by anyone.
static ALWAYS: LazyLock<HashSet<Short>> = LazyLock::new(|| {
    let b3 = cas_core::algo::lookup("blake3").unwrap();
    let mut s = HashSet::new();
    let meta = b3.key(cas_version::formats::METAFORMAT.as_bytes());
    s.extend(short(&meta));
    for f in cas_version::formats::all() {
        s.extend(short(f.id()));
        s.extend(short(&b3.key(f.spec.as_bytes())));
        for (spec, _, id) in f.older_versions() {
            s.extend(short(id));
            s.extend(short(&b3.key(spec.as_bytes())));
        }
    }
    s
});

/// Whether `rest`, a variable's name in a project, is readable on the wire.
pub fn readable_var(rest: &str) -> bool {
    !(rest == "meta" || rest.starts_with("m/"))
}

#[derive(Default)]
struct Set {
    keys: HashSet<Short>,
}

#[derive(Default)]
pub struct Index {
    sets: Mutex<HashMap<ProjectId, Arc<RwLock<Option<Set>>>>>,
}

/// Add everything `root` reaches to `set`, stopping at what it holds.
/// Blobs the store lacks (a partial copy) are leaves.
fn walk(store: &dyn Store, root: &View, set: &mut HashSet<Short>) {
    let mut stack = vec![root.clone()];
    while let Some(v) = stack.pop() {
        set.extend(short(v.format()));
        let Some(k) = short(v.key()) else { continue };
        if !set.insert(k) {
            continue;
        }
        if let Ok(children) = cas_version::sync::children(store, &v) {
            stack.extend(children);
        }
    }
}

impl Index {
    fn entry(&self, p: &ProjectId) -> Arc<RwLock<Option<Set>>> {
        self.sets.lock().unwrap().entry(p.clone()).or_default().clone()
    }

    fn build(store: &dyn Store, p: &ProjectId) -> Set {
        let mut set = Set::default();
        let prefix = p.prefix();
        for var in store.list_all(&Span::prefix(prefix.as_bytes())).unwrap_or_default() {
            if !readable_var(&var[prefix.len()..]) {
                continue;
            }
            let Ok(Some(bytes)) = store.read(&var) else { continue };
            if let Some(v) = std::str::from_utf8(&bytes).ok().and_then(|s| View::parse(s).ok()) {
                walk(store, &v, &mut set.keys);
            }
        }
        set
    }

    /// Whether a readable variable of `p` reaches blob `key`.
    pub fn reaches(&self, store: &dyn Store, p: &ProjectId, key: &str) -> bool {
        let Some(k) = short(key) else { return false };
        if ALWAYS.contains(&k) {
            return true;
        }
        let e = self.entry(p);
        if let Some(s) = &*e.read().unwrap() {
            return s.keys.contains(&k);
        }
        let mut w = e.write().unwrap();
        if w.is_none() {
            *w = Some(Index::build(store, p));
        }
        w.as_ref().unwrap().keys.contains(&k)
    }

    pub fn always(key: &str) -> bool {
        short(key).is_some_and(|k| ALWAYS.contains(&k))
    }

    /// A variable of `p` now holds `v`.
    pub fn add(&self, store: &dyn Store, p: &ProjectId, v: &View) {
        let e = self.entry(p);
        let mut w = e.write().unwrap();
        if let Some(s) = w.as_mut() {
            walk(store, v, &mut s.keys);
        }
    }

    /// Drop `p`'s set; it is built again when next asked for.
    pub fn forget(&self, p: &ProjectId) {
        self.sets.lock().unwrap().remove(p);
    }

    /// Build the sets of `projects` now, rather than at their first read.
    pub fn warm(&self, store: &dyn Store, projects: &[ProjectId]) {
        for p in projects {
            let e = self.entry(p);
            let mut w = e.write().unwrap();
            if w.is_none() {
                *w = Some(Index::build(store, p));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn short_keys() {
        let a = super::short("blake3:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff").unwrap();
        let b = super::short("sha256:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff").unwrap();
        assert_ne!(a, b);
        assert!(super::short("nope").is_none());
    }
}
