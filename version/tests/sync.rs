//! Checks for sync (Stage 7).

use cas_core::algo::{self, Algorithm};
use cas_core::client::HttpStore;
use cas_core::mem::MemBackend;
use cas_core::server::Server;
use cas_core::{AllowAll, CoreStore, Expect, ListPage, Policy, Span, Store, Who};
use cas_version::history::Step;
use cas_version::sync::{self, Copied, Outcome};
use cas_version::{Edit, Error, Node, Path, Ref, build, diff, formats, history, put, update, walk};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn store() -> Arc<dyn Store> {
    Arc::new(CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous()))
}

/// A store reached over HTTP, to show sync needs nothing beyond the ops.
fn remote() -> Arc<dyn Store> {
    let core = CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive());
    let server = Server::start(core, "127.0.0.1:0", 8, Arc::new(|_: &tiny_http::Request| Who::anonymous())).unwrap();
    let url = server.url();
    Box::leak(Box::new(server));
    Arc::new(HttpStore::new(url))
}

/// Fails every write after the first `budget`, like a dropped connection.
struct Flaky {
    inner: Arc<dyn Store>,
    budget: AtomicUsize,
}

impl Store for Flaky {
    fn read(&self, key: &str) -> cas_core::Result<Option<Vec<u8>>> {
        self.inner.read(key)
    }
    fn write(&self, key: &str, value: &[u8], expect: &Expect) -> cas_core::Result<()> {
        if self.budget.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |b| b.checked_sub(1)).is_err() {
            return Err(cas_core::Error::Unavailable("connection dropped".into()));
        }
        self.inner.write(key, value, expect)
    }
    fn remove(&self, key: &str, expect: &Expect) -> cas_core::Result<()> {
        self.inner.remove(key, expect)
    }
    fn list(&self, span: &Span) -> cas_core::Result<ListPage> {
        self.inner.list(span)
    }
}

fn big(s: &dyn Store) -> Ref {
    let mut m = BTreeMap::new();
    for i in 0..10 {
        m.insert(Path::new(format!("d{i}")).unwrap(), Node::Dir);
        for j in 0..10 {
            let f = put(s, b3(), "raw", format!("{i}/{j}").as_bytes()).unwrap();
            m.insert(Path::new(format!("d{i}/f{j}")).unwrap(), Node::File(f));
        }
    }
    build(s, b3(), formats::by_alias("dir").unwrap(), &m).unwrap()
}

fn same_tree(a: &dyn Store, b: &dyn Store, root: &Ref) {
    let x: Vec<_> = walk(a, root, None).map(|r| r.unwrap()).collect();
    let y: Vec<_> = walk(b, root, None).map(|r| r.unwrap()).collect();
    assert_eq!(x, y);
    for (_, n) in x {
        if let Node::File(f) = n {
            assert_eq!(b.read(f.key()).unwrap(), a.read(f.key()).unwrap());
        }
    }
}

fn edit(s: &dyn Store, root: &Ref, path: &str, text: &str) -> Ref {
    let f = put(s, b3(), "raw", text.as_bytes()).unwrap();
    update(s, b3(), root, &BTreeMap::from([(Path::new(path).unwrap(), Edit::Set(Node::File(f)))])).unwrap()
}

#[test]
fn push_copies_only_what_is_missing() {
    for dst in [store(), remote()] {
        let src = store();
        let v1 = big(&*src);
        history::commit(&*src, b3(), "main", &v1, Step::default(), None).unwrap();
        let (o, n) = sync::push(&*src, &*dst, "main", "main").unwrap();
        assert_eq!(o, Outcome::Created);
        // 100 files, 10 directories, the root, one history, and the specs and
        // objects of the dir, raw and history formats.
        assert_eq!(n.blobs, 100 + 10 + 1 + 1 + 6);
        same_tree(&*src, &*dst, &v1);

        assert_eq!(sync::push(&*src, &*dst, "main", "main").unwrap(), (Outcome::UpToDate, Copied::default()));

        // One edit: one file, one directory, the root, one history.
        let v2 = edit(&*src, &v1, "d4/f7", "changed");
        history::commit(&*src, b3(), "main", &v2, Step::default(), None).unwrap();
        let (o, n) = sync::push(&*src, &*dst, "main", "main").unwrap();
        assert_eq!(o, Outcome::FastForward);
        assert_eq!(n.blobs, 4);
        assert_eq!(history::head(&*dst, "main").unwrap(), history::head(&*src, "main").unwrap());
        assert_eq!(diff(&*dst, &v1, &v2, None).unwrap().len(), 1);
    }
}

#[test]
fn diverged_histories_are_not_overwritten() {
    let (a, b) = (store(), store());
    let v = big(&*a);
    history::commit(&*a, b3(), "main", &v, Step::default(), None).unwrap();
    sync::push(&*a, &*b, "main", "main").unwrap();
    // Both sides add a node after the shared tip.
    let va = edit(&*a, &v, "d1/f1", "a");
    history::commit(&*a, b3(), "main", &va, Step::default(), None).unwrap();
    let vb = edit(&*b, &v, "d2/f2", "b");
    history::commit(&*b, b3(), "main", &vb, Step::default(), None).unwrap();
    let b_head = history::head(&*b, "main").unwrap();

    assert_eq!(sync::push(&*a, &*b, "main", "main"), Err(Error::Diverged("main".into())));
    assert_eq!(history::head(&*b, "main").unwrap(), b_head);
    // A rewritten history (a reworded step) is not an extension either.
    let (old, mut h) = history::load(&*a, "main").unwrap();
    let tip = h.tip().unwrap();
    let pred = h.nodes[&tip].after.keys().next().unwrap().clone();
    h.nodes.get_mut(&tip).unwrap().after.insert(pred, Step { changelog: Some("reworded".into()), compatible: false });
    history::save(&*a, b3(), "main", old.as_ref(), &h).unwrap();
    // Under another name it is just a new history.
    sync::push(&*a, &*b, "main", "a-main").unwrap();
    assert_eq!(history::head(&*b, "a-main").unwrap(), history::head(&*a, "main").unwrap());
}

#[test]
fn an_interrupted_copy_resumes() {
    let src = store();
    let v = big(&*src);
    history::commit(&*src, b3(), "main", &v, Step::default(), None).unwrap();
    let head = history::head(&*src, "main").unwrap();
    let dst = store();
    for budget in [5, 40, 1000] {
        let flaky = Flaky { inner: Arc::clone(&dst), budget: AtomicUsize::new(budget) };
        match sync::push(&*src, &flaky, "main", "main") {
            Ok((o, _)) => assert_eq!(o, Outcome::Created),
            Err(Error::Store(cas_core::Error::Unavailable(_))) => {
                // Whatever arrived is complete: each directory the
                // destination has can be walked in full there.
                assert_eq!(history::head(&*dst, "main").unwrap(), None);
                let formats::Kind::Dir(d) = &formats::by_alias("dir").unwrap().kind else { unreachable!() };
                let items = d.decode(v.key(), &src.read(v.key()).unwrap().unwrap()).unwrap();
                let present: Vec<_> = items.iter().filter(|i| sync::has(&*dst, i.child.key()).unwrap()).collect();
                if budget == 40 {
                    assert!(!present.is_empty());
                }
                for i in present {
                    assert_eq!(walk(&*dst, &i.child, None).filter(|r| r.is_err()).count(), 0);
                }
            }
            Err(e) => panic!("{e}"),
        }
    }
    assert_eq!(history::head(&*dst, "main").unwrap(), head);
    same_tree(&*src, &*dst, &v);
    // Format descriptions came along.
    for alias in ["dir", "raw", "history"] {
        assert!(sync::has(&*dst, formats::by_alias(alias).unwrap().id()).unwrap(), "{alias}");
    }
}

#[test]
fn has_does_not_match_prefixes() {
    let s = store();
    let k = put(&*s, b3(), "raw", b"x").unwrap();
    assert!(sync::has(&*s, k.key()).unwrap());
    assert!(!sync::has(&*s, &b3().key(b"y")).unwrap());
}
