//! Checks for reachability, collection and purging (spec/gc.md).

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Expect, Policy, Span, Store, Who, list_all};
use cas_version::{Node, Path, build, formats, gc, history, put, sync, walk};
use std::collections::BTreeMap;
use std::sync::Arc;

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn store() -> Arc<dyn Store> {
    Arc::new(CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous()))
}

fn commit(s: &dyn Store, files: &[(&str, &str)]) -> history::NodeId {
    let mut m = BTreeMap::new();
    for (p, c) in files {
        m.insert(Path::new(*p).unwrap(), Node::File(put(s, b3(), "raw", c.as_bytes()).unwrap()));
    }
    let v = build(s, b3(), formats::by_alias("dir").unwrap(), &m).unwrap();
    history::commit(s, b3(), "main", &v, history::Step::default(), None).unwrap()
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
    let v = history::load(&*s, "main").unwrap().1.tip().unwrap().version();
    assert_eq!(files(&*s, &v).len(), 2);
    // The garbage went; both pending sets are still there (the first is
    // unreachable now and goes two runs later), and the history's ancestry
    // blob the first run recorded.
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
    let head = history::head(&*s, "main").unwrap().unwrap();
    assert!(marked.contains(head.key()));
    for id in history::read(&*s, &head).unwrap().log() {
        assert!(marked.contains(id.version().key()));
    }
    let dir = formats::by_alias("dir").unwrap();
    assert!(marked.contains(dir.id()));
}

/// Delta hints between big `tree` directories pair each changed file with
/// its old self by name, across leaves that differ, and skip what did not
/// change.
#[test]
fn hints_pair_changed_files_of_big_directories_by_name() {
    let s = store();
    let tree = formats::by_alias("tree").unwrap();
    let mut m = BTreeMap::new();
    for i in 0..3000 {
        m.insert(
            Path::new(format!("f{i:05}")).unwrap(),
            Node::File(put(&*s, b3(), "raw", format!("v1 {i}").as_bytes()).unwrap()),
        );
    }
    let v1 = build(&*s, b3(), tree, &m).unwrap();
    history::commit(&*s, b3(), "main", &v1, history::Step::default(), None).unwrap();
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
    history::commit(&*s, b3(), "main", &v2, history::Step::default(), None).unwrap();
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
