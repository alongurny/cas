//! Checks for the version format (spec/version.md, section 6), comparing
//! every operation with a plain in-memory model of a path map.

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Expect, ListPage, Policy, Session, Span, Store, Who};
use cas_version::formats::{self, Format, METAFORMAT, git_blob};
use cas_version::{Change, Edit, Error, Node, Path, View, build, diff, fs, lookup, merge_diff, put, update, walk};
use proptest::prelude::*;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

type Map = BTreeMap<Path, Node>;

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn dir() -> &'static Format {
    formats::by_alias("dir").unwrap()
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

/// A file view in one of the leaf formats (contents are not read).
fn file(c: u8, alias: &str) -> Node {
    let f = formats::by_alias(alias).unwrap();
    Node::File(View::new(f.id(), b3().key(&[c])).unwrap())
}

fn node_strategy() -> impl Strategy<Value = Node> {
    prop_oneof![
        1 => Just(Node::Dir),
        4 => (0u8..4, prop::sample::select(&["raw", "exec", "symlink"][..])).prop_map(|(c, a)| file(c, a)),
    ]
}

/// Every parent of a path is a directory: drop what lies under a file, and
/// add the missing directories.
fn closed(raw: Vec<(Path, Node)>) -> Map {
    let mut m: Map = raw.into_iter().collect();
    let files: Vec<Path> = m.iter().filter(|(_, n)| matches!(n, Node::File(_))).map(|(p, _)| p.clone()).collect();
    m.retain(|p, _| !files.iter().any(|f| f != p && p.starts_with(f)));
    let paths: Vec<Path> = m.keys().cloned().collect();
    for p in paths {
        let mut q = p.parent();
        while let Some(d) = q {
            q = d.parent();
            m.insert(d, Node::Dir);
        }
    }
    m
}

fn map_strategy() -> impl Strategy<Value = Map> {
    prop::collection::vec((path_strategy(), node_strategy()), 0..24).prop_map(closed)
}

fn edits_strategy() -> impl Strategy<Value = BTreeMap<Path, Edit>> {
    let edit = prop_oneof![3 => node_strategy().prop_map(Edit::Set), 1 => Just(Edit::Remove)];
    prop::collection::btree_map(path_strategy(), edit, 0..8)
}

/// The model of `update`: removes first, then sets in canonical order.
fn model_update(m: &Map, edits: &BTreeMap<Path, Edit>) -> Option<Map> {
    let mut out = m.clone();
    for (p, e) in edits {
        if *e == Edit::Remove {
            out.retain(|q, _| !q.starts_with(p));
        }
    }
    for (p, e) in edits {
        let Edit::Set(node) = e else { continue };
        let mut q = p.parent();
        while let Some(d) = q {
            match out.get(&d) {
                Some(Node::File(_)) => return None,
                Some(Node::Dir) => {}
                None => {
                    out.insert(d.clone(), Node::Dir);
                }
            }
            q = d.parent();
        }
        match node {
            Node::Dir if out.get(p) == Some(&Node::Dir) => {}
            _ => {
                out.retain(|q, _| !q.starts_with(p));
                out.insert(p.clone(), node.clone());
            }
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

fn walked(s: &dyn Store, root: &View, prefix: Option<&Path>) -> Vec<(Path, Node)> {
    walk(s, root, prefix).map(|r| r.unwrap()).collect()
}

fn as_vec(m: &Map) -> Vec<(Path, Node)> {
    m.clone().into_iter().collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn roundtrip(m in map_strategy()) {
        let s = store();
        let root = build(&s, b3(), dir(), &m).unwrap();
        prop_assert_eq!(walked(&s, &root, None), as_vec(&m));
        for a in NAMES {
            for p in std::iter::once(a.to_string()).chain(NAMES.iter().map(|b| format!("{a}/{b}"))) {
                let p = Path::new(p).unwrap();
                prop_assert_eq!(lookup(&s, &root, &p).unwrap(), m.get(&p).cloned());
            }
        }
    }

    #[test]
    fn history_independent(m in map_strategy(), extra in map_strategy(), order in prop::collection::vec(any::<usize>(), 48)) {
        let s = store();
        let direct = build(&s, b3(), dir(), &m).unwrap();
        // Set the entries one at a time in a scrambled order, after paths
        // outside `m` that are removed again at the end.
        let mut root = build(&s, b3(), dir(), &Map::new()).unwrap();
        let extra: Vec<Path> = extra.keys().filter(|p| m.keys().all(|q| !p.starts_with(q) && !q.starts_with(p))).cloned().collect();
        for p in &extra {
            root = update(&s, b3(), &root, &BTreeMap::from([(p.clone(), Edit::Set(Node::Dir))])).unwrap();
        }
        let mut entries: Vec<(&Path, &Node)> = m.iter().collect();
        for (i, o) in (0..entries.len()).zip(order.iter().cycle()) {
            let j = i + o % (entries.len() - i);
            entries.swap(i, j);
        }
        for (p, n) in entries {
            root = update(&s, b3(), &root, &BTreeMap::from([(p.clone(), Edit::Set(n.clone()))])).unwrap();
        }
        for p in &extra {
            root = update(&s, b3(), &root, &BTreeMap::from([(p.clone(), Edit::Remove)])).unwrap();
        }
        prop_assert_eq!(root, direct);
    }

    #[test]
    fn update_matches_model(m in map_strategy(), edits in edits_strategy()) {
        let s = store();
        let root = build(&s, b3(), dir(), &m).unwrap();
        match (update(&s, b3(), &root, &edits), model_update(&m, &edits)) {
            (Ok(new), Some(want)) => {
                prop_assert_eq!(walked(&s, &new, None), as_vec(&want));
                prop_assert_eq!(new, build(&s, b3(), dir(), &want).unwrap());
            }
            (Err(Error::PathConflict(_)), None) => {}
            (got, want) => panic!("update gave {got:?}, model {want:?}"),
        }
    }

    #[test]
    fn prefix_walk_and_diff(a in map_strategy(), edits in edits_strategy(), prefix in path_strategy()) {
        let s = store();
        let b = model_update(&a, &edits).unwrap_or_else(|| a.clone());
        let (ra, rb) = (build(&s, b3(), dir(), &a).unwrap(), build(&s, b3(), dir(), &b).unwrap());
        let want: Vec<(Path, Node)> = a.iter().filter(|(p, _)| p.starts_with(&prefix)).map(|(p, n)| (p.clone(), n.clone())).collect();
        prop_assert_eq!(walked(&s, &ra, Some(&prefix)), want);
        for pre in [None, Some(&prefix)] {
            let want = model_diff(&a, &b, pre);
            prop_assert_eq!(&diff(&s, &ra, &rb, pre).unwrap(), &want);
            prop_assert_eq!(&merge_diff(walk(&s, &ra, pre), walk(&s, &rb, pre)).unwrap(), &want);
        }
    }
}

/// A wide, deep version: one file changed, a handful of blobs touched.
fn big() -> Map {
    let mut m = Vec::new();
    for i in 0..20 {
        for j in 0..20 {
            for k in 0..5u8 {
                m.push((Path::new(format!("d{i}/e{j}/f{k}")).unwrap(), file(k, "raw")));
            }
        }
    }
    closed(m)
}

#[test]
fn cost_scales_with_the_change() {
    let s = store();
    let m = big();
    let root = build(&s, b3(), dir(), &m).unwrap();
    let p = Path::new("d7/e3/f2").unwrap();
    let n = file(9, "exec");

    let c = Counting::new(&s);
    let new = update(&c, b3(), &root, &BTreeMap::from([(p.clone(), Edit::Set(n.clone()))])).unwrap();
    let (reads, writes) = (c.reads.load(Ordering::SeqCst), c.writes.load(Ordering::SeqCst));
    assert!(reads <= 3 && writes <= 3, "update read {reads}, wrote {writes} blobs");

    let c = Counting::new(&s);
    let d = diff(&c, &root, &new, None).unwrap();
    assert_eq!(d, vec![Change { path: p.clone(), old: m.get(&p).cloned(), new: Some(n.clone()) }]);
    assert!(c.reads.load(Ordering::SeqCst) <= 6);

    let c = Counting::new(&s);
    assert_eq!(lookup(&c, &new, &p).unwrap(), Some(n));
    assert!(c.reads.load(Ordering::SeqCst) <= 3);

    // A sparse walk reads only the part it returns.
    let c = Counting::new(&s);
    assert_eq!(walk(&c, &new, Some(&Path::new("d7/e3").unwrap())).count(), 6);
    assert!(c.reads.load(Ordering::SeqCst) <= 3);
}

#[test]
fn empty_directories_are_kept() {
    let s = store();
    let m = closed(vec![(Path::new("a/b/c").unwrap(), Node::Dir), (Path::new("x").unwrap(), Node::Dir)]);
    let root = build(&s, b3(), dir(), &m).unwrap();
    assert_eq!(walked(&s, &root, None).len(), 4);
    // Removing a directory's last file leaves the directory.
    let p = Path::new("a/b/c/f").unwrap();
    let with = update(&s, b3(), &root, &BTreeMap::from([(p.clone(), Edit::Set(file(1, "raw")))])).unwrap();
    let without = update(&s, b3(), &with, &BTreeMap::from([(p, Edit::Remove)])).unwrap();
    assert_eq!(without, root);
    assert_eq!(s.read(build(&s, b3(), dir(), &Map::new()).unwrap().key()).unwrap().unwrap(), b"");
}

#[test]
fn dir_encoding_is_readable_and_canonical() {
    let s = store();
    let f = file(1, "raw");
    let Node::File(fr) = &f else { unreachable!() };
    let m = closed(vec![(Path::new("b c").unwrap(), f.clone()), (Path::new("a/x").unwrap(), Node::Dir)]);
    let root = build(&s, b3(), dir(), &m).unwrap();
    let empty = b3().key(b"");
    let sub = b3().key(format!("{} {empty} x\n", dir().id()).as_bytes());
    let text = String::from_utf8(s.read(root.key()).unwrap().unwrap()).unwrap();
    assert_eq!(text, format!("{} {sub} a\n{fr} b c\n", dir().id()));

    let k = fr.to_string();
    let bad = [
        format!("{k} x"),
        format!("{k} b\n{k} a\n"),
        format!("{k} a\n{k} a\n"),
        format!("{} a\n", &k[..k.len() - 2]),
        format!("raw {} a\n", fr.key()),
        format!("{k} .\n"),
        format!("{k} a/b\n"),
        format!("{k}\n"),
        format!("\n{k} a\n"),
    ];
    for text in bad {
        let key = b3().key(text.as_bytes());
        s.write(&key, text.as_bytes(), &Expect::Absent).unwrap();
        let r = View::new(dir().id(), key).unwrap();
        assert!(matches!(lookup(&s, &r, &Path::new("a").unwrap()), Err(Error::Malformed { .. })), "{text:?}");
        assert!(dir().check(r.key(), text.as_bytes()).is_err());
    }
}

#[test]
fn format_ids_follow_the_metaformat() {
    let s = store();
    formats::publish(&s).unwrap();
    assert_eq!(formats::metaformat_id(), b3().key(METAFORMAT.as_bytes()));
    let mut ids = std::collections::BTreeSet::new();
    for f in formats::all() {
        let object = String::from_utf8(s.read(f.id()).unwrap().unwrap()).unwrap();
        let lines: Vec<&str> = object.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], format!("name {}", f.alias));
        assert_eq!(lines[1], "media text/markdown");
        let spec = lines[2].strip_prefix("spec ").unwrap();
        assert_eq!(s.read(spec).unwrap().unwrap(), f.spec.as_bytes());
        assert_eq!(object, f.object());
        assert!(ids.insert(f.id().to_string()));
    }
}

#[test]
fn unknown_formats_are_opaque_files() {
    let s = store();
    let stranger = View::new(b3().key(b"some format object"), b3().key(b"x")).unwrap();
    let m = closed(vec![(Path::new("doc.pdf").unwrap(), Node::File(stranger.clone()))]);
    let root = build(&s, b3(), dir(), &m).unwrap();
    assert_eq!(walked(&s, &root, None), as_vec(&m));
    assert_eq!(lookup(&s, &root, &Path::new("doc.pdf/inner").unwrap()).unwrap(), None);
    // A root has to be a directory format this crate knows.
    assert!(matches!(lookup(&s, &stranger, &Path::new("a").unwrap()), Err(Error::UnknownFormat(_))));
    let out = tempfile::tempdir().unwrap();
    assert!(matches!(fs::checkout(&s, &root, None, out.path()), Err(Error::UnknownFormat(_))));
}

#[test]
fn bad_input() {
    let s = store();
    let missing = View::new(dir().id(), b3().key(b"absent")).unwrap();
    assert_eq!(lookup(&s, &missing, &Path::new("a").unwrap()), Err(Error::Missing(missing.key().into())));
    for bad in ["", "/a", "a/", "a//b", ".", "a/../b", "a\nb"] {
        assert!(Path::new(bad).is_err(), "{bad:?}");
    }
    assert!(View::new("raw", b3().key(b"x")).is_err());
    assert!(View::parse(&format!("{} {}", dir().id(), b3().key(b""))).is_ok());
    assert!(matches!(put(&s, b3(), "symlink", b""), Err(Error::Malformed { .. })));
    assert!(matches!(put(&s, b3(), "git-blob", b"blob 3\0ab"), Err(Error::Malformed { .. })));
    assert!(matches!(put(&s, b3(), "git-blob", b"blob 02\0ab"), Err(Error::Malformed { .. })));
}

#[test]
fn git_blob_keys_are_git_object_ids() {
    let s = store();
    let r = put(&s, algo::lookup("sha1").unwrap(), "git-blob", &git_blob(b"hello\n")).unwrap();
    // `printf 'hello\n' | git hash-object --stdin`
    assert_eq!(r.key(), "sha1:ce013625030ba8dba906f756967f9e9ca394464a");
    let root = build(&s, b3(), dir(), &closed(vec![(Path::new("hi").unwrap(), Node::File(r))])).unwrap();
    let out = tempfile::tempdir().unwrap();
    fs::checkout(&s, &root, None, out.path()).unwrap();
    assert_eq!(std::fs::read(out.path().join("hi")).unwrap(), b"hello\n");
}

#[test]
fn import_and_sparse_checkout() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let src = tempfile::tempdir().unwrap();
    let d = src.path();
    std::fs::create_dir_all(d.join("src/bin")).unwrap();
    std::fs::create_dir_all(d.join("src/empty")).unwrap();
    std::fs::write(d.join("README"), "hi\n").unwrap();
    std::fs::write(d.join("src/lib.rs"), "fn x() {}\n").unwrap();
    std::fs::write(d.join("src/bin/run"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(d.join("src/bin/run"), std::fs::Permissions::from_mode(0o755)).unwrap();
    symlink("../README", d.join("src/link")).unwrap();

    let s = store();
    let root = fs::import(&s, b3(), dir(), d).unwrap();
    let kinds: Vec<(String, &str)> = walked(&s, &root, None)
        .into_iter()
        .map(|(p, n)| {
            let alias = match n {
                Node::Dir => "dir",
                Node::File(r) => r.known_format().unwrap().alias,
            };
            (p.to_string(), alias)
        })
        .collect();
    let want = [
        ("README", "raw"),
        ("src", "dir"),
        ("src/bin", "dir"),
        ("src/bin/run", "exec"),
        ("src/empty", "dir"),
        ("src/lib.rs", "raw"),
        ("src/link", "symlink"),
    ];
    assert_eq!(kinds, want.map(|(p, a)| (p.to_string(), a)));

    let out = tempfile::tempdir().unwrap();
    assert_eq!(fs::checkout(&s, &root, Some(&Path::new("src").unwrap()), out.path()).unwrap(), 6);
    assert!(!out.path().join("README").exists());
    assert!(out.path().join("src/empty").is_dir());
    assert_eq!(std::fs::read_to_string(out.path().join("src/lib.rs")).unwrap(), "fn x() {}\n");
    let run = std::fs::metadata(out.path().join("src/bin/run")).unwrap();
    assert_eq!(run.permissions().mode() & 0o777, 0o755);
    assert_eq!(std::fs::read_link(out.path().join("src/link")).unwrap().to_str(), Some("../README"));

    // A full checkout imports back to the same version.
    let full = tempfile::tempdir().unwrap();
    fs::checkout(&s, &root, None, full.path()).unwrap();
    assert_eq!(fs::import(&s, b3(), dir(), full.path()).unwrap(), root);
}
