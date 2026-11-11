//! Manifests: what a proof covers, and what a commit brings.

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Policy, Session, Store, Who};
use cas_version::commits::{self, Commit, Notes};
use cas_version::proofs::{self, MANIFEST, Manifest};
use cas_version::stepnotes::PathNotes;
use cas_version::typed::{PROVEN, key_bytes};
use cas_version::{Edit, Node, Path, View, build, by_alias, derived, put, sync, update};
use std::collections::{BTreeMap, BTreeSet};

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn store() -> Session {
    CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous())
}

fn file(s: &dyn Store, text: &str) -> Node {
    Node::File(put(s, b3(), "raw", text.as_bytes()).unwrap())
}

fn set(s: &dyn Store, v: &View, path: &str, text: &str) -> View {
    let e = BTreeMap::from([(Path::new(path).unwrap(), Edit::Set(file(s, text)))]);
    update(s, b3(), v, &e).unwrap()
}

fn commit(s: &dyn Store, v: &View, parents: &[(&str, Option<String>)], notes: Option<String>, time: u64) -> String {
    let author = commits::put_text(s, b3(), "author", "Dana").unwrap();
    let parents = parents.iter().map(|(p, n)| (p.to_string(), n.clone())).collect();
    commits::write(s, b3(), &Commit { version: v.key().to_string(), author, notes, time, parents }).unwrap()
}

/// Every blob `tip` reaches that a proof can cover, with its type.
fn reach(s: &dyn Store, tip: &str) -> BTreeSet<(String, &'static str)> {
    let mut seen = BTreeSet::new();
    let mut todo = vec![commits::view("commit", tip)];
    while let Some(v) = todo.pop() {
        let alias = v.known_format().unwrap().alias;
        if !PROVEN.contains(&alias) || !seen.insert((v.key().to_string(), alias)) {
            continue;
        }
        todo.extend(sync::children(s, &v).unwrap());
    }
    seen
}

#[test]
fn a_manifest_has_one_encoding() {
    let mut m = Manifest::default();
    m.blobs.insert([7; 32], ("directory", 128));
    m.blobs.insert([3; 32], ("note", 5));
    let b = m.encode();
    assert_eq!(b.len(), 128);
    assert_eq!(Manifest::decode(&b).unwrap(), m);
    by_alias("manifest").unwrap().check("k", &b).unwrap();
    // Records out of order, a tag that names no type, and no record at all.
    let mut swapped = b[64..].to_vec();
    swapped.extend_from_slice(&b[..64]);
    assert!(Manifest::decode(&swapped).is_err());
    let mut tag = b.clone();
    tag[32] = 9;
    assert!(Manifest::decode(&tag).is_err());
    assert!(Manifest::decode(&[]).is_err());
}

#[test]
fn the_manifests_of_a_history_cover_what_it_reaches() {
    let s = store();
    let files = BTreeMap::from([
        (Path::new("README").unwrap(), file(&s, "hi\n")),
        (Path::new("src/a.rs").unwrap(), file(&s, "fn a() {}\n")),
        (Path::new("src/deep/b.rs").unwrap(), file(&s, "fn b() {}\n")),
        (Path::new("doc/c.md").unwrap(), file(&s, "c\n")),
    ]);
    let v1 = build(&s, b3(), by_alias("directory").unwrap(), &files).unwrap();
    let c1 = commit(&s, &v1, &[], None, 1);
    // One edit deep in the tree, with a note on the step.
    let v2 = set(&s, &v1, "src/deep/b.rs", "fn b() {}\nfn c() {}\n");
    let n = Notes {
        note: "add c".into(),
        paths: BTreeMap::from([(
            "src/deep/b.rs".to_string(),
            PathNotes { note: String::new(), old: None, new: Some(vec![(1, String::new()), (1, "new".into())]) },
        )]),
    };
    let notes = commits::write_notes(&s, b3(), &n, &|p| Ok(!p.ends_with(".rs"))).unwrap();
    let c2 = commit(&s, &v2, &[(&c1, notes)], None, 2);
    // A side line from the first commit, and a merge of the two.
    let v3 = set(&s, &v1, "doc/c.md", "c\nd\n");
    let c3 = commit(&s, &v3, &[(&c1, None)], None, 3);
    let v4 = set(&s, &v2, "doc/c.md", "c\nd\n");
    let c4 = commit(&s, &v4, &[(&c2, None), (&c3, None)], None, 4);

    let m = |k: &str| proofs::of_commit(&s, k).unwrap();
    let names = |m: &Manifest| -> Vec<&'static str> {
        let mut a: Vec<&'static str> = m.blobs.values().map(|x| x.0).collect();
        a.sort();
        a
    };
    // The first commit brings everything: itself, its author, four directories.
    assert_eq!(names(&m(&c1)), ["author", "commit", "directory", "directory", "directory", "directory"]);
    // The second: itself, the three directories on the edited path, its notes.
    let second = m(&c2);
    assert_eq!(names(&second).iter().filter(|a| **a == "directory").count(), 3);
    assert!(!second.blobs.contains_key(&key_bytes(&c1).unwrap()));
    assert!(names(&second).contains(&"annotation") && names(&second).contains(&"runs"));
    assert!(!names(&second).contains(&"author"));
    // The merge has doc/ as the side line has it, so it brings the root only.
    assert_eq!(names(&m(&c4)), ["commit", "directory"]);

    // Together they list exactly what the tip reaches.
    let mut listed = BTreeSet::new();
    for (k, _) in commits::log(&s, &c4).unwrap() {
        let man = m(&k);
        for (key, (alias, len)) in &man.blobs {
            let text = cas_version::typed::key_text(key);
            assert_eq!(cas_version::read_blob(&s, &text).unwrap().len() as u64, *len);
            listed.insert((text, *alias));
        }
        // As derived data: the same blob, found again under the commit.
        let view = derived::derive(&s, b3(), &MANIFEST, &commits::view("commit", &k)).unwrap();
        assert_eq!(proofs::read(&s, view.key()).unwrap(), man);
        assert_eq!(derived::cached(&s, &MANIFEST, &commits::view("commit", &k)).unwrap(), Some(view));
    }
    assert_eq!(listed, reach(&s, &c4));
}
