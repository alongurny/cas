//! Checks for sync (Stage 7).

use cas_core::algo::{self, Algorithm};
use cas_core::client::HttpStore;
use cas_core::mem::MemBackend;
use cas_core::server::Server;
use cas_core::{AllowAll, CoreStore, Expect, ListPage, Policy, Span, Store, Who};
use cas_version::sync::{self, Copied, Outcome};
use cas_version::{Edit, Error, Node, Path, View, build, diff, formats, head, put, update, walk};

mod common;
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
    build(s, b3(), formats::by_alias("directory").unwrap(), &m).unwrap()
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
        common::commit(&*src, "main", &v1);
        let (o, n) = sync::push(&*src, &*dst, "main", "main").unwrap();
        assert_eq!(o, Outcome::Created);
        // 100 files, 10 directories, the root, the commit and its author,
        // and the specs and objects of the directory, raw, commit and
        // author formats.
        assert_eq!(n.blobs, 100 + 10 + 1 + 2 + 8);
        same_tree(&*src, &*dst, &v1);

        assert_eq!(sync::push(&*src, &*dst, "main", "main").unwrap(), (Outcome::UpToDate, Copied::default()));

        // One edit: one file, one directory, the root and the commit.
        let v2 = edit(&*src, &v1, "d4/f7", "changed");
        common::commit(&*src, "main", &v2);
        let (o, n) = sync::push(&*src, &*dst, "main", "main").unwrap();
        assert_eq!(o, Outcome::FastForward);
        assert_eq!(n.blobs, 4);
        assert_eq!(head(&*dst, "main").unwrap(), head(&*src, "main").unwrap());
        assert_eq!(diff(&*dst, &v1, &v2, None).unwrap().len(), 1);
    }
}

#[test]
fn diverged_histories_are_not_overwritten() {
    let (a, b) = (store(), store());
    let v = big(&*a);
    common::commit(&*a, "main", &v);
    sync::push(&*a, &*b, "main", "main").unwrap();
    // Both sides add a node after the shared tip.
    let va = edit(&*a, &v, "d1/f1", "a");
    common::commit(&*a, "main", &va);
    let vb = edit(&*b, &v, "d2/f2", "b");
    common::commit(&*b, "main", &vb);
    let b_head = head(&*b, "main").unwrap();

    assert_eq!(sync::push(&*a, &*b, "main", "main"), Err(Error::Behind("main".into())));
    assert_eq!(head(&*b, "main").unwrap(), b_head);
    // Under another name it is just a new history.
    sync::push(&*a, &*b, "main", "a-main").unwrap();
    assert_eq!(head(&*b, "a-main").unwrap(), head(&*a, "main").unwrap());
}

#[test]
fn an_interrupted_copy_resumes() {
    let src = store();
    let v = big(&*src);
    common::commit(&*src, "main", &v);
    let tip = head(&*src, "main").unwrap();
    let dst = store();
    for budget in [5, 40, 1000] {
        let flaky = Flaky { inner: Arc::clone(&dst), budget: AtomicUsize::new(budget) };
        match sync::push(&*src, &flaky, "main", "main") {
            Ok((o, _)) => assert_eq!(o, Outcome::Created),
            Err(Error::Store(cas_core::Error::Unavailable(_))) => {
                // Whatever arrived is complete: each directory the
                // destination has can be walked in full there.
                assert_eq!(head(&*dst, "main").unwrap(), None);
                let formats::Kind::Dir(d) = &formats::by_alias("directory").unwrap().kind else { unreachable!() };
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
    assert_eq!(head(&*dst, "main").unwrap(), tip);
    same_tree(&*src, &*dst, &v);
    // Format descriptions came along.
    for alias in ["directory", "raw", "commit"] {
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
        common::commit(&*src, "main", &v);
    }
    sync::push(&*src, &*inner, "main", "main").unwrap();
    let v2 = edit(&*src, &v, "d4/f7", "changed");
    common::commit(&*src, "main", &v2);
    let dst = Counting { inner, asked: AtomicUsize::new(0), reads: AtomicUsize::new(0) };
    let (o, n) = sync::push(&*src, &dst, "main", "main").unwrap();
    // The new file, its directory and root, and the commit.
    assert_eq!((o, n.blobs), (Outcome::FastForward, 4));
    // The commit and the one before it, then the new version level by
    // level: far fewer than the 301 versions.
    assert!(dst.asked.load(Ordering::SeqCst) < 40, "asked about {}", dst.asked.load(Ordering::SeqCst));
    // Only the name was read, not the 300 commits.
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
        common::commit(&src, "main", &root);
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
    common::commit(&*src, "main", &root);
    let dst = store();
    let err = sync::push(&Liar(src.clone()), &*dst, "main", "main").unwrap_err().to_string();
    assert!(err.contains("wrong bytes"), "{err}");
    assert!(head(&*dst, "main").unwrap().is_none());
}

/// A blob that is not valid in the format it is reached as is never
/// written: the copy checks each blob (parse, then print gives its bytes)
/// before it writes it, and the history does not move.
#[test]
fn a_blob_invalid_in_its_format_is_refused() {
    for dst in [store(), remote()] {
        let src = store();
        let mut m = BTreeMap::new();
        let ok = put(&*src, b3(), "raw", b"fine").unwrap();
        m.insert(Path::new("a").unwrap(), Node::File(ok));
        // A link target holding NUL, written past `put`'s check.
        let bad_key = cas_version::write_blob(&*src, b3(), b"to\0where").unwrap();
        let bad = View::new(formats::by_alias("symlink").unwrap().id(), bad_key.clone()).unwrap();
        m.insert(Path::new("l").unwrap(), Node::File(bad));
        let root = build(&*src, b3(), formats::by_alias("directory").unwrap(), &m).unwrap();
        common::commit(&*src, "main", &root);
        let err = sync::push(&*src, &*dst, "main", "main").unwrap_err();
        assert!(matches!(err, Error::Malformed { ref key, .. } if *key == bad_key), "{err}");
        assert!(head(&*dst, "main").unwrap().is_none());
        assert!(!sync::has(&*dst, &bad_key).unwrap());
        assert!(!sync::has(&*dst, root.key()).unwrap());
    }
}

/// A store served with a [`sync::Checker`] refuses to move a history to a
/// view that reaches a blob invalid in its format, even from a client that
/// writes the blobs itself; good pushes go through.
#[test]
fn a_served_store_refuses_a_history_with_an_invalid_blob() {
    let core = CoreStore::with_check(
        MemBackend::new(),
        Arc::new(AllowAll),
        Policy::permissive(),
        Arc::new(sync::Checker::new()),
    );
    let (tls, ca) = cas_core::server::self_signed();
    let server =
        Server::start(core, "127.0.0.1:0", 8, Arc::new(|_: &cas_core::server::Request| Who::anonymous()), tls).unwrap();
    let dst: Arc<dyn Store> = Arc::new(HttpStore::new(server.url()).unwrap().trusting(&ca).unwrap());

    let src = store();
    let root = big(&*src);
    common::commit(&*src, "main", &root);
    assert_eq!(sync::push(&*src, &*dst, "main", "main").unwrap().0, Outcome::Created);
    let next = edit(&*src, &root, "d1/f1", "changed");
    common::commit(&*src, "main", &next);
    assert_eq!(sync::push(&*src, &*dst, "main", "main").unwrap().0, Outcome::FastForward);

    // A client that skips the copy's checks: every blob written by hand.
    let mut m = BTreeMap::new();
    let bad_key = cas_version::write_blob(&*src, b3(), b"to\0where").unwrap();
    let bad = View::new(formats::by_alias("symlink").unwrap().id(), bad_key.clone()).unwrap();
    m.insert(Path::new("l").unwrap(), Node::File(bad));
    let tree = build(&*src, b3(), formats::by_alias("directory").unwrap(), &m).unwrap();
    for k in [&bad_key, tree.key()] {
        dst.write(k, &src.read(k).unwrap().unwrap(), &Expect::Absent).unwrap();
    }
    let err = dst.write("other", tree.to_string().as_bytes(), &Expect::Absent).unwrap_err();
    assert!(matches!(err, cas_core::Error::Refused { ref reason, .. } if reason == "invalid-blob"), "{err:?}");
    assert!(head(&*dst, "other").unwrap().is_none());
    Box::leak(Box::new(server));
}
