//! Checks for reachability, collection and purging (spec/gc.md).

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Expect, Policy, Span, Store, Who, list_all};
use cas_version::{Error, Node, Path, build, formats, gc, history, put, read_blob, sync, walk};
use std::collections::BTreeMap;
use std::sync::Arc;

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn store() -> Arc<dyn Store> {
    Arc::new(CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous()))
}

fn commit(s: &dyn Store, files: &[(&str, &str)]) -> cas_version::Ref {
    let mut m = BTreeMap::new();
    for (p, c) in files {
        m.insert(Path::new(*p).unwrap(), Node::File(put(s, b3(), "raw", c.as_bytes()).unwrap()));
    }
    let v = build(s, b3(), formats::by_alias("dir").unwrap(), &m).unwrap();
    history::commit(s, b3(), "main", &v, BTreeMap::new()).unwrap()
}

fn files(s: &dyn Store, v: &cas_version::Ref) -> Vec<(Path, cas_version::Ref)> {
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
    let head = history::head(&*s, "main").unwrap().unwrap();
    let v = history::read(&*s, &head).unwrap().version;
    assert_eq!(files(&*s, &v).len(), 2);
    // The garbage went; both pending sets are still there (the first is
    // unreachable now and goes two runs later).
    assert_eq!(blobs(&*s), before - 1 + 2);
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
    for (r, rec) in history::log(&*s, &head).unwrap() {
        assert!(marked.contains(r.key()) && marked.contains(rec.version.key()));
    }
    let dir = formats::by_alias("dir").unwrap();
    assert!(marked.contains(dir.id()));
}

#[test]
fn purge_leaves_a_tombstone_that_reads_and_syncs_respect() {
    let (s, other) = (store(), store());
    let head = commit(&*s, &[("secret", "hunter2\n"), ("ok", "fine\n")]);
    let v = history::read(&*s, &head).unwrap().version;
    let secret = files(&*s, &v).into_iter().find(|(p, _)| p.as_str() == "secret").unwrap().1;
    gc::purge(&*s, secret.key(), "leaked password").unwrap();
    assert_eq!(read_blob(&*s, secret.key()), Err(Error::Purged(secret.key().to_string())));
    // Purging twice is fine.
    gc::purge(&*s, secret.key(), "again").unwrap();
    // History and the tree are intact; only the content is gone.
    assert_eq!(files(&*s, &v).len(), 2);
    // A sync carries the tombstone, not an error.
    let (_, copied) = sync::push(&*s, &*other, "main").unwrap();
    assert_eq!(copied.purged, 1);
    assert_eq!(read_blob(&*other, secret.key()), Err(Error::Purged(secret.key().to_string())));
    assert_eq!(other.read(&gc::tombstone(secret.key())).unwrap().unwrap(), b"leaked password");
    // Collection does not trip over the hole.
    gc::collect(&*s, b3()).unwrap();
    gc::collect(&*s, b3()).unwrap();
    assert_eq!(files(&*s, &v).len(), 2);
}

#[test]
fn purging_a_copy_held_elsewhere_reaches_it_on_the_next_sync() {
    let (s, other) = (store(), store());
    let head = commit(&*s, &[("f", "oops\n")]);
    sync::push(&*s, &*other, "main").unwrap();
    let v = history::read(&*s, &head).unwrap().version;
    let (_, f) = files(&*s, &v).remove(0);
    gc::purge(&*s, f.key(), "mistake").unwrap();
    // Push only adds; the other side purges when told about the tombstone.
    for k in list_all(&*s, &Span::all()).unwrap().into_iter().filter(|k| k.starts_with("purged/")) {
        let key = k.strip_prefix("purged/").unwrap();
        gc::purge(&*other, key, &String::from_utf8(s.read(&k).unwrap().unwrap()).unwrap()).unwrap();
    }
    assert_eq!(read_blob(&*other, f.key()), Err(Error::Purged(f.key().to_string())));
}
