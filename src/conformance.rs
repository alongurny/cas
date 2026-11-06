//! The conformance suite every store must pass: backends directly now, and
//! later the same suite over the wire and through a cache.
//!
//! A harness builds a fresh, empty store for a given policy and gate. Use
//! [`crate::conformance_tests!`] to generate one `#[test]` per check.

use crate::algo::{self, REGISTRY};
use crate::gate::{AllowAll, DenyAll, Gate, Op};
use crate::policy::Policy;
use crate::store::{Store, list_all};
use crate::types::{Error, Expect, Invalid, Span};
use std::collections::BTreeMap;
use std::sync::{Arc, Barrier};
use std::thread;

pub trait Harness {
    fn store(&self, policy: Policy, gate: Box<dyn Gate>) -> Arc<dyn Store>;

    fn open(&self) -> Arc<dyn Store> {
        self.store(Policy::permissive(), Box::new(AllowAll))
    }
}

impl<F> Harness for F
where
    F: Fn(Policy, Box<dyn Gate>) -> Arc<dyn Store>,
{
    fn store(&self, policy: Policy, gate: Box<dyn Gate>) -> Arc<dyn Store> {
        self(policy, gate)
    }
}

fn blake3(v: &[u8]) -> String {
    algo::lookup("blake3").unwrap().key(v)
}

fn refused_reason(r: Result<impl std::fmt::Debug, Error>) -> String {
    match r {
        Err(Error::Refused { reason, .. }) => reason,
        other => panic!("expected refused, got {other:?}"),
    }
}

const A: Expect = Expect::Absent;

fn val(v: &[u8]) -> Expect {
    Expect::Value(v.to_vec())
}

/// Spec 9.1: key classification and grammar.
pub fn key_grammar(h: &dyn Harness) {
    let s = h.open();
    let bad = ["", "a\nb", "tab\there", "blake3:AF13", "http://x", ":abc", "Blake3:ab", "blake3:"];
    for k in bad {
        assert_eq!(s.read(k), Err(Error::Invalid(Invalid::Key)), "read {k:?}");
        assert_eq!(s.write(k, b"x", &A), Err(Error::Invalid(Invalid::Key)), "write {k:?}");
        assert_eq!(s.remove(k, &A), Err(Error::Invalid(Invalid::Key)), "remove {k:?}");
    }
    // Known algorithm, wrong digest length.
    let short = &blake3(b"x")[..20];
    assert_eq!(s.read(short), Err(Error::Invalid(Invalid::Key)));
    let long = format!("{}00", blake3(b"x"));
    assert_eq!(s.read(&long), Err(Error::Invalid(Invalid::Key)));
    // Well formed, but no such algorithm in the registry.
    assert_eq!(refused_reason(s.read("foo:abcd")), "unsupported-algorithm");
    // Tags may hold ':' after a '/'.
    let derived = format!("derived/blame/{}", blake3(b"x"));
    s.write(&derived, b"v", &A).unwrap();
    assert_eq!(s.read(&derived).unwrap(), Some(b"v".to_vec()));
}

/// Spec 9.2: every registered algorithm round-trips when accepted, and is
/// refused when the policy leaves it out.
pub fn algorithms(h: &dyn Harness) {
    let s = h.open();
    for a in REGISTRY {
        let k = a.key(b"hello\n");
        s.write(&k, b"hello\n", &A).unwrap_or_else(|e| panic!("{}: {e}", a.name));
        assert_eq!(s.read(&k).unwrap(), Some(b"hello\n".to_vec()), "{}", a.name);
    }
    let only_blake3 = h.store(Policy::permissive().with_algorithms(&["blake3"]), Box::new(AllowAll));
    let sha = algo::lookup("sha256").unwrap().key(b"hello\n");
    assert_eq!(refused_reason(only_blake3.write(&sha, b"hello\n", &A)), "unsupported-algorithm");
    assert_eq!(refused_reason(only_blake3.read(&sha)), "unsupported-algorithm");
    only_blake3.write(&blake3(b"hello\n"), b"hello\n", &A).unwrap();
}

/// Spec 9.3: a blob whose bytes do not match its key is never stored.
pub fn hash_mismatch(h: &dyn Harness) {
    let s = h.open();
    let k = blake3(b"right");
    assert_eq!(s.write(&k, b"wrong", &A), Err(Error::Invalid(Invalid::HashMismatch)));
    assert_eq!(s.read(&k).unwrap(), None);
}

/// Spec 9.4: blob writes are idempotent, sequentially and concurrently.
pub fn blob_idempotent(h: &dyn Harness) {
    let s = h.open();
    let k = blake3(b"same");
    s.write(&k, b"same", &A).unwrap();
    s.write(&k, b"same", &A).unwrap();
    s.write(&k, b"same", &val(b"same")).unwrap();
    let k2 = blake3(b"raced");
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let (s, b, k2) = (Arc::clone(&s), Arc::clone(&barrier), k2.clone());
            thread::spawn(move || {
                b.wait();
                s.write(&k2, b"raced", &A)
            })
        })
        .collect();
    for t in handles {
        t.join().unwrap().unwrap();
    }
    assert_eq!(s.read(&k2).unwrap(), Some(b"raced".to_vec()));
}

/// Spec 9.5: blob `expect` must be absent or the value; blob ops never conflict.
pub fn blob_expect(h: &dyn Harness) {
    let s = h.open();
    let k = blake3(b"v");
    assert_eq!(s.write(&k, b"v", &val(b"other")), Err(Error::Invalid(Invalid::Expect)));
    assert_eq!(s.remove(&k, &val(b"other")), Err(Error::Invalid(Invalid::Expect)));
    // Expecting presence on an absent blob is not a conflict.
    s.write(&k, b"v", &val(b"v")).unwrap();
    // Remove is idempotent and ignores whether the blob is there.
    s.remove(&k, &A).unwrap();
    assert_eq!(s.read(&k).unwrap(), None);
    s.remove(&k, &val(b"v")).unwrap();
    s.write(&k, b"v", &A).unwrap();
    assert_eq!(s.read(&k).unwrap(), Some(b"v".to_vec()));
}

/// Spec 9.6: the tag CAS truth table for write and remove.
pub fn tag_cas_table(h: &dyn Harness) {
    let currents: [Option<&[u8]>; 2] = [None, Some(b"A")];
    let expects = [A, val(b"A"), val(b"B")];
    for (i, current) in currents.iter().enumerate() {
        for (j, expect) in expects.iter().enumerate() {
            let s = h.open();
            let t = format!("t{i}{j}");
            if let Some(c) = current {
                s.write(&t, c, &A).unwrap();
            }
            let ok = expect.matches(*current);
            let w = s.write(&t, b"N", expect);
            assert_eq!(w, if ok { Ok(()) } else { Err(Error::Conflict) }, "write {current:?} {expect:?}");
            let after = s.read(&t).unwrap();
            assert_eq!(after.as_deref(), if ok { Some(&b"N"[..]) } else { *current });

            let s = h.open();
            if let Some(c) = current {
                s.write(&t, c, &A).unwrap();
            }
            let r = s.remove(&t, expect);
            assert_eq!(r, if ok { Ok(()) } else { Err(Error::Conflict) }, "remove {current:?} {expect:?}");
            assert_eq!(s.read(&t).unwrap().as_deref(), if ok { None } else { *current });
        }
    }
}

/// Spec 9.7: concurrent CAS has exactly one winner, and a retry loop never
/// loses an update.
pub fn cas_race(h: &dyn Harness) {
    let s = h.open();
    let n = 16;
    let barrier = Arc::new(Barrier::new(n));
    let wins: usize = (0..n)
        .map(|i| {
            let (s, b) = (Arc::clone(&s), Arc::clone(&barrier));
            thread::spawn(move || {
                b.wait();
                s.write("race", format!("{i}").as_bytes(), &A)
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|t| match t.join().unwrap() {
            Ok(()) => 1,
            Err(Error::Conflict) => 0,
            Err(e) => panic!("{e}"),
        })
        .sum();
    assert_eq!(wins, 1);

    let (threads, incs) = (8, 25);
    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let s = Arc::clone(&s);
            thread::spawn(move || {
                for _ in 0..incs {
                    loop {
                        let cur = s.read("counter").unwrap();
                        let n: u64 = cur.as_deref().map_or(0, |v| std::str::from_utf8(v).unwrap().parse().unwrap());
                        let expect = cur.map_or(A, Expect::Value);
                        match s.write("counter", (n + 1).to_string().as_bytes(), &expect) {
                            Ok(()) => break,
                            Err(Error::Conflict) => continue,
                            Err(e) => panic!("{e}"),
                        }
                    }
                }
            })
        })
        .collect();
    for t in handles {
        t.join().unwrap();
    }
    assert_eq!(s.read("counter").unwrap(), Some((threads * incs).to_string().into_bytes()));
}

/// Spec 9.8: an empty value is present.
pub fn empty_values(h: &dyn Harness) {
    let s = h.open();
    s.write("empty", b"", &A).unwrap();
    assert_eq!(s.read("empty").unwrap(), Some(Vec::new()));
    assert_eq!(s.write("empty", b"x", &A), Err(Error::Conflict));
    s.write("empty", b"x", &val(b"")).unwrap();
    let k = blake3(b"");
    s.write(&k, b"", &A).unwrap();
    assert_eq!(s.read(&k).unwrap(), Some(Vec::new()));
}

/// Spec 9.9: size limits are store policy and refuse with a reason.
pub fn size_policy(h: &dyn Harness) {
    let policy = Policy { max_blob_len: Some(4), max_tag_len: Some(2), ..Policy::permissive() };
    let s = h.store(policy, Box::new(AllowAll));
    s.write(&blake3(b"1234"), b"1234", &A).unwrap();
    assert_eq!(refused_reason(s.write(&blake3(b"12345"), b"12345", &A)), "too-large");
    s.write("t", b"12", &A).unwrap();
    assert_eq!(refused_reason(s.write("t", b"123", &val(b"12"))), "too-large");
    assert_eq!(s.read("t").unwrap(), Some(b"12".to_vec()));
    let short_keys = h.store(Policy { max_key_len: Some(8), ..Policy::permissive() }, Box::new(AllowAll));
    short_keys.write("12345678", b"v", &A).unwrap();
    assert_eq!(refused_reason(short_keys.read("123456789")), "key-too-long");
}

/// Spec 9.10: list over spans, both kinds, paging.
pub fn list_spans(h: &dyn Harness) {
    let s = h.store(Policy { list_page: 2, ..Policy::permissive() }, Box::new(AllowAll));
    let tags = ["a", "refs/heads/main", "refs/heads/x", "refs/tags/v1", "z"];
    for t in tags {
        s.write(t, b"v", &A).unwrap();
    }
    let blobs: Vec<String> = (0..3u8).map(|i| blake3(&[i])).collect();
    for (i, b) in blobs.iter().enumerate() {
        s.write(b, &[i as u8], &A).unwrap();
    }
    let mut everything: Vec<String> = tags.iter().map(|t| t.to_string()).chain(blobs.iter().cloned()).collect();
    everything.sort();
    assert_eq!(list_all(&*s, &Span::all()).unwrap(), everything);

    let mut b3 = blobs.clone();
    b3.sort();
    assert_eq!(list_all(&*s, &Span::prefix("blake3:")).unwrap(), b3);
    assert_eq!(list_all(&*s, &Span::prefix("refs/heads/")).unwrap(), ["refs/heads/main", "refs/heads/x"]);
    assert_eq!(list_all(&*s, &Span::range("refs/", "refs/tags/v1")).unwrap(), ["refs/heads/main", "refs/heads/x"]);
    assert_eq!(list_all(&*s, &Span { from: Some(b"refs/tags/v1".to_vec()), to: None }).unwrap(), ["refs/tags/v1", "z"]);
    assert!(list_all(&*s, &Span::range("m", "m")).unwrap().is_empty());
    assert!(list_all(&*s, &Span::range("z", "a")).unwrap().is_empty());

    // Partial answers always progress and never repeat.
    let page = s.list(&Span::all()).unwrap();
    assert!(!page.keys.is_empty());
    let rest = page.rest.expect("more than one page");
    let next = s.list(&rest).unwrap();
    assert!(next.keys[0] > *page.keys.last().unwrap());
}

/// Spec 9.11: outcome order. Denial first and alone; syntax before policy;
/// policy before content checks.
pub fn outcome_order(h: &dyn Harness) {
    let denied = h.store(Policy::permissive(), Box::new(DenyAll));
    assert_eq!(denied.read("bad\nkey"), Err(Error::Denied));
    assert_eq!(denied.write("foo:abcd", b"x", &A), Err(Error::Denied));

    let tight = h.store(
        Policy { max_blob_len: Some(1), ..Policy::permissive() }.with_algorithms(&["blake3"]),
        Box::new(AllowAll),
    );
    assert_eq!(tight.write("blake3:XYZ", b"x", &A), Err(Error::Invalid(Invalid::Key)));
    let sha = algo::lookup("sha256").unwrap().key(b"x");
    assert_eq!(refused_reason(tight.write(&sha, b"wrong", &A)), "unsupported-algorithm");
    // Too large and a hash mismatch: policy is reported.
    assert_eq!(refused_reason(tight.write(&blake3(b"ab"), b"cd", &A)), "too-large");
}

/// Spec 9.12: deny paths change nothing and gate `list` by span.
pub fn deny_paths(h: &dyn Harness) {
    let s = h.store(Policy::permissive(), Box::new(DenyAll));
    let k = blake3(b"x");
    assert_eq!(s.read("t"), Err(Error::Denied));
    assert_eq!(s.write("t", b"x", &A), Err(Error::Denied));
    assert_eq!(s.write(&k, b"x", &A), Err(Error::Denied));
    assert_eq!(s.remove("t", &A), Err(Error::Denied));
    assert_eq!(s.list(&Span::all()), Err(Error::Denied));

    // Writes under ro/ are denied, everything else allowed; list may not
    // overlap secret/.
    let gate = |_: &crate::gate::Who, op: &Op<'_>| match op {
        Op::Write { key, .. } | Op::Remove { key, .. } => !key.starts_with("ro/"),
        Op::Read { key } => !key.starts_with("secret/"),
        Op::List { span } => !overlaps(span, &Span::prefix("secret/")),
    };
    let s = h.store(Policy::permissive(), Box::new(gate));
    assert_eq!(s.write("ro/x", b"v", &A), Err(Error::Denied));
    assert_eq!(s.read("ro/x").unwrap(), None);
    s.write("public/a", b"v", &A).unwrap();
    s.write("secret/a", b"v", &A).unwrap();
    assert_eq!(s.read("secret/a"), Err(Error::Denied));
    assert_eq!(s.list(&Span::all()), Err(Error::Denied));
    assert_eq!(list_all(&*s, &Span::prefix("public/")).unwrap(), ["public/a"]);
}

fn overlaps(a: &Span, b: &Span) -> bool {
    let lo = std::cmp::max(a.from.as_deref().unwrap_or(&[]), b.from.as_deref().unwrap_or(&[]));
    match (a.to.as_deref(), b.to.as_deref()) {
        (None, None) => true,
        (Some(t), None) | (None, Some(t)) => lo < t,
        (Some(x), Some(y)) => lo < std::cmp::min(x, y),
    }
}

/// Spec 9.13: random op sequences match a reference model.
pub fn model(h: &dyn Harness) {
    use proptest::prelude::*;
    use proptest::test_runner::{Config, TestRunner};

    #[derive(Clone, Debug)]
    enum M {
        Read(usize),
        Write(usize, u8, Option<u8>),
        Remove(usize, Option<u8>),
        List(usize, usize),
    }
    // Keys 0..3 are tags, 3..6 blobs of the bytes [0], [1], [2].
    fn key(i: usize) -> String {
        if i < 3 { format!("t{i}") } else { blake3(&[(i - 3) as u8]) }
    }
    let op = prop_oneof![
        (0..6usize).prop_map(M::Read),
        (0..6usize, 0..3u8, proptest::option::of(0..3u8)).prop_map(|(k, v, e)| M::Write(k, v, e)),
        (0..6usize, proptest::option::of(0..3u8)).prop_map(|(k, e)| M::Remove(k, e)),
        (0..6usize, 0..6usize).prop_map(|(a, b)| M::List(a, b)),
    ];

    let mut runner = TestRunner::new(Config { cases: 64, ..Config::default() });
    runner
        .run(&proptest::collection::vec(op, 1..40), |ops| {
            let s = h.store(Policy { list_page: 2, ..Policy::permissive() }, Box::new(AllowAll));
            let mut m: BTreeMap<String, Vec<u8>> = BTreeMap::new();
            for o in ops {
                match o {
                    M::Read(k) => prop_assert_eq!(s.read(&key(k)).unwrap(), m.get(&key(k)).cloned()),
                    M::Write(k, v, e) => {
                        let (k, e) = (key(k), e.map_or(A, |e| val(&[e])));
                        let got = s.write(&k, &[v], &e);
                        let want = if k.starts_with('t') {
                            if e.matches(m.get(&k).map(Vec::as_slice)) {
                                m.insert(k, vec![v]);
                                Ok(())
                            } else {
                                Err(Error::Conflict)
                            }
                        } else if blake3(&[v]) != k {
                            Err(Error::Invalid(Invalid::HashMismatch))
                        } else if matches!(&e, Expect::Value(x) if x != &[v]) {
                            Err(Error::Invalid(Invalid::Expect))
                        } else {
                            m.insert(k, vec![v]);
                            Ok(())
                        };
                        prop_assert_eq!(got, want);
                    }
                    M::Remove(k, e) => {
                        let (k, e) = (key(k), e.map_or(A, |e| val(&[e])));
                        let got = s.remove(&k, &e);
                        let want = if k.starts_with('t') {
                            if e.matches(m.get(&k).map(Vec::as_slice)) {
                                m.remove(&k);
                                Ok(())
                            } else {
                                Err(Error::Conflict)
                            }
                        } else if matches!(&e, Expect::Value(x) if blake3(x) != k) {
                            Err(Error::Invalid(Invalid::Expect))
                        } else {
                            m.remove(&k);
                            Ok(())
                        };
                        prop_assert_eq!(got, want);
                    }
                    M::List(a, b) => {
                        let span = Span::range(key(a), key(b));
                        let want: Vec<String> = m.keys().filter(|k| span.contains(k.as_bytes())).cloned().collect();
                        prop_assert_eq!(list_all(&*s, &span).unwrap(), want);
                    }
                }
            }
            Ok(())
        })
        .unwrap();
}

/// Generate one `#[test]` per conformance check for a harness expression.
#[macro_export]
macro_rules! conformance_tests {
    ($harness:expr) => {
        $crate::conformance_tests!(@each $harness;
            key_grammar algorithms hash_mismatch blob_idempotent blob_expect tag_cas_table
            cas_race empty_values size_policy list_spans outcome_order deny_paths model);
    };
    (@each $harness:expr; $($name:ident)*) => {
        $(
            #[test]
            fn $name() {
                $crate::conformance::$name(&$harness);
            }
        )*
    };
}
