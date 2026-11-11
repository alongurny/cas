//! The `tree` directory format (spec/formats/tree.md): binary entries, big
//! directories cut into blobs by a hash of each name.

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Expect, Policy, Session, Store, Who};
use cas_version::dir::{Blob, Item, TREE, height, tree_decode, tree_encode};
use cas_version::formats::{self, Format};
use cas_version::{Edit, Node, Path, View, build, diff, lookup, put, update, walk};
use proptest::prelude::*;
use std::collections::BTreeMap;

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn store() -> Session {
    CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous())
}

fn tree() -> &'static Format {
    formats::by_alias("tree").unwrap()
}

fn count(s: &Session) -> usize {
    cas_core::list_all(s, &cas_core::Span::all()).unwrap().len()
}

/// A flat directory of `n` files.
fn flat(s: &Session, n: usize) -> BTreeMap<Path, Node> {
    (0..n)
        .map(|i| {
            let f = put(s, b3(), "raw", format!("file {i}").as_bytes()).unwrap();
            (Path::new(format!("name-{i:05}.c")).unwrap(), Node::File(f))
        })
        .collect()
}

/// Every `tree` blob under `v`, with its level.
fn blobs(s: &Session, v: &View, out: &mut Vec<(u64, usize)>) {
    match tree_decode(&s.read(v.key()).unwrap().unwrap()).unwrap() {
        Blob::Items(i) => out.push((0, i.len())),
        Blob::Parts { level, parts } => {
            out.push((level, parts.len()));
            for (_, k) in parts {
                blobs(s, &View::new(tree().id(), k).unwrap(), out);
            }
        }
    }
}

#[test]
fn a_big_directory_is_cut_and_reads_back() {
    let s = store();
    let m = flat(&s, 5000);
    let root = build(&s, b3(), tree(), &m).unwrap();
    let mut all = Vec::new();
    blobs(&s, &root, &mut all);
    let leaves: Vec<usize> = all.iter().filter(|(l, _)| *l == 0).map(|(_, n)| *n).collect();
    assert!((40..140).contains(&leaves.len()), "{} leaves", leaves.len());
    assert_eq!(leaves.iter().sum::<usize>(), 5000);
    assert!(all[0].0 >= 1, "an inner blob on top of 5000 names");
    let walked: Vec<_> = walk(&s, &root, None).map(Result::unwrap).collect();
    assert_eq!(walked, m.iter().map(|(p, n)| (p.clone(), n.clone())).collect::<Vec<_>>());
    for (p, n) in m.iter().step_by(97) {
        assert_eq!(lookup(&s, &root, p).unwrap().as_ref(), Some(n));
    }
    assert_eq!(lookup(&s, &root, &Path::new("zzz").unwrap()).unwrap(), None);
}

#[test]
fn one_change_rewrites_one_leaf_and_the_blobs_above() {
    let s = store();
    let m = flat(&s, 3000);
    let root = build(&s, b3(), tree(), &m).unwrap();
    let before = count(&s);
    let f = put(&s, b3(), "raw", b"changed").unwrap();
    let before = before + 1;
    let p = Path::new("name-01234.c").unwrap();
    let new = update(&s, b3(), &root, &BTreeMap::from([(p.clone(), Edit::Set(Node::File(f.clone())))])).unwrap();
    // A leaf, one inner blob, and the top: few, not the whole directory.
    let written = count(&s) - before;
    assert!(written <= 4, "{written} blobs written");
    assert_eq!(diff(&s, &root, &new, None).unwrap().len(), 1);
    assert_eq!(lookup(&s, &new, &p).unwrap(), Some(Node::File(f)));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// The blobs depend only on the entries: any order of edits arrives at
    /// the same key as a direct build.
    #[test]
    fn the_cut_depends_on_the_names_alone(
        n in 0usize..900,
        removed in proptest::collection::vec(0usize..900, 0..40),
    ) {
        let s = store();
        let mut m = flat(&s, n);
        let mut root = build(&s, b3(), tree(), &m).unwrap();
        for r in removed {
            let p = Path::new(format!("name-{r:05}.c")).unwrap();
            m.remove(&p);
            root = update(&s, b3(), &root, &BTreeMap::from([(p, Edit::Remove)])).unwrap();
            prop_assert_eq!(&root, &build(&s, b3(), tree(), &m).unwrap());
        }
    }

    /// Any blob a directory writes decodes to what it holds and is the
    /// only encoding of it.
    #[test]
    fn encoding_round_trips(names in proptest::collection::btree_set("[a-z.]{1,12}", 1..40)) {
        let s = store();
        let f = put(&s, b3(), "raw", b"x").unwrap();
        let items: Vec<Item> = names
            .iter()
            .filter(|n| *n != "." && *n != "..")
            .map(|n| Item { name: n.clone(), child: f.clone() })
            .collect();
        prop_assume!(!items.is_empty());
        let (_, blobs) = TREE.blobs(b3(), &items);
        for (_, b) in &blobs {
            let d = tree_decode(b).unwrap();
            prop_assert_eq!(&tree_encode(&d), b);
        }
    }
}

#[test]
fn bad_blobs_are_refused() {
    let s = store();
    let f = put(&s, b3(), "raw", b"x").unwrap();
    let item = |n: &str| Item { name: n.into(), child: f.clone() };
    // Names out of order, a bad name, a non-canonical varint, trailing bytes.
    let good = tree_encode(&Blob::Items(vec![item("a"), item("b")]));
    assert!(tree_decode(&good).is_ok());
    assert!(tree_decode(&tree_encode(&Blob::Items(vec![item("b"), item("a")]))).is_err());
    assert!(tree_decode(&tree_encode(&Blob::Items(vec![item("a/b")]))).is_err());
    assert!(tree_decode(&[good.clone(), vec![0]].concat()).is_err());
    let mut long = good.clone();
    long.splice(0..1, [0x80, 0x00]);
    assert!(tree_decode(&long).is_err());
    assert!(tree_decode(&[0]).is_err(), "an empty leaf is the empty blob");
    // A chunk end before the last name: not cut by name.
    let end = (0..).map(|i| format!("n{i}")).find(|n| height(n) > 0).unwrap();
    let names = [end.clone(), format!("{end}z")];
    if names[0] == end {
        assert!(tree_decode(&tree_encode(&Blob::Items(names.iter().map(|n| item(n)).collect()))).is_err());
    }
    // A wrong part in an inner blob is caught when the directory is read.
    let m = flat(&s, 3000);
    let root = build(&s, b3(), tree(), &m).unwrap();
    let Blob::Parts { level, mut parts } = tree_decode(&s.read(root.key()).unwrap().unwrap()).unwrap() else {
        panic!("a top inner blob")
    };
    parts.swap(0, 1);
    let bytes = tree_encode(&Blob::Parts { level, parts });
    let key = b3().key(&bytes);
    s.write(&key, &bytes, &Expect::Absent).unwrap();
    let bad = View::new(tree().id(), &key).unwrap();
    assert!(walk(&s, &bad, None).any(|r| r.is_err()));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// A blob with any byte changed, dropped or added is refused, or is the
    /// canonical encoding of what it decodes to: no two byte strings decode
    /// to the same directory.
    #[test]
    fn a_damaged_blob_is_refused_or_canonical(
        names in proptest::collection::btree_set("[a-c]{1,4}", 1..12),
        at in 0usize..400,
        byte in 0u8..=255,
        op in 0u8..3,
    ) {
        let s = store();
        let f = put(&s, b3(), "raw", b"x").unwrap();
        let g = put(&s, b3(), "exec", b"y").unwrap();
        let items: Vec<Item> = names
            .iter()
            .enumerate()
            .map(|(i, n)| Item { name: n.clone(), child: if i % 2 == 0 { f.clone() } else { g.clone() } })
            .collect();
        let mut b = tree_encode(&Blob::Items(items));
        let at = at % b.len();
        match op {
            0 => b[at] = byte,
            1 => { b.remove(at); }
            _ => b.insert(at, byte),
        }
        if let Ok(d) = tree_decode(&b) {
            prop_assert_eq!(tree_encode(&d), b);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// A diff of two big directories, which reads only the leaves that
    /// differ, finds exactly the entries that changed.
    #[test]
    fn a_diff_skipping_shared_leaves_finds_every_change(
        edits in proptest::collection::vec((0usize..3500, 0u8..3), 1..30),
    ) {
        let s = store();
        let m = flat(&s, 3000);
        let a = build(&s, b3(), tree(), &m).unwrap();
        let mut m2 = m.clone();
        let mut batch = BTreeMap::new();
        for (i, op) in edits {
            let p = Path::new(format!("name-{i:05}.c")).unwrap();
            let e = match op {
                0 => Edit::Remove,
                _ => Edit::Set(Node::File(put(&s, b3(), "raw", format!("new {i} {op}").as_bytes()).unwrap())),
            };
            match &e {
                Edit::Remove => m2.remove(&p),
                Edit::Set(n) => m2.insert(p.clone(), n.clone()),
            };
            batch.insert(p, e);
        }
        let b = update(&s, b3(), &a, &batch).unwrap();
        let got: Vec<(Path, Option<Node>, Option<Node>)> =
            diff(&s, &a, &b, None).unwrap().into_iter().map(|c| (c.path, c.old, c.new)).collect();
        let mut want = Vec::new();
        let paths: std::collections::BTreeSet<&Path> = m.keys().chain(m2.keys()).collect();
        for p in paths {
            let (o, n) = (m.get(p).cloned(), m2.get(p).cloned());
            if o != n {
                want.push((p.clone(), o, n));
            }
        }
        prop_assert_eq!(got, want);
    }
}
