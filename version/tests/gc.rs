//! Checks for reachability, collection and purging (spec/gc.md).

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Expect, Policy, Span, Store, Who, list_all};
use cas_version::commits;
use cas_version::{Node, Path, build, formats, gc, put, sync, walk};

mod common;
use std::collections::BTreeMap;
use std::sync::Arc;

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn store() -> Arc<dyn Store> {
    Arc::new(CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous()))
}

fn commit(s: &dyn Store, files: &[(&str, &str)]) -> String {
    let mut m = BTreeMap::new();
    for (p, c) in files {
        m.insert(Path::new(*p).unwrap(), Node::File(put(s, b3(), "raw", c.as_bytes()).unwrap()));
    }
    let v = build(s, b3(), formats::by_alias("directory").unwrap(), &m).unwrap();
    common::commit(s, "main", &v)
}

fn files(s: &dyn Store, v: &cas_version::View) -> Vec<(Path, cas_version::View)> {
    walk(s, v, None)
        .map(|r| r.unwrap())
        .filter_map(|(p, n)| match n {
            Node::File(r) => Some((p, r)),
            Node::Dir => None,
        })
        .collect()
}

fn blobs(s: &dyn Store) -> usize {
    list_all(s, &Span::all()).unwrap().iter().filter(|k| !k.contains('/') && k.contains(':')).count()
}

#[test]
fn reachable_blobs_survive_and_garbage_goes_after_two_runs() {
    let s = store();
    commit(&*s, &[("a", "kept\n"), ("b", "also\n")]);
    let garbage = put(&*s, b3(), "raw", b"nobody links me").unwrap();
    let before = blobs(&*s);

    let first = gc::collect(&*s, b3()).unwrap();
    assert!(first.removed.is_empty(), "the first run only marks");
    assert_eq!(first.unreachable, 1);
    assert!(sync::has(&*s, garbage.key()).unwrap());

    let second = gc::collect(&*s, b3()).unwrap();
    assert_eq!(second.removed, vec![garbage.key().to_string()]);
    assert!(!sync::has(&*s, garbage.key()).unwrap());
    // Everything the branch reaches is still readable.
    let v = common::tip_version(&*s, "main");
    assert_eq!(files(&*s, &v).len(), 2);
    // The garbage went; both pending sets are still there (the first is
    // unreachable now and goes two runs later), and the marked set of the last full run.
    assert_eq!(blobs(&*s), before - 1 + 2 + 1);
}

#[test]
fn a_blob_linked_between_runs_survives() {
    let s = store();
    commit(&*s, &[("a", "x\n")]);
    let late = put(&*s, b3(), "raw", b"linked soon\n").unwrap();
    gc::collect(&*s, b3()).unwrap();
    // The writer links it before the next run.
    s.write("keep", late.to_string().as_bytes(), &Expect::Absent).unwrap();
    let r = gc::collect(&*s, b3()).unwrap();
    assert!(!r.removed.contains(&late.key().to_string()));
    assert!(sync::has(&*s, late.key()).unwrap());
}

#[test]
fn history_and_format_objects_are_reachable() {
    let s = store();
    commit(&*s, &[("a", "1\n")]);
    commit(&*s, &[("a", "2\n")]);
    formats::publish(&*s).unwrap();
    let marked = gc::mark(&*s).unwrap();
    let tip = commits::tip(&*s, "main").unwrap().unwrap();
    for (k, c) in commits::log(&*s, &tip).unwrap() {
        assert!(marked.contains(&k));
        assert!(marked.contains(c.version().key()));
    }
    let dir = formats::by_alias("directory").unwrap();
    assert!(marked.contains(dir.id()));
}

/// Delta hints between big directories pair each changed file with
/// its old self by name, across leaves that differ, and skip what did not
/// change.
#[test]
fn hints_pair_changed_files_of_big_directories_by_name() {
    let s = store();
    let tree = formats::by_alias("directory").unwrap();
    let mut m = BTreeMap::new();
    for i in 0..3000 {
        m.insert(
            Path::new(format!("f{i:05}")).unwrap(),
            Node::File(put(&*s, b3(), "raw", format!("v1 {i}").as_bytes()).unwrap()),
        );
    }
    let v1 = build(&*s, b3(), tree, &m).unwrap();
    common::commit(&*s, "main", &v1);
    let changed = [7usize, 1500, 1501, 2999];
    let mut m2 = m.clone();
    for i in changed {
        m2.insert(
            Path::new(format!("f{i:05}")).unwrap(),
            Node::File(put(&*s, b3(), "raw", format!("v2 {i}").as_bytes()).unwrap()),
        );
    }
    // One removed and one added name too.
    m2.remove(&Path::new("f00100").unwrap());
    m2.insert(Path::new("f01500x").unwrap(), Node::File(put(&*s, b3(), "raw", b"new").unwrap()));
    let v2 = build(&*s, b3(), tree, &m2).unwrap();
    common::commit(&*s, "main", &v2);
    let (_, hints) = gc::delta_hints(&*s, &|_| false).unwrap();
    let key = |m: &BTreeMap<Path, Node>, i: usize| match &m[&Path::new(format!("f{i:05}")).unwrap()] {
        Node::File(f) => f.key().to_string(),
        Node::Dir => unreachable!(),
    };
    for i in changed {
        assert_eq!(hints.get(&key(&m2, i)), Some(&key(&m, i)), "f{i:05}");
    }
    // Unchanged files are hinted to nothing.
    assert!(!hints.contains_key(&key(&m2, 8)));
}

/// After a full run, a run walks only what came since, and still finds the
/// new garbage and keeps what new nodes reach.
#[test]
fn runs_after_a_full_one_walk_only_what_came_since() {
    let s = store();
    let many: Vec<(String, String)> = (0..60).map(|i| (format!("f{i}"), format!("{i}\n"))).collect();
    let files_of = |extra: &str| -> Vec<(String, String)> {
        let mut v = many.clone();
        v.push(("extra".into(), extra.into()));
        v
    };
    commit(&*s, &refs(&files_of("1\n")));
    assert!(gc::collect(&*s, b3()).unwrap().full);
    // New work: a node with a new file, and garbage.
    commit(&*s, &refs(&files_of("2\n")));
    let garbage = put(&*s, b3(), "raw", b"nobody links me either").unwrap();
    let first = gc::collect(&*s, b3()).unwrap();
    assert!(!first.full, "{first:?}");
    let second = gc::collect(&*s, b3()).unwrap();
    assert!(!second.full);
    assert!(second.removed.contains(&garbage.key().to_string()), "{second:?}");
    assert!(!sync::has(&*s, garbage.key()).unwrap());
    // Both nodes' files are all there.
    for (_, c) in commits::log(&*s, &commits::tip(&*s, "main").unwrap().unwrap()).unwrap() {
        assert_eq!(files(&*s, &c.version()).len(), 61);
    }
    // A full run removes no more than garbage.
    let full = gc::collect_with(&*s, b3(), true).unwrap();
    assert!(full.full);
    for (_, c) in commits::log(&*s, &commits::tip(&*s, "main").unwrap().unwrap()).unwrap() {
        assert_eq!(files(&*s, &c.version()).len(), 61);
    }
}

fn refs(v: &[(String, String)]) -> Vec<(&str, &str)> {
    v.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect()
}
