//! Checks for sync (Stage 7).

use cas_core::algo::{self, Algorithm};
use cas_core::client::HttpStore;
use cas_core::mem::MemBackend;
use cas_core::server::Server;
use cas_core::{AllowAll, CoreStore, Expect, ListPage, Policy, Span, Store, Who};
use cas_version::history::Step;
use cas_version::sync::{self, Copied, Outcome};
use cas_version::{Edit, Error, Node, Path, View, build, diff, formats, history, put, update, walk};
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
    let (tls, ca) = cas_core::server::self_signed();
    let server =
        Server::start(core, "127.0.0.1:0", 8, Arc::new(|_: &cas_core::server::Request| Who::anonymous()), tls).unwrap();
    let url = server.url();
    Box::leak(Box::new(server));
    Arc::new(HttpStore::new(url).unwrap().trusting(&ca).unwrap())
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
        // Take one from the budget, unless it is spent.
        let mut left = self.budget.load(Ordering::SeqCst);
        loop {
            if left == 0 {
                return Err(cas_core::Error::Unavailable("connection dropped".into()));
            }
            match self.budget.compare_exchange(left, left - 1, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => break,
                Err(now) => left = now,
            }
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

fn big(s: &dyn Store) -> View {
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

fn same_tree(a: &dyn Store, b: &dyn Store, root: &View) {
    let x: Vec<_> = walk(a, root, None).map(|r| r.unwrap()).collect();
    let y: Vec<_> = walk(b, root, None).map(|r| r.unwrap()).collect();
    assert_eq!(x, y);
    for (_, n) in x {
        if let Node::File(f) = n {
            assert_eq!(b.read(f.key()).unwrap(), a.read(f.key()).unwrap());
        }
    }
}

fn edit(s: &dyn Store, root: &View, path: &str, text: &str) -> View {
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
        // 100 files, 10 directories, the root, a history root and its one
        // leaf, and the specs and objects of the dir, raw, history and
        // history-part formats.
        assert_eq!(n.blobs, 100 + 10 + 1 + 2 + 8);
        same_tree(&*src, &*dst, &v1);

        assert_eq!(sync::push(&*src, &*dst, "main", "main").unwrap(), (Outcome::UpToDate, Copied::default()));

        // One edit: one file, one directory, the root, a history root and
        // its leaf.
        let v2 = edit(&*src, &v1, "d4/f7", "changed");
        history::commit(&*src, b3(), "main", &v2, Step::default(), None).unwrap();
        let (o, n) = sync::push(&*src, &*dst, "main", "main").unwrap();
        assert_eq!(o, Outcome::FastForward);
        assert_eq!(n.blobs, 5);
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

    assert_eq!(sync::push(&*a, &*b, "main", "main"), Err(Error::Behind("main".into())));
    assert_eq!(history::head(&*b, "main").unwrap(), b_head);
    // A rewritten history (a reworded step) is not an extension either.
    let (old, mut h) = history::load(&*a, "main").unwrap();
    let tip = h.tip().unwrap();
    let pred = h.nodes[&tip].after.keys().next().unwrap().clone();
    h.nodes
        .get_mut(&tip)
        .unwrap()
        .after
        .insert(pred, Step { changelog: Some("reworded".into()), compatible: false, notes: None });
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
                let kids = d.blob_children(v.key(), &src.read(v.key()).unwrap().unwrap()).unwrap();
                let present: Vec<_> = kids.iter().filter(|c| sync::has(&*dst, c.key()).unwrap()).collect();
                // Breadth first, files arrive before the directories naming them.
                if budget == 40 {
                    let files = walk(&*src, &v, None).filter_map(|r| match r.unwrap().1 {
                        Node::File(f) => Some(f),
                        _ => None,
                    });
                    assert!(files.filter(|f| sync::has(&*dst, f.key()).unwrap()).count() > 0);
                }
                for c in present {
                    assert_eq!(walk(&*dst, c, None).filter(|r| r.is_err()).count(), 0);
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

/// Counts what is asked of a store.
struct Counting {
    inner: Arc<dyn Store>,
    asked: AtomicUsize,
    reads: AtomicUsize,
}

impl Store for Counting {
    fn read(&self, key: &str) -> cas_core::Result<Option<Vec<u8>>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.read(key)
    }
    fn write(&self, key: &str, value: &[u8], expect: &Expect) -> cas_core::Result<()> {
        self.inner.write(key, value, expect)
    }
    fn remove(&self, key: &str, expect: &Expect) -> cas_core::Result<()> {
        self.inner.remove(key, expect)
    }
    fn list(&self, span: &Span) -> cas_core::Result<ListPage> {
        self.inner.list(span)
    }
    fn has_many(&self, keys: &[String]) -> cas_core::Result<Vec<bool>> {
        self.asked.fetch_add(keys.len(), Ordering::SeqCst);
        self.inner.has_many(keys)
    }
    fn read_many(&self, keys: &[String]) -> Vec<cas_core::Result<Option<Vec<u8>>>> {
        self.reads.fetch_add(keys.len(), Ordering::SeqCst);
        self.inner.read_many(keys)
    }
}

/// Push walks locally: one commit on a long history asks the destination
/// about the new objects only, and does not fetch its history.
#[test]
fn push_of_one_commit_asks_only_about_what_is_new() {
    let src = store();
    let inner = store();
    let mut v = big(&*src);
    for i in 0..300 {
        v = edit(&*src, &v, "d1/f1", &format!("v{i}"));
        history::commit(&*src, b3(), "main", &v, Step::default(), None).unwrap();
    }
    sync::push(&*src, &*inner, "main", "main").unwrap();
    let v2 = edit(&*src, &v, "d4/f7", "changed");
    history::commit(&*src, b3(), "main", &v2, Step::default(), None).unwrap();
    let dst = Counting { inner, asked: AtomicUsize::new(0), reads: AtomicUsize::new(0) };
    let (o, n) = sync::push(&*src, &dst, "main", "main").unwrap();
    // The new file, its directory and root, and of the history its root,
    // the leaf the new node is in and the top part over the leaves.
    assert_eq!((o, n.blobs), (Outcome::FastForward, 6));
    // The history and new version, then the new root's children level by
    // level: far fewer than the 301 versions.
    assert!(dst.asked.load(Ordering::SeqCst) < 40, "asked about {}", dst.asked.load(Ordering::SeqCst));
    // Only the name was read, not the 300-node history.
    assert_eq!(dst.reads.load(Ordering::SeqCst), 1);
}

/// A disk store after gc keeps most versions of a file as deltas; a pull
/// over HTTP gets them in that form and keeps them so.
#[test]
fn a_pull_keeps_the_sources_deltas() {
    use cas_core::Backend;
    use cas_core::disk::DiskBackend;
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let src_core = CoreStore::new(DiskBackend::open(a.path()).unwrap(), AllowAll, Policy::permissive());
    let src = src_core.session(Who::anonymous());
    let text = |i: usize| {
        (0..400).map(|l| format!("line {l} of version {}\n", if l == 7 { i } else { 0 })).collect::<String>()
    };
    let mut root = big(&src);
    // Within the delta chain limit, so the newest is a delta too.
    for i in 0..8 {
        root = edit(&src, &root, "d1/f1", &text(i));
        history::commit(&src, b3(), "main", &root, Step::default(), None).unwrap();
    }
    let disk = DiskBackend::open(a.path()).unwrap();
    let (order, hints) = cas_version::gc::delta_hints(&src, &|_| false).unwrap();
    assert!(disk.repack(&order, &hints).unwrap().deltas >= 7);
    let (tls, ca) = cas_core::server::self_signed();
    let server =
        Server::start(src_core, "127.0.0.1:0", 8, Arc::new(|_: &cas_core::server::Request| Who::anonymous()), tls)
            .unwrap();
    let remote = HttpStore::new(server.url()).unwrap().trusting(&ca).unwrap();
    let dst =
        CoreStore::new(DiskBackend::open(b.path()).unwrap(), AllowAll, Policy::permissive()).session(Who::anonymous());
    sync::push(&remote, &dst, "main", "main").unwrap();
    same_tree(&src, &dst, &root);
    assert!(cas_version::fsck::fsck(&dst).unwrap().ok());
    // The file's versions arrived as deltas and stayed deltas.
    let f = match cas_version::lookup(&dst, &root, &Path::new("d1/f1").unwrap()).unwrap() {
        Some(Node::File(f)) => f,
        n => panic!("{n:?}"),
    };
    let had = DiskBackend::open(a.path()).unwrap().get_stored(f.key()).unwrap().unwrap();
    let kept = DiskBackend::open(b.path()).unwrap().get_stored(f.key()).unwrap().unwrap();
    assert!(had.base().is_some(), "source: {:?}", had.base());
    assert!(kept.base().is_some(), "{:?}", kept.base());
}

/// A source whose stored forms do not decode to their keys' values is
/// caught: nothing wrong is written.
#[test]
fn a_wrong_stored_form_is_refused() {
    struct Liar(Arc<dyn Store>);
    impl Store for Liar {
        fn read(&self, key: &str) -> cas_core::Result<Option<Vec<u8>>> {
            self.0.read(key)
        }
        fn write(&self, key: &str, value: &[u8], expect: &Expect) -> cas_core::Result<()> {
            self.0.write(key, value, expect)
        }
        fn remove(&self, key: &str, expect: &Expect) -> cas_core::Result<()> {
            self.0.remove(key, expect)
        }
        fn list(&self, span: &Span) -> cas_core::Result<ListPage> {
            self.0.list(span)
        }
        fn read_stored_many(&self, keys: &[String]) -> Vec<cas_core::Result<Option<cas_core::Stored>>> {
            let z = zstd::bulk::compress(b"not it", 3).unwrap();
            keys.iter()
                .map(|k| match self.0.read(k)? {
                    Some(v) if v.starts_with(b"0/") => {
                        Ok(Some(cas_core::Stored::Form { len: 6, form: cas_core::Form::Zstd(z.clone()) }))
                    }
                    v => Ok(v.map(cas_core::Stored::Value)),
                })
                .collect()
        }
    }
    let src = store();
    let root = big(&*src);
    history::commit(&*src, b3(), "main", &root, Step::default(), None).unwrap();
    let dst = store();
    let err = sync::push(&Liar(src.clone()), &*dst, "main", "main").unwrap_err().to_string();
    assert!(err.contains("wrong bytes"), "{err}");
    assert!(history::head(&*dst, "main").unwrap().is_none());
}
