//! Checks for the `uni-bin` and `history` formats and the history tooling.

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Error as CoreError, Expect, Policy, Session, Store, Who};
use cas_version::history::{self, History, NodeId, Step};
use cas_version::uni::Value;
use cas_version::{Error, Node, Path, View, build, formats, put};
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

fn version(s: &dyn Store, name: &str) -> View {
    let f = put(s, b3(), "raw", name.as_bytes()).unwrap();
    build(s, b3(), formats::by_alias("dir").unwrap(), &BTreeMap::from([(Path::new(name).unwrap(), Node::File(f))]))
        .unwrap()
}

fn step(msg: &str) -> Step {
    Step { changelog: Some(msg.into()), compatible: false, notes: None }
}

fn ok(msg: &str) -> Step {
    Step { changelog: Some(msg.into()), compatible: true, notes: None }
}

/// A history given as (node version, [(predecessor, step)]).
fn hist(nodes: &[(&View, &[(&NodeId, Step)])]) -> (History, Vec<NodeId>) {
    let mut h = History::default();
    let mut ids = Vec::new();
    for (v, after) in nodes {
        let after = after.iter().map(|(p, s)| ((*p).clone(), s.clone())).collect();
        ids.push(h.add(v, after, None).unwrap());
    }
    (h, ids)
}

#[test]
fn histories_round_trip_and_are_content_addressed() {
    let s = store();
    let (v1, v2) = (version(&s, "a"), version(&s, "b"));
    let mut h = History { doc: Some("demo".into()), ..History::default() };
    let n1 = h.append(&v1, Step::default(), Some("first".into())).unwrap();
    let n2 = h.append(&v2, ok("rename"), None).unwrap();
    h.tags.insert("v1".into(), n1.clone());
    let r = history::write(&s, b3(), &h).unwrap();
    assert_eq!(history::read(&s, &r).unwrap(), h);
    assert_eq!(history::write(&s, b3(), &h).unwrap(), r);
    // The root: doc, tags, tips and the top of the node tree, here a leaf.
    let bytes = s.read(r.key()).unwrap().unwrap();
    let root = Value::deserialize(&bytes).unwrap();
    let top = root.as_map().unwrap()[&Value::str("nodes")].as_str().unwrap().to_string();
    assert_eq!(
        root.to_text(),
        // Keys sort by their serialization: shorter first.
        format!("{{'doc': 'demo', 'tags': {{'v1': '{n1}'}}, 'tips': {{'{n2}': {{}}}}, 'nodes': '{top}'}}")
    );
    let leaf = s.read(View::parse(&top).unwrap().key()).unwrap().unwrap();
    assert_eq!(
        Value::deserialize(&leaf).unwrap().to_text(),
        format!(
            "{{'nodes': {{'{n1}': {{'doc': 'first', 'generation': '1'}}, '{n2}': {{'after': {{'{n1}': {{'changelog': 'rename', 'compatible': {{}}}}}}, 'generation': '2'}}}}}}"
        )
    );
    // A node is its version: nothing about the history is in it.
    assert_eq!(n1.version(), v1);
    assert_eq!(version(&s, "a"), v1);
    // The same version again gets an occurrence suffix.
    let n3 = h.append(&v1, step("revert"), None).unwrap();
    assert_eq!(n3.as_str(), format!("{v1}#2"));
    assert_eq!(n3.version(), v1);
}

#[test]
fn malformed_histories_are_rejected() {
    let s = store();
    let v = version(&s, "a");
    let id = v.to_string();
    let hf = formats::by_alias("history").unwrap();
    let node = |after: Value| Value::map([("after", after)]);
    let bad = [
        Value::str("x"),
        Value::map([("extra", Value::empty())]),
        Value::map([("nodes", Value::map([("nope", Value::empty())]))]),
        Value::map([("nodes", Value::map([(format!("{id}#1"), Value::empty())]))]),
        Value::map([("nodes", Value::map([(format!("{id}#02"), Value::empty())]))]),
        Value::map([("nodes", Value::map([(id.clone(), Value::map([("x", Value::empty())]))]))]),
        // A predecessor that is not a node.
        Value::map([("nodes", Value::map([(id.clone(), node(Value::map([(format!("{id}#2"), Value::empty())])))]))]),
        // A cycle.
        Value::map([(
            "nodes",
            Value::map([
                (id.clone(), node(Value::map([(format!("{id}#2"), Value::empty())]))),
                (format!("{id}#2"), node(Value::map([(id.clone(), Value::empty())]))),
            ]),
        )]),
        Value::map([("nodes", Value::map([(id.clone(), node(Value::map([(id.clone(), Value::empty())])))]))]),
        // compatible is {} when present.
        Value::map([(
            "nodes",
            Value::map([
                (id.clone(), Value::empty()),
                (format!("{id}#2"), node(Value::map([(id.clone(), Value::map([("compatible", "yes")]))]))),
            ]),
        )]),
        Value::map([("tags", Value::map([("v1", id.as_str())]))]),
        Value::map([("nodes", Value::map([(id.clone(), Value::empty())])), ("tags", Value::map([("", id.as_str())]))]),
    ];
    for b in bad {
        let bytes = b.serialize();
        assert!(hf.check("k", &bytes).is_err(), "{b}");
        assert!(matches!(put(&s, b3(), "history", &bytes), Err(Error::Malformed { .. })));
    }
    assert!(matches!(history::read(&s, &v), Err(Error::UnknownFormat(_))));
    assert!(put(&s, b3(), "history", &Value::empty().serialize()).is_ok());
}

#[test]
fn commit_moves_the_name_by_compare_and_swap() {
    let s = store();
    let (v1, v2) = (version(&s, "a"), version(&s, "b"));
    assert_eq!(history::head(&s, "main").unwrap(), None);
    let n1 = history::commit(&s, b3(), "main", &v1, step("one"), None).unwrap();
    let n2 = history::commit(&s, b3(), "main", &v2, step("two"), None).unwrap();
    let (old, h) = history::load(&s, "main").unwrap();
    assert_eq!(h.log(), vec![n2.clone(), n1.clone()]);
    assert_eq!(h.nodes[&n2].after[&n1], step("two"));

    // Someone else moves the name between our load and our save.
    let mut mine = h.clone();
    mine.append(&v1, step("mine"), None).unwrap();
    history::commit(&s, b3(), "main", &v1, step("theirs"), None).unwrap();
    let r = history::save(&s, b3(), "main", old.as_ref(), &mine);
    assert_eq!(r, Err(Error::Store(CoreError::Conflict)));
    // `record` retries from the new value instead.
    let (_, h) = history::load(&s, "main").unwrap();
    assert_eq!(h.nodes.len(), 3);
    let _ = Expect::Absent;
}

#[test]
fn log_merge_bases_compatibility_and_extension() {
    let s = store();
    let (v0, va, vb, vc, vm) =
        (version(&s, "0"), version(&s, "a"), version(&s, "b"), version(&s, "c"), version(&s, "m"));
    //   root - a1 - a2 ----- m
    //      \               /
    //       b1 ------------
    let mut h = History::default();
    let root = h.add(&v0, BTreeMap::new(), None).unwrap();
    let a1 = h.add(&va, BTreeMap::from([(root.clone(), ok("a1"))]), None).unwrap();
    let a2 = h.add(&vb, BTreeMap::from([(a1.clone(), ok("a2"))]), None).unwrap();
    let b1 = h.add(&vc, BTreeMap::from([(root.clone(), step("b1"))]), None).unwrap();
    let before = h.clone();
    let m = h.add(&vm, BTreeMap::from([(a2.clone(), ok("m")), (b1.clone(), step("m"))]), None).unwrap();

    let log = h.log();
    assert_eq!(log.len(), 5);
    assert_eq!((&log[0], &log[4]), (&m, &root));
    let pos = |r: &NodeId| log.iter().position(|x| x == r).unwrap();
    assert!(pos(&a2) < pos(&a1) && pos(&a1) < pos(&root) && pos(&b1) < pos(&root));
    assert_eq!(h.tips(), BTreeSet::from([m.clone()]));
    assert_eq!(before.tips(), BTreeSet::from([a2.clone(), b1.clone()]));

    assert_eq!(h.merge_bases(&a2, &b1), BTreeSet::from([root.clone()]));
    assert_eq!(h.merge_bases(&m, &a1), BTreeSet::from([a1.clone()]));
    assert_eq!(h.merge_bases(&a2, &a2), BTreeSet::from([a2.clone()]));

    // Compatible along root-a1-a2-m, not along b1.
    assert!(h.compatible(&root, &m) && h.compatible(&a1, &a2) && h.compatible(&b1, &b1));
    assert!(!h.compatible(&root, &b1) && !h.compatible(&b1, &m) && !h.compatible(&m, &root));

    // Extension ignores tags and docs, not steps.
    assert!(h.extends(&before) && !before.extends(&h));
    let mut tagged = before.clone();
    tagged.tags.insert("v1".into(), a1.clone());
    tagged.doc = Some("x".into());
    assert!(tagged.extends(&before) && before.extends(&tagged));
    let mut rewritten = before.clone();
    rewritten.nodes.get_mut(&a2).unwrap().after.insert(a1.clone(), step("reworded"));
    assert!(!rewritten.extends(&before));

    // Criss-cross: two merges of a1 and b1 have both as best common ancestors.
    let x = h.add(&vm, BTreeMap::from([(a1.clone(), step("x")), (b1.clone(), step("x"))]), None).unwrap();
    let y = h.add(&vm, BTreeMap::from([(b1.clone(), step("y")), (a1.clone(), step("y"))]), None).unwrap();
    assert_eq!(h.merge_bases(&x, &y), BTreeSet::from([a1, b1]));

    // Unrelated lines share nothing.
    let other = h.add(&va, BTreeMap::new(), None).unwrap();
    assert!(h.merge_bases(&m, &other).is_empty());
    let _ = hist;
}

proptest! {
    /// merge_bases agrees with the definition: common ancestors that no
    /// other common ancestor follows.
    #[test]
    fn merge_bases_match_the_definition(edges in prop::collection::vec(prop::collection::vec(any::<prop::sample::Index>(), 0..3), 1..25), a in any::<prop::sample::Index>(), b in any::<prop::sample::Index>()) {
        let s = store();
        let mut h = History::default();
        let mut ids: Vec<NodeId> = Vec::new();
        for (i, preds) in edges.iter().enumerate() {
            let after: BTreeMap<NodeId, Step> =
                if ids.is_empty() { BTreeMap::new() } else { preds.iter().map(|p| (ids[p.index(ids.len())].clone(), Step::default())).collect() };
            ids.push(h.add(&version(&s, &format!("v{i}")), after, None).unwrap());
        }
        let (a, b) = (&ids[a.index(ids.len())], &ids[b.index(ids.len())]);
        let common: BTreeSet<NodeId> = h.ancestors(a).intersection(&h.ancestors(b)).cloned().collect();
        let want: BTreeSet<NodeId> = common
            .iter()
            .filter(|c| !common.iter().any(|d| d != *c && h.follows(d, c)))
            .cloned()
            .collect();
        prop_assert_eq!(h.merge_bases(a, b), want);
    }
}

/// Values shaped like histories, valid or nearly so.
fn history_like() -> impl Strategy<Value = Value> {
    let s = store();
    let ids: Vec<String> = (0..3).map(|i| NodeId::of(&version(&s, &format!("h{i}"))).to_string()).collect();
    let id = prop::sample::select(ids);
    let text = prop_oneof![Just(""), Just("note"), Just("é")].prop_map(Value::str);
    let empty = prop_oneof![4 => Just(Value::empty()), 1 => text.clone()];
    let step = prop::collection::btree_map(
        prop_oneof![Just("changelog"), Just("compatible"), Just("other")].prop_map(Value::str),
        prop_oneof![text.clone(), empty],
        0..3,
    )
    .prop_map(Value::Map);
    let after = prop::collection::btree_map(id.clone().prop_map(Value::str), step, 0..3).prop_map(Value::Map);
    let node = prop::collection::btree_map(
        prop_oneof![Just("after"), Just("doc"), Just("x")].prop_map(Value::str),
        prop_oneof![after, text.clone()],
        0..3,
    )
    .prop_map(Value::Map);
    let nodes = prop::collection::btree_map(
        prop_oneof![8 => id.clone().prop_map(Value::str), 1 => Just(Value::str("bad id"))],
        node,
        0..4,
    )
    .prop_map(Value::Map);
    let tags = prop::collection::btree_map(
        prop_oneof![Just("v1"), Just("")].prop_map(Value::str),
        prop_oneof![id.prop_map(Value::str), Just(Value::empty())],
        0..2,
    )
    .prop_map(Value::Map);
    prop::collection::btree_map(
        prop_oneof![Just("doc"), Just("nodes"), Just("tags"), Just("zz")].prop_map(Value::str),
        prop_oneof![nodes, tags, text],
        0..4,
    )
    .prop_map(Value::Map)
}

proptest! {
    /// The direct decoder agrees with deserialize plus from_value: the same
    /// history, or an error for an error, also on damaged bytes.
    #[test]
    fn decode_agrees_with_from_value(v in history_like(), flip in any::<prop::sample::Index>()) {
        let slow = |b: &[u8]| Value::deserialize(b).and_then(|v| History::from_value(&v)).ok();
        let bytes = v.serialize();
        prop_assert_eq!(History::decode(&bytes).ok(), slow(&bytes));
        let mut damaged = bytes.clone();
        let i = flip.index(damaged.len());
        damaged[i] ^= 0x41;
        prop_assert_eq!(History::decode(&damaged).ok(), slow(&damaged));
        let cut = &bytes[..i];
        prop_assert_eq!(History::decode(cut).ok(), slow(cut));
    }
}

/// A history saved before steps had notes names the format's first
/// version: it is read as a history still.
#[test]
fn histories_of_the_first_format_version_still_read() {
    let s = store();
    let v = version(&s, "a");
    history::commit(&s, b3(), "main", &v, Step::default(), None).unwrap();
    let head = history::head(&s, "main").unwrap().unwrap();
    let hf = formats::by_alias("history").unwrap();
    let (_, _, old_id) = &hf.older_versions()[0];
    let old = View::parse(&format!("{old_id} {}", head.key())).unwrap();
    assert!(old.known_format().is_some_and(|f| f.alias == "history"));
    assert_eq!(history::read(&s, &old).unwrap(), history::read(&s, &head).unwrap());
    // Its spec and format object are published with the current ones.
    formats::publish(&s).unwrap();
    let (spec, object) = hf.described_by(old_id);
    assert_eq!(algo::lookup("blake3").unwrap().key(object.as_bytes()), *old_id);
    assert!(s.read(&algo::lookup("blake3").unwrap().key(spec.as_bytes())).unwrap().is_some());
}
