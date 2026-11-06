//! Checks for the `uni-bin` and `record` formats and the history tooling
//! (Stage 5).

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Error as CoreError, Expect, Policy, Session, Store, Who};
use cas_version::history::{self, Record};
use cas_version::uni::Value;
use cas_version::{Error, Node, Path, Ref, build, formats, put};
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn store() -> Session {
    CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous())
}

fn value_strategy() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![Just(""), Just("a"), Just("b"), Just("it's"), Just("\\"), Just("é\n"), Just("{}: , '")]
        .prop_map(Value::str);
    leaf.prop_recursive(4, 32, 6, |inner| prop::collection::btree_map(inner.clone(), inner, 0..6).prop_map(Value::Map))
}

proptest! {
    #[test]
    fn uni_round_trips(v in value_strategy()) {
        let bytes = v.serialize();
        prop_assert_eq!(Value::deserialize(&bytes).unwrap(), v.clone());
        prop_assert_eq!(Value::from_text(&v.to_text()).unwrap(), v.clone());
        // No other byte string decodes to this value: any one-byte change
        // fails or decodes to something else.
        for i in 0..bytes.len() {
            let mut b = bytes.clone();
            b[i] ^= 1;
            prop_assert_ne!(Value::deserialize(&b).ok(), Some(v.clone()));
        }
        prop_assert!(Value::deserialize(&[bytes.clone(), vec![0]].concat()).is_err());
    }

    #[test]
    fn uni_map_order_is_serialization_order(m in prop::collection::btree_map(value_strategy(), value_strategy(), 0..8)) {
        let keys: Vec<Vec<u8>> = m.keys().map(Value::serialize).collect();
        prop_assert!(keys.windows(2).all(|w| w[0] < w[1]));
    }
}

#[test]
fn uni_serialization_examples() {
    let v = Value::map([("a", Value::str("x")), ("b", Value::empty())]);
    assert_eq!(v.serialize(), [1, 2, 0, 1, b'a', 0, 1, b'x', 0, 1, b'b', 1, 0]);
    assert_eq!(v.to_text(), "{'a': 'x', 'b': {}}");
    let long = Value::str("x".repeat(300));
    assert_eq!(&long.serialize()[..3], [0, 0xac, 0x02]);
    assert_eq!(Value::str("it's \\").to_text(), r"'it\'s \\'");

    let bad: &[&[u8]] = &[
        &[],
        &[2, 0],
        &[0, 2, b'a'],
        &[0, 0x80, 0],                               // length not in shortest form
        &[0, 1, 0xff],                               // not UTF-8
        &[1, 2, 0, 1, b'b', 1, 0, 0, 1, b'a', 1, 0], // keys out of order
        &[1, 2, 0, 1, b'a', 1, 0, 0, 1, b'a', 1, 0], // duplicate key
        &[1, 1, 0, 1, b'a'],
    ];
    for b in bad {
        assert!(Value::deserialize(b).is_err(), "{b:?}");
    }
    // The same value in both formats: different blobs, lossless both ways.
    let s = store();
    let (bin, text) =
        (put(&s, b3(), "uni-bin", &v.serialize()).unwrap(), put(&s, b3(), "uni-txt", v.to_text().as_bytes()).unwrap());
    assert_ne!(bin.key(), text.key());
    assert_ne!(bin.format(), text.format());
    assert!(put(&s, b3(), "uni-txt", b"{'b': {}, 'a': 'x'}").is_err());
    for t in ["{'b': {}, 'a': 'x'}", "{'a':'x'}", "'a' ", "\"a\"", "'a\\n'", "{ }", "{'a': 'x',}"] {
        assert!(Value::from_text(t).is_err(), "{t}");
    }
    // Nesting is bounded, so a small hostile blob cannot overflow the stack.
    let nested = |n: usize| {
        let mut v = Value::empty();
        for _ in 1..n {
            v = Value::map([("k", v)]);
        }
        v
    };
    let deepest = nested(cas_version::uni::MAX_DEPTH);
    assert_eq!(Value::deserialize(&deepest.serialize()).unwrap(), deepest);
    assert_eq!(Value::from_text(&deepest.to_text()).unwrap(), deepest);
    let deeper = Value::map([("k", deepest)]);
    assert!(Value::deserialize(&deeper.serialize()).is_err());
    assert!(Value::from_text(&deeper.to_text()).is_err());
    let hostile: Vec<u8> = [1u8, 1, 0, 0].repeat(1_000_000);
    assert!(Value::deserialize(&hostile).is_err());
    assert!(Value::from_text(&"{'': ".repeat(1_000_000)).is_err());
}

fn version(s: &dyn Store, name: &str) -> Ref {
    let f = put(s, b3(), "raw", name.as_bytes()).unwrap();
    build(s, b3(), formats::by_alias("dir").unwrap(), &BTreeMap::from([(Path::new(name).unwrap(), Node::File(f))]))
        .unwrap()
}

fn meta(msg: &str) -> BTreeMap<Value, Value> {
    BTreeMap::from([(Value::str("message"), Value::str(msg))])
}

fn rec(s: &dyn Store, v: &Ref, parents: &[&Ref], msg: &str) -> Ref {
    let parents = parents.iter().map(|p| (*p).clone()).collect();
    history::write(s, b3(), &Record { version: v.clone(), parents, meta: meta(msg) }).unwrap()
}

#[test]
fn records_round_trip_and_are_content_addressed() {
    let s = store();
    let v = version(&s, "a");
    let r1 = rec(&s, &v, &[], "first");
    let r2 = rec(&s, &v, &[&r1], "second");
    let got = history::read(&s, &r2).unwrap();
    assert_eq!(got, Record { version: v.clone(), parents: BTreeSet::from([r1.clone()]), meta: meta("second") });
    // The same record written again is the same blob.
    assert_eq!(rec(&s, &v, &[&r1], "second"), r2);
    // The stored bytes are uni, keyed by their hash, with this exact text.
    let bytes = s.read(r2.key()).unwrap().unwrap();
    assert_eq!(b3().key(&bytes), r2.key());
    assert_eq!(
        Value::deserialize(&bytes).unwrap().to_text(),
        format!("{{'meta': {{'message': 'second'}}, 'parents': {{'{r1}': {{}}}}, 'version': '{v}'}}")
    );
    // Versions do not change when records point at them.
    assert_eq!(version(&s, "a"), v);
}

#[test]
fn malformed_records_are_rejected() {
    let s = store();
    let v = version(&s, "a");
    let rf = formats::by_alias("record").unwrap();
    let bad = [
        Value::str("x"),
        Value::map([("version", Value::str(v.to_string()))]),
        Value::map([("meta", Value::empty()), ("parents", Value::empty()), ("version", Value::str("nope"))]),
        Value::map([
            ("meta", Value::empty()),
            ("parents", Value::map([(v.to_string(), "x")])),
            ("version", Value::str(v.to_string())),
        ]),
        Value::map([
            ("extra", Value::empty()),
            ("meta", Value::empty()),
            ("parents", Value::empty()),
            ("version", Value::str(v.to_string())),
        ]),
    ];
    for b in bad {
        let bytes = b.serialize();
        assert!(rf.check("k", &bytes).is_err(), "{b}");
        assert!(matches!(put(&s, b3(), "record", &bytes), Err(Error::Malformed { .. })));
    }
    assert!(matches!(history::read(&s, &v), Err(Error::UnknownFormat(_))));
}

#[test]
fn commit_moves_the_branch_by_compare_and_swap() {
    let s = store();
    let (v1, v2) = (version(&s, "a"), version(&s, "b"));
    assert_eq!(history::head(&s, "main").unwrap(), None);
    let c1 = history::commit(&s, b3(), "main", &v1, meta("one")).unwrap();
    let c2 = history::commit(&s, b3(), "main", &v2, meta("two")).unwrap();
    assert_eq!(history::head(&s, "main").unwrap(), Some(c2.clone()));
    assert_eq!(history::read(&s, &c2).unwrap().parents, BTreeSet::from([c1.clone()]));

    // Someone else moves the branch between our read and our swap.
    let mine = history::write(&s, b3(), &Record { version: v1.clone(), parents: [c2.clone()].into(), meta: meta("x") })
        .unwrap();
    let theirs = history::commit(&s, b3(), "main", &v1, meta("theirs")).unwrap();
    let r = s.write("main", mine.to_string().as_bytes(), &Expect::Value(c2.to_string().into_bytes()));
    assert_eq!(r, Err(CoreError::Conflict));
    assert_eq!(history::head(&s, "main").unwrap(), Some(theirs));
}

#[test]
fn log_and_merge_bases() {
    let s = store();
    let v = version(&s, "a");
    //   root - a1 - a2 ----- m
    //      \             /
    //       b1 --------
    let root = rec(&s, &v, &[], "root");
    let a1 = rec(&s, &v, &[&root], "a1");
    let a2 = rec(&s, &v, &[&a1], "a2");
    let b1 = rec(&s, &v, &[&root], "b1");
    let m = rec(&s, &v, &[&a2, &b1], "m");

    let log: Vec<Ref> = history::log(&s, &m).unwrap().into_iter().map(|(r, _)| r).collect();
    assert_eq!(log.len(), 5);
    assert_eq!(log[0], m);
    assert_eq!(log[4], root);
    let pos = |r: &Ref| log.iter().position(|x| x == r).unwrap();
    assert!(pos(&a2) < pos(&a1) && pos(&a1) < pos(&root) && pos(&b1) < pos(&root));

    assert_eq!(history::merge_bases(&s, &a2, &b1).unwrap(), BTreeSet::from([root.clone()]));
    assert_eq!(history::merge_bases(&s, &m, &a1).unwrap(), BTreeSet::from([a1.clone()]));
    assert_eq!(history::merge_bases(&s, &a2, &a2).unwrap(), BTreeSet::from([a2.clone()]));

    // Criss-cross: two merges of a1 and b1 in opposite orders have both as
    // best common ancestors.
    let x = rec(&s, &v, &[&a1, &b1], "x");
    let y = rec(&s, &v, &[&b1, &a1], "y-differs");
    assert_eq!(history::merge_bases(&s, &x, &y).unwrap(), BTreeSet::from([a1, b1]));

    // Unrelated histories share nothing.
    let other = rec(&s, &v, &[], "other");
    assert!(history::merge_bases(&s, &m, &other).unwrap().is_empty());
}
