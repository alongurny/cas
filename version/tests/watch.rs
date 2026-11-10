//! The status watcher (version/src/watch.rs): a commit with a watcher
//! running edits only what the watcher logged into the last import, and
//! gets what a full look at the directory gets.

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Policy, Session, Who};
use cas_version::{formats, fs, watch};
use std::path::Path;
use std::time::{Duration, Instant};

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn store() -> Session {
    CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous())
}

fn tree() -> &'static formats::Format {
    formats::by_alias("tree").unwrap()
}

/// Wait until the watcher's log stops growing: it has caught up.
fn settle(dir: &Path) {
    let len = || std::fs::metadata(dir.join(watch::LOG)).map(|m| m.len()).unwrap_or(0);
    let start = Instant::now();
    let mut last = len();
    loop {
        std::thread::sleep(Duration::from_millis(50));
        let now = len();
        if (now == last && watch::token(dir).is_some()) || start.elapsed() > Duration::from_secs(5) {
            return;
        }
        last = now;
    }
}

fn write(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

#[test]
fn watched_commits_match_a_full_look() {
    let s = store();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    for i in 0..30 {
        write(dir, &format!("d{}/e{}/f{i}", i % 3, i % 5), &format!("{i}\n"));
    }
    write(dir, "top", "top\n");
    std::fs::create_dir(dir.join("empty")).unwrap();
    assert!(watch::token(dir).is_none(), "no watcher yet");
    let w = watch::Watcher::start(dir).unwrap();
    std::thread::spawn(move || w.run());
    settle(dir);
    assert!(watch::Watcher::start(dir).is_err(), "one watcher a directory");
    let scope = Some("test");
    let full = |s: &Session| fs::import(s, b3(), tree(), dir).unwrap();
    let first = fs::import_indexed(&s, b3(), tree(), dir, scope).unwrap();
    assert_eq!(first, full(&s));
    // Nothing changed: the same version, from the cache alone.
    settle(dir);
    assert_eq!(fs::import_indexed(&s, b3(), tree(), dir, scope).unwrap(), first);
    // A changed file, a new tree, a removed tree, a renamed directory, a
    // file replaced by a directory and an emptied directory.
    write(dir, "top", "changed\n");
    write(dir, "new/a/b", "b\n");
    std::fs::remove_dir_all(dir.join("d1")).unwrap();
    std::fs::rename(dir.join("d2"), dir.join("d2-moved")).unwrap();
    std::fs::remove_file(dir.join("d0/e0/f0")).unwrap();
    std::fs::create_dir(dir.join("d0/e0/f0")).unwrap();
    write(dir, "d0/e0/f0/inner", "inner\n");
    for e in std::fs::read_dir(dir.join("d0/e3")).unwrap() {
        std::fs::remove_file(e.unwrap().path()).unwrap();
    }
    settle(dir);
    let second = fs::import_indexed(&s, b3(), tree(), dir, scope).unwrap();
    assert_ne!(second, first);
    assert_eq!(second, full(&s));
    // A log that says events were lost: a full look, still right.
    write(dir, "top", "again\n");
    settle(dir);
    use std::io::Write;
    std::fs::OpenOptions::new().append(true).open(dir.join(watch::LOG)).unwrap().write_all(b"!\n").unwrap();
    let third = fs::import_indexed(&s, b3(), tree(), dir, scope).unwrap();
    assert_eq!(third, full(&s));
    // The watcher's own files are never imported.
    let names: Vec<String> = cas_version::walk(&s, &third, None).map(|r| r.unwrap().0.as_str().to_string()).collect();
    assert!(names.iter().all(|n| !n.starts_with(".cas")), "{names:?}");
}

#[test]
fn touched_covers_paths_under_a_changed_one() {
    let changed = ["a/b", "c"].iter().map(|s| s.to_string()).collect();
    assert!(watch::touched("a/b", &changed));
    assert!(watch::touched("a/b/c/d", &changed));
    assert!(watch::touched("c/x", &changed));
    assert!(!watch::touched("a", &changed));
    assert!(!watch::touched("a/bc", &changed));
    assert!(!watch::touched("cc", &changed));
}
