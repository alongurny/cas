//! The working-directory stat cache (version/src/index.rs).

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Expect, ListPage, Policy, Session, Span, Store, Who};
use cas_version::{formats, fs, index, walk};
use std::sync::atomic::{AtomicUsize, Ordering};

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn store() -> Session {
    CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous())
}

/// Counts reads and writes.
struct Counting<'a> {
    inner: &'a dyn Store,
    reads: AtomicUsize,
    writes: AtomicUsize,
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

fn tree(dir: &std::path::Path) {
    std::fs::create_dir_all(dir.join("src/deep")).unwrap();
    std::fs::create_dir_all(dir.join("empty")).unwrap();
    for i in 0..20 {
        std::fs::write(dir.join(format!("src/f{i}")), format!("file {i}\n")).unwrap();
    }
    std::fs::write(dir.join("src/deep/x"), "x\n").unwrap();
    std::os::unix::fs::symlink("src/f1", dir.join("link")).unwrap();
}

#[test]
fn a_checkout_then_commit_hashes_nothing_again() {
    let s = store();
    let src = tempfile::tempdir().unwrap();
    tree(src.path());
    let dir = formats::by_alias("dir").unwrap();
    let root = fs::import(&s, b3(), dir, src.path()).unwrap();

    let out = tempfile::tempdir().unwrap();
    fs::checkout_indexed(&s, &root, None, out.path(), Some("here")).unwrap();
    assert!(out.path().join(index::FILE).exists());
    std::thread::sleep(std::time::Duration::from_millis(5));

    let counted = Counting { inner: &s, reads: AtomicUsize::new(0), writes: AtomicUsize::new(0) };
    assert_eq!(fs::import_indexed(&counted, b3(), dir, out.path(), Some("here")).unwrap(), root);
    // Only the directories are written again (each a no-op); no file is.
    assert_eq!(counted.writes.load(Ordering::SeqCst), 4, "root, src, src/deep, empty");
    assert_eq!(counted.reads.load(Ordering::SeqCst), 0);

    // A changed file is hashed again; the result is what a plain import gives.
    std::fs::write(out.path().join("src/f3"), "changed, and longer\n").unwrap();
    let new = fs::import_indexed(&s, b3(), dir, out.path(), Some("here")).unwrap();
    assert_ne!(new, root);
    assert_eq!(new, fs::import(&s, b3(), dir, out.path()).unwrap());
}

#[test]
fn the_cache_is_ignored_for_another_store_and_when_blobs_are_gone() {
    let s = store();
    let d = tempfile::tempdir().unwrap();
    tree(d.path());
    let dir = formats::by_alias("dir").unwrap();
    let root = fs::import_indexed(&s, b3(), dir, d.path(), Some("one")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));

    // Another store has none of the blobs: the cached views are checked and
    // the files hashed into it, so what the root names is all there.
    let other = store();
    assert_eq!(fs::import_indexed(&other, b3(), dir, d.path(), Some("one")).unwrap(), root);
    for item in walk(&other, &root, None) {
        let (_, n) = item.unwrap();
        if let cas_version::Node::File(v) = n {
            assert!(other.read(v.key()).unwrap().is_some());
        }
    }
    // A cache for another scope is not used, and the result is the same.
    let counted = Counting { inner: &s, reads: AtomicUsize::new(0), writes: AtomicUsize::new(0) };
    assert_eq!(fs::import_indexed(&counted, b3(), dir, d.path(), Some("two")).unwrap(), root);
    assert!(counted.writes.load(Ordering::SeqCst) > 20);
    // A damaged cache is just a missing one.
    std::fs::write(d.path().join(index::FILE), b"CASINDEX1\ngarbage").unwrap();
    assert_eq!(fs::import_indexed(&s, b3(), dir, d.path(), Some("two")).unwrap(), root);
}

fn snapshot(dir: &std::path::Path) -> cas_version::View {
    let s = store();
    fs::import(&s, b3(), formats::by_alias("dir").unwrap(), dir).unwrap().clone()
}

/// Paths changed in `dir` against the checkout of `tree`.
fn dirty(dir: &std::path::Path) {
    std::fs::write(dir.join("src/f0"), "changed\n").unwrap();
    std::fs::write(dir.join("new"), "new\n").unwrap();
    std::fs::create_dir_all(dir.join("extra/deeper")).unwrap();
    std::fs::write(dir.join("extra/deeper/y"), "y\n").unwrap();
    std::fs::remove_file(dir.join("src/f1")).unwrap();
    std::fs::remove_dir_all(dir.join("src/deep")).unwrap();
    std::fs::write(dir.join("src/deep"), "now a file\n").unwrap();
    std::fs::remove_dir(dir.join("empty")).unwrap();
    std::fs::create_dir(dir.join("link_dir")).unwrap();
}

#[test]
fn status_reports_changes_and_stores_nothing() {
    let s = store();
    let src = tempfile::tempdir().unwrap();
    tree(src.path());
    let dirf = formats::by_alias("dir").unwrap();
    let root = fs::import(&s, b3(), dirf, src.path()).unwrap();
    let out = tempfile::tempdir().unwrap();
    fs::checkout_indexed(&s, &root, None, out.path(), Some("here")).unwrap();
    assert!(fs::status(&s, b3(), dirf, out.path(), Some("here"), &root).unwrap().is_empty());
    dirty(out.path());
    let c = Counting { inner: &s, reads: AtomicUsize::new(0), writes: AtomicUsize::new(0) };
    let changes = fs::status(&c, b3(), dirf, out.path(), Some("here"), &root).unwrap();
    assert_eq!(c.writes.load(Ordering::SeqCst), 0);
    let paths: Vec<String> = changes.iter().map(|c| c.path.to_string()).collect();
    for p in ["src/f0", "new", "extra", "src/f1", "src/deep", "empty", "link_dir"] {
        assert!(paths.iter().any(|x| x == p), "{p} missing from {paths:?}");
    }
}

#[test]
fn reset_restores_the_version_and_clean_removes_only_untracked_paths() {
    let s = store();
    let src = tempfile::tempdir().unwrap();
    tree(src.path());
    let dirf = formats::by_alias("dir").unwrap();
    let root = fs::import(&s, b3(), dirf, src.path()).unwrap();
    for scope in [Some("here"), None] {
        let out = tempfile::tempdir().unwrap();
        fs::checkout_indexed(&s, &root, None, out.path(), scope).unwrap();
        dirty(out.path());
        fs::reset(&s, b3(), dirf, out.path(), scope, &root, true).unwrap();
        assert!(!out.path().join("new").exists());
        assert!(!out.path().join("extra").exists());
        assert!(!out.path().join("link_dir").exists());
        assert_eq!(std::fs::read_to_string(out.path().join("src/f0")).unwrap(), "changed\n");
        assert!(out.path().join("src/deep").is_file());
        fs::reset(&s, b3(), dirf, out.path(), scope, &root, false).unwrap();
        assert_eq!(snapshot(out.path()), root);
        assert!(fs::status(&s, b3(), dirf, out.path(), scope, &root).unwrap().is_empty());
    }
}

#[test]
fn a_sparse_checkout_commits_over_the_rest_of_the_base() {
    let s = store();
    let src = tempfile::tempdir().unwrap();
    tree(src.path());
    let dirf = formats::by_alias("dir").unwrap();
    let root = fs::import(&s, b3(), dirf, src.path()).unwrap();
    let p = |x: &str| cas_version::Path::new(x).unwrap();

    let out = tempfile::tempdir().unwrap();
    fs::checkout_sparse(&s, &root, &[p("src/deep"), p("link"), p("absent/dir")], out.path(), Some("here")).unwrap();
    assert!(out.path().join("src/deep/x").is_file());
    assert!(!out.path().join("src/f0").exists() && !out.path().join("empty").exists());
    // Unchanged: the commit is the base, and status is clean.
    assert_eq!(fs::import_worktree(&s, b3(), dirf, out.path(), Some("here"), Some(&root)).unwrap(), root);
    assert!(fs::status(&s, b3(), dirf, out.path(), Some("here"), &root).unwrap().is_empty());

    std::fs::write(out.path().join("src/deep/x"), "changed\n").unwrap();
    std::fs::create_dir_all(out.path().join("absent/dir")).unwrap();
    std::fs::write(out.path().join("absent/dir/n"), "n\n").unwrap();
    std::fs::remove_file(out.path().join("link")).unwrap();
    std::fs::write(out.path().join("outside"), "ignored\n").unwrap();
    let new = fs::import_worktree(&s, b3(), dirf, out.path(), Some("here"), Some(&root)).unwrap();

    std::fs::write(src.path().join("src/deep/x"), "changed\n").unwrap();
    std::fs::create_dir_all(src.path().join("absent/dir")).unwrap();
    std::fs::write(src.path().join("absent/dir/n"), "n\n").unwrap();
    std::fs::remove_file(src.path().join("link")).unwrap();
    assert_eq!(new, fs::import(&s, b3(), dirf, src.path()).unwrap());

    // Reset puts the sparse paths back and leaves the rest alone.
    fs::reset(&s, b3(), dirf, out.path(), Some("here"), &root, false).unwrap();
    assert!(out.path().join("link").symlink_metadata().is_ok(), "link");
    assert!(!out.path().join("absent/dir/n").exists(), "n");
    assert!(out.path().join("outside").exists());
    assert!(fs::status(&s, b3(), dirf, out.path(), Some("here"), &root).unwrap().is_empty());
}
