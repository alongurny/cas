//! Checks every shape in `SHAPES` must pass (spec/version.md, section 6).
//! Each property runs against a plain in-memory model of a path map.

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Expect, ListPage, Policy, Session, Span, Store, Who};
use cas_version::{Change, Edit, Entry, Error, Mode, Path, SHAPES, Shape, diff, fs, lookup, merge_diff, update, walk};
use proptest::prelude::*;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

type Map = BTreeMap<Path, Entry>;

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn store() -> Session {
    CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous())
}

/// Counts reads and blob writes, to check that cost follows the change.
struct Counting<'s> {
    inner: &'s dyn Store,
    reads: AtomicUsize,
    writes: AtomicUsize,
}

impl<'s> Counting<'s> {
    fn new(inner: &'s dyn Store) -> Counting<'s> {
        Counting { inner, reads: AtomicUsize::new(0), writes: AtomicUsize::new(0) }
    }
}

impl Store for Counting<'_> {
    fn read(&self, key: &str) -> cas_core::Result<Option<Vec<u8>>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.read(key)
    }
    fn write(&self, key: &str, value: &[u8], expect: &Expect) -> cas_core::Result<()> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.inner.write(key, value, expect)
    }
    fn remove(&self, key: &str, expect: &Expect) -> cas_core::Result<()> {
        self.inner.remove(key, expect)
    }
    fn list(&self, span: &Span) -> cas_core::Result<ListPage> {
        self.inner.list(span)
    }
}

// Names chosen so that bytewise path order and canonical order differ
// ('-' and '.' sort before '/').
const NAMES: &[&str] = &["a", "b", "a-b", "a.b", "c d", "é"];

fn path_strategy() -> impl Strategy<Value = Path> {
    prop::collection::vec(prop::sample::select(NAMES), 1..4).prop_map(|n| Path::new(n.join("/")).unwrap())
}

fn entry_strategy() -> impl Strategy<Value = Entry> {
    (0u8..4, prop::sample::select(&[Mode::File, Mode::Exec, Mode::Link][..]))
        .prop_map(|(c, m)| Entry::new(b3().key(&[c]), m).unwrap())
}

/// Drop paths under another path in the map, so it is a valid version.
fn consistent(raw: Vec<(Path, Entry)>) -> Map {
    let mut m = Map::new();
    for (p, e) in raw {
        m.insert(p, e);
    }
    let paths: Vec<Path> = m.keys().cloned().collect();
    m.retain(|p, _| !paths.iter().any(|q| q != p && p.starts_with(q)));
    m
}

fn map_strategy() -> impl Strategy<Value = Map> {
    prop::collection::vec((path_strategy(), entry_strategy()), 0..24).prop_map(consistent)
}

fn edits_strategy() -> impl Strategy<Value = BTreeMap<Path, Edit>> {
    let edit = prop_oneof![entry_strategy().prop_map(Edit::Set), Just(Edit::Remove)];
    prop::collection::btree_map(path_strategy(), edit, 0..8)
}

/// The model of `update`: removes first, then sets in canonical order; a
/// set under a file, or under another set in the batch, is a conflict.
fn model_update(m: &Map, edits: &BTreeMap<Path, Edit>) -> Option<Map> {
    let mut out = m.clone();
    for (p, e) in edits {
        if *e == Edit::Remove {
            out.retain(|q, _| !q.starts_with(p));
        }
    }
    for (p, e) in edits {
        if let Edit::Set(entry) = e {
            if out.keys().any(|q| q != p && p.starts_with(q)) {
                return None;
            }
            out.retain(|q, _| !q.starts_with(p));
            out.insert(p.clone(), entry.clone());
        }
    }
    Some(out)
}

fn model_diff(a: &Map, b: &Map, prefix: Option<&Path>) -> Vec<Change> {
    let mut paths: Vec<&Path> = a.keys().chain(b.keys()).collect();
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .filter(|p| prefix.is_none_or(|x| p.starts_with(x)))
        .filter(|p| a.get(*p) != b.get(*p))
        .map(|p| Change { path: p.clone(), old: a.get(p).cloned(), new: b.get(p).cloned() })
        .collect()
}

fn walked(s: &dyn Store, root: &str, prefix: Option<&Path>) -> Map {
    walk(s, root, prefix).unwrap().map(|r| r.unwrap()).collect()
}

fn check_roundtrip(shape: &dyn Shape, m: &Map) {
    let s = store();
    let root = shape.build(&s, b3(), m).unwrap();
    let got: Vec<(Path, Entry)> = walk(&s, &root, None).unwrap().map(|r| r.unwrap()).collect();
    let want: Vec<(Path, Entry)> = m.clone().into_iter().collect();
    assert_eq!(got, want, "walk order and contents");
    for p in NAMES
        .iter()
        .flat_map(|a| NAMES.iter().map(move |b| format!("{a}/{b}")))
        .chain(NAMES.iter().map(|n| n.to_string()))
    {
        let p = Path::new(p).unwrap();
        assert_eq!(lookup(&s, &root, &p).unwrap(), m.get(&p).cloned(), "lookup {p}");
    }
}

fn check_history_independence(shape: &dyn Shape, m: &Map, extra: &Map, order: &[usize]) {
    let s = store();
    let direct = shape.build(&s, b3(), m).unwrap();
    // Insert the entries one by one in a scrambled order, with extra paths
    // that are then removed again.
    let mut root = shape.build(&s, b3(), &Map::new()).unwrap();
    let entries: Vec<(&Path, &Entry)> = m.iter().collect();
    let mut idx: Vec<usize> = (0..entries.len()).collect();
    for (i, &o) in order.iter().enumerate().take(idx.len()) {
        let j = i + o % (idx.len() - i);
        idx.swap(i, j);
    }
    let extra: Map = extra
        .iter()
        .filter(|(p, _)| m.keys().all(|q| !p.starts_with(q) && !q.starts_with(p)))
        .map(|(p, e)| (p.clone(), e.clone()))
        .collect();
    let extra = consistent(extra.into_iter().collect());
    for (p, e) in &extra {
        root = update(&s, b3(), &root, &BTreeMap::from([(p.clone(), Edit::Set(e.clone()))])).unwrap();
    }
    for i in idx {
        let (p, e) = entries[i];
        root = update(&s, b3(), &root, &BTreeMap::from([(p.clone(), Edit::Set(e.clone()))])).unwrap();
    }
    for p in extra.keys() {
        root = update(&s, b3(), &root, &BTreeMap::from([(p.clone(), Edit::Remove)])).unwrap();
    }
    assert_eq!(root, direct);
}

fn check_update(shape: &dyn Shape, m: &Map, edits: &BTreeMap<Path, Edit>) {
    let s = store();
    let root = shape.build(&s, b3(), m).unwrap();
    match (update(&s, b3(), &root, edits), model_update(m, edits)) {
        (Ok(new), Some(want)) => {
            assert_eq!(walked(&s, &new, None), want);
            assert_eq!(new, shape.build(&s, b3(), &want).unwrap(), "canonical after update");
        }
        (Err(Error::PathConflict(_)), None) => {}
        (got, want) => panic!("update gave {got:?}, model {want:?}"),
    }
}

fn check_prefix_and_diff(shape: &dyn Shape, a: &Map, b: &Map, prefix: &Path) {
    let s = store();
    let (ra, rb) = (shape.build(&s, b3(), a).unwrap(), shape.build(&s, b3(), b).unwrap());
    let want: Map = a.iter().filter(|(p, _)| p.starts_with(prefix)).map(|(p, e)| (p.clone(), e.clone())).collect();
    assert_eq!(walked(&s, &ra, Some(prefix)), want, "walk under {prefix}");
    // A prefix that is a file walks to that file alone.
    for p in a.keys() {
        assert_eq!(walked(&s, &ra, Some(p)), BTreeMap::from([(p.clone(), a[p].clone())]));
    }
    for pre in [None, Some(prefix)] {
        let want = model_diff(a, b, pre);
        assert_eq!(diff(&s, &ra, &rb, pre).unwrap(), want, "diff under {pre:?}");
        assert_eq!(shape.diff(&s, &ra, &rb, pre).unwrap(), want);
        let naive = merge_diff(walk(&s, &ra, pre).unwrap(), walk(&s, &rb, pre).unwrap()).unwrap();
        assert_eq!(naive, want);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn roundtrip(m in map_strategy()) {
        for shape in SHAPES { check_roundtrip(*shape, &m); }
    }

    #[test]
    fn history_independent(m in map_strategy(), extra in map_strategy(), order in prop::collection::vec(any::<usize>(), 24)) {
        for shape in SHAPES { check_history_independence(*shape, &m, &extra, &order); }
    }

    #[test]
    fn update_matches_model(m in map_strategy(), edits in edits_strategy()) {
        for shape in SHAPES { check_update(*shape, &m, &edits); }
    }

    #[test]
    fn prefix_walk_and_diff(a in map_strategy(), edits in edits_strategy(), prefix in path_strategy()) {
        let b = model_update(&a, &edits).unwrap_or_else(|| a.clone());
        for shape in SHAPES { check_prefix_and_diff(*shape, &a, &b, &prefix); }
    }
}

/// A wide, deep version: one file changed, a handful of nodes touched.
fn big() -> Map {
    let mut m = Map::new();
    for i in 0..20 {
        for j in 0..20 {
            for k in 0..5 {
                let p = Path::new(format!("d{i}/e{j}/f{k}")).unwrap();
                m.insert(p, Entry::new(b3().key(format!("{i} {j} {k}").as_bytes()), Mode::File).unwrap());
            }
        }
    }
    m
}

#[test]
fn cost_scales_with_the_change() {
    for shape in SHAPES {
        let s = store();
        let m = big();
        let root = shape.build(&s, b3(), &m).unwrap();
        let p = Path::new("d7/e3/f2").unwrap();
        let e = Entry::new(b3().key(b"changed"), Mode::File).unwrap();

        let c = Counting::new(&s);
        let new = shape.update(&c, b3(), &root, &BTreeMap::from([(p.clone(), Edit::Set(e.clone()))])).unwrap();
        let (reads, writes) = (c.reads.load(Ordering::SeqCst), c.writes.load(Ordering::SeqCst));
        assert!(reads <= 8 && writes <= 8, "{}: update read {reads}, wrote {writes} blobs", shape.name());

        let c = Counting::new(&s);
        let d = shape.diff(&c, &root, &new, None).unwrap();
        assert_eq!(d, vec![Change { path: p.clone(), old: m.get(&p).cloned(), new: Some(e.clone()) }]);
        let reads = c.reads.load(Ordering::SeqCst);
        assert!(reads <= 16, "{}: diff read {reads} blobs", shape.name());

        let c = Counting::new(&s);
        assert_eq!(shape.lookup(&c, &new, &p).unwrap(), Some(e));
        assert!(c.reads.load(Ordering::SeqCst) <= 8);

        // A sparse walk reads only the part it returns.
        let c = Counting::new(&s);
        let n = shape.walk(&c, &new, Some(&Path::new("d7/e3").unwrap())).count();
        assert_eq!(n, 5);
        assert!(c.reads.load(Ordering::SeqCst) <= 8);
    }
}

#[test]
fn algorithm_is_the_writers_choice() {
    for shape in SHAPES {
        let s = store();
        let m = big();
        let sha = algo::lookup("sha256").unwrap();
        let (a, b) = (shape.build(&s, b3(), &m).unwrap(), shape.build(&s, sha, &m).unwrap());
        assert!(a.starts_with("blake3:") && b.starts_with("sha256:"));
        assert_eq!(walked(&s, &a, None), walked(&s, &b, None));
        assert_eq!(diff(&s, &a, &b, None).unwrap(), vec![]);
    }
}

#[test]
fn unknown_shape_and_bad_input() {
    let s = store();
    let text = b"cas-tree nope/1\n";
    let key = b3().key(text);
    s.write(&key, text, &Expect::Absent).unwrap();
    assert_eq!(lookup(&s, &key, &Path::new("a").unwrap()), Err(Error::UnknownShape("nope/1".into())));
    let missing = b3().key(b"absent");
    assert_eq!(lookup(&s, &missing, &Path::new("a").unwrap()), Err(Error::Missing(missing)));
    assert!(matches!(lookup(&s, "main", &Path::new("a").unwrap()), Err(Error::BadKey(_))));
    for bad in ["", "/a", "a/", "a//b", ".", "a/../b", "a\nb"] {
        assert!(Path::new(bad).is_err(), "{bad:?}");
    }
    assert!(Entry::new("main", Mode::File).is_err());
}

#[test]
fn import_and_sparse_checkout() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let src = tempfile::tempdir().unwrap();
    let d = src.path();
    std::fs::create_dir_all(d.join("src/bin")).unwrap();
    std::fs::create_dir_all(d.join("empty")).unwrap();
    std::fs::write(d.join("README"), "hi\n").unwrap();
    std::fs::write(d.join("src/lib.rs"), "fn x() {}\n").unwrap();
    std::fs::write(d.join("src/bin/run"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(d.join("src/bin/run"), std::fs::Permissions::from_mode(0o755)).unwrap();
    symlink("../README", d.join("src/link")).unwrap();

    for shape in SHAPES {
        let s = store();
        let root = fs::import(&s, b3(), *shape, d).unwrap();
        let m = walked(&s, &root, None);
        let modes: Vec<(&str, Mode)> = m.iter().map(|(p, e)| (p.as_str(), e.mode)).collect();
        assert_eq!(
            modes,
            vec![
                ("README", Mode::File),
                ("src/bin/run", Mode::Exec),
                ("src/lib.rs", Mode::File),
                ("src/link", Mode::Link)
            ]
        );

        let out = tempfile::tempdir().unwrap();
        let n = fs::checkout(&s, &root, Some(&Path::new("src").unwrap()), out.path()).unwrap();
        assert_eq!(n, 3);
        assert!(!out.path().join("README").exists());
        assert_eq!(std::fs::read_to_string(out.path().join("src/lib.rs")).unwrap(), "fn x() {}\n");
        let run = std::fs::metadata(out.path().join("src/bin/run")).unwrap();
        assert_eq!(run.permissions().mode() & 0o777, 0o755);
        assert_eq!(std::fs::read_link(out.path().join("src/link")).unwrap().to_str(), Some("../README"));

        // A checkout imports back to the same version (under its prefix).
        let full = tempfile::tempdir().unwrap();
        fs::checkout(&s, &root, None, full.path()).unwrap();
        assert_eq!(fs::import(&s, b3(), *shape, full.path()).unwrap(), root);
    }
}
