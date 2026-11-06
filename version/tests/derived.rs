//! Checks for derived data (Stage 6).

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Expect, ListPage, Policy, Session, Span, Store, Who};
use cas_version::derived::{self, SUMMARY};
use cas_version::history::{History, Step};
use cas_version::uni::Value;
use cas_version::{Edit, Node, Path, Ref, build, formats, put, update};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn store() -> Session {
    CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous())
}

struct Counting<'s> {
    inner: &'s dyn Store,
    writes: AtomicUsize,
}

impl Store for Counting<'_> {
    fn read(&self, key: &str) -> cas_core::Result<Option<Vec<u8>>> {
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

fn file(s: &dyn Store, text: &str) -> Node {
    Node::File(put(s, b3(), "raw", text.as_bytes()).unwrap())
}

fn version(s: &dyn Store, paths: &[&str]) -> Ref {
    let mut m = BTreeMap::new();
    for p in paths {
        let path = Path::new(p.trim_end_matches('/')).unwrap();
        let mut q = path.parent();
        while let Some(d) = q {
            q = d.parent();
            m.insert(d, Node::Dir);
        }
        m.insert(path, if p.ends_with('/') { Node::Dir } else { file(s, p) });
    }
    build(s, b3(), formats::by_alias("dir").unwrap(), &m).unwrap()
}

fn counts(s: &dyn Store, r: &Ref) -> (String, String) {
    let v = derived::value(s, r).unwrap();
    let m = v.as_map().unwrap();
    let get = |k: &str| m[&Value::str(k)].as_str().unwrap().to_string();
    (get("dirs"), get("files"))
}

#[test]
fn function_ids_are_content_addressed() {
    let s = store();
    derived::publish(&s, &SUMMARY).unwrap();
    let object = s.read(&SUMMARY.id()).unwrap().unwrap();
    assert_eq!(Value::deserialize(&object).unwrap(), SUMMARY.object());
    let v = version(&s, &["a"]);
    assert_eq!(SUMMARY.tag(&v), format!("derived/{}/{}", SUMMARY.id(), v.key()));
}

#[test]
fn summary_is_cached_and_incremental() {
    let s = store();
    let mut paths = Vec::new();
    for i in 0..10 {
        for j in 0..10 {
            paths.push(format!("d{i}/e{j}/f"));
        }
    }
    paths.push("empty/".to_string());
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let v = version(&s, &refs);
    let r = derived::derive(&s, b3(), &SUMMARY, &v).unwrap();
    assert_eq!(counts(&s, &r), ("111".into(), "100".into()));
    assert_eq!(derived::cached(&s, &SUMMARY, &v).unwrap(), Some(r.clone()));

    // A second derive writes nothing.
    let c = Counting { inner: &s, writes: AtomicUsize::new(0) };
    assert_eq!(derived::derive(&c, b3(), &SUMMARY, &v).unwrap(), r);
    assert_eq!(c.writes.load(Ordering::SeqCst), 0);

    // After one edit, only the directories on its path are recomputed:
    // three results and three tags.
    let edit = BTreeMap::from([(Path::new("d3/e4/g").unwrap(), Edit::Set(file(&s, "new")))]);
    let v2 = update(&s, b3(), &v, &edit).unwrap();
    let c = Counting { inner: &s, writes: AtomicUsize::new(0) };
    let r2 = derived::derive(&c, b3(), &SUMMARY, &v2).unwrap();
    assert_eq!(counts(&s, &r2), ("111".into(), "101".into()));
    assert!(c.writes.load(Ordering::SeqCst) <= 6, "{} writes", c.writes.load(Ordering::SeqCst));
}

#[test]
fn a_result_recorded_first_wins() {
    let s = store();
    let v = version(&s, &["a"]);
    // Another writer recorded a result under the tag first.
    let theirs = put(&s, b3(), "uni-bin", &Value::map([("dirs", "0"), ("files", "1")]).serialize()).unwrap();
    s.write(&SUMMARY.tag(&v), theirs.to_string().as_bytes(), &Expect::Absent).unwrap();
    assert_eq!(derived::derive(&s, b3(), &SUMMARY, &v).unwrap(), theirs);
    // Inputs in the wrong format are refused.
    let raw = put(&s, b3(), "raw", b"not a dir").unwrap();
    assert!(derived::derive(&s, b3(), &SUMMARY, &raw).is_err());
}

#[test]
fn path_log() {
    let s = store();
    let mut h = History::default();
    let v1 = version(&s, &["a/x", "b"]);
    let c1 = h.append(&v1, Step::default(), None).unwrap();
    let v2 = update(&s, b3(), &v1, &BTreeMap::from([(Path::new("b").unwrap(), Edit::Set(file(&s, "b2")))])).unwrap();
    let c2 = h.append(&v2, Step::default(), None).unwrap();
    let v3 = update(&s, b3(), &v2, &BTreeMap::from([(Path::new("a/y").unwrap(), Edit::Set(file(&s, "y")))])).unwrap();
    let c3 = h.append(&v3, Step::default(), None).unwrap();

    // A merge of c3 and a side line that added c: a path counts only when
    // it differs from every predecessor.
    let side_v = update(&s, b3(), &v1, &BTreeMap::from([(Path::new("c").unwrap(), Edit::Set(file(&s, "c")))])).unwrap();
    let side = h.add(&side_v, BTreeMap::from([(c1.clone(), Step::default())]), None).unwrap();
    let mv = update(&s, b3(), &v3, &BTreeMap::from([(Path::new("c").unwrap(), Edit::Set(file(&s, "c")))])).unwrap();
    let m = h.add(&mv, BTreeMap::from([(c3.clone(), Step::default()), (side.clone(), Step::default())]), None).unwrap();

    let log = |p: &str| derived::path_log(&s, &h, &Path::new(p).unwrap()).unwrap();
    assert_eq!(log("b"), [c2.clone(), c1.clone()]);
    assert_eq!(log("a"), [c3.clone(), c1.clone()]);
    assert_eq!(log("a/y"), std::slice::from_ref(&c3));
    assert_eq!(log("c"), [side]);
    assert!(log("zzz").is_empty());
    let _ = (m, Ref::parse);
}
