//! Checks for three-way merge.

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Policy, Session, Who};
use cas_version::history::{History, NodeId, Step};
use cas_version::merge::{self, Outcome, diff3, merge_versions};
use cas_version::{Edit, Node, Path, Ref, build, formats, lookup, put, read_blob, update, walk};
use proptest::prelude::*;
use std::collections::BTreeMap;

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn store() -> Session {
    CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous())
}

fn file(s: &Session, text: &str) -> Node {
    Node::File(put(s, b3(), "raw", text.as_bytes()).unwrap())
}

fn p(s: &str) -> Path {
    Path::new(s).unwrap()
}

fn tree(s: &Session, entries: &[(&str, &str)]) -> Ref {
    let empty = build(s, b3(), formats::by_alias("dir").unwrap(), &BTreeMap::new()).unwrap();
    let edits = entries
        .iter()
        .map(|(path, text)| (p(path), Edit::Set(if *text == "/" { Node::Dir } else { file(s, text) })))
        .collect();
    update(s, b3(), &empty, &edits).unwrap()
}

fn edit(s: &Session, v: &Ref, path: &str, text: Option<&str>) -> Ref {
    let e = match text {
        None => Edit::Remove,
        Some("/") => Edit::Set(Node::Dir),
        Some(t) => Edit::Set(file(s, t)),
    };
    update(s, b3(), v, &BTreeMap::from([(p(path), e)])).unwrap()
}

fn text_at(s: &Session, v: &Ref, path: &str) -> Option<String> {
    match lookup(s, v, &p(path)).unwrap()? {
        Node::File(r) => Some(String::from_utf8(read_blob(s, r.key()).unwrap()).unwrap()),
        Node::Dir => Some("/".into()),
    }
}

#[test]
fn diff3_cases() {
    let l = |s: &'static str| s.split_inclusive('\n').collect::<Vec<_>>();
    let base = l("a\nb\nc\nd\n");
    assert_eq!(diff3(&base, &l("A\nb\nc\nd\n"), &l("a\nb\nc\nD\n")).unwrap().concat(), "A\nb\nc\nD\n");
    assert_eq!(diff3(&base, &l("a\nb\nx\nc\nd\n"), &l("a\nb\nc\nd\ny\n")).unwrap().concat(), "a\nb\nx\nc\nd\ny\n");
    assert_eq!(diff3(&base, &l("a\nc\nd\n"), &l("a\nb\nc\n")).unwrap().concat(), "a\nc\n");
    assert_eq!(diff3(&base, &l("a\nB\nc\nd\n"), &l("a\nB\nc\nd\n")).unwrap().concat(), "a\nB\nc\nd\n");
    assert!(diff3(&base, &l("a\nX\nc\nd\n"), &l("a\nY\nc\nd\n")).is_none());
    assert_eq!(diff3(&[], &l("x\n"), &[]).unwrap().concat(), "x\n");
    assert!(diff3(&[], &l("x\n"), &l("y\n")).is_none());
}

#[test]
fn path_level_merges() {
    let s = store();
    let base = tree(&s, &[("a/x", "1\n"), ("a/y", "2\n"), ("b", "3\n"), ("d/z", "4\n")]);
    // Disjoint edits combine.
    let ours = edit(&s, &base, "a/x", Some("ours\n"));
    let theirs = edit(&s, &edit(&s, &base, "b", None), "c", Some("new\n"));
    let m = merge_versions(&s, b3(), &base, &ours, &theirs).unwrap();
    assert!(m.conflicts.is_empty());
    let want = edit(&s, &edit(&s, &ours, "b", None), "c", Some("new\n"));
    assert_eq!(m.version, want);
    // And the other way round gives the same version.
    assert_eq!(merge_versions(&s, b3(), &base, &theirs, &ours).unwrap().version, want);

    // Same change on both sides is no conflict.
    let m = merge_versions(&s, b3(), &base, &ours, &ours).unwrap();
    assert_eq!((m.version, m.conflicts.len()), (ours.clone(), 0));

    // Different changes to one path: conflict, ours kept.
    let theirs = edit(&s, &base, "a/x", Some("theirs\n"));
    let m = merge_versions(&s, b3(), &base, &ours, &theirs).unwrap();
    assert_eq!(m.conflicts.len(), 1);
    assert_eq!(m.conflicts[0].path, p("a/x"));
    assert_eq!(text_at(&s, &m.version, "a/x").as_deref(), Some("ours\n"));

    // They removed a directory we changed inside: conflict at the
    // directory, and our change survives.
    let theirs = edit(&s, &base, "a", None);
    let m = merge_versions(&s, b3(), &base, &ours, &theirs).unwrap();
    assert!(m.conflicts.iter().any(|c| c.path == p("a")));
    assert_eq!(text_at(&s, &m.version, "a/x").as_deref(), Some("ours\n"));

    // We replaced a directory by a file; they added inside it: conflict.
    let ours2 = edit(&s, &base, "d", Some("file now\n"));
    let theirs2 = edit(&s, &base, "d/new", Some("n\n"));
    let m = merge_versions(&s, b3(), &base, &ours2, &theirs2).unwrap();
    assert_eq!(m.conflicts.iter().map(|c| c.path.to_string()).collect::<Vec<_>>(), ["d/new"]);
    assert_eq!(text_at(&s, &m.version, "d").as_deref(), Some("file now\n"));

    // Both add files in a new directory, and an empty one: they combine.
    let o = edit(&s, &base, "n/o", Some("o\n"));
    let t = edit(&s, &edit(&s, &base, "n/t", Some("t\n")), "e", Some("/"));
    let m = merge_versions(&s, b3(), &base, &o, &t).unwrap();
    assert!(m.conflicts.is_empty());
    assert_eq!(text_at(&s, &m.version, "n/t").as_deref(), Some("t\n"));
    assert_eq!(text_at(&s, &m.version, "e").as_deref(), Some("/"));
}

#[test]
fn line_level_merge_inside_one_file() {
    let s = store();
    let base = tree(&s, &[("f", "a\nb\nc\nd\n")]);
    let ours = edit(&s, &base, "f", Some("A\nb\nc\nd\n"));
    let theirs = edit(&s, &base, "f", Some("a\nb\nc\nD\n"));
    let m = merge_versions(&s, b3(), &base, &ours, &theirs).unwrap();
    assert!(m.conflicts.is_empty());
    assert_eq!(text_at(&s, &m.version, "f").as_deref(), Some("A\nb\nc\nD\n"));
}

fn step(msg: &str) -> Step {
    Step { changelog: Some(msg.into()), compatible: false }
}

#[test]
fn merging_two_tips_of_a_history() {
    let s = store();
    let v0 = tree(&s, &[("f", "a\nb\nc\n")]);
    let mut h = History::default();
    let root = h.append(&v0, Step::default(), None).unwrap();
    let ours =
        h.add(&edit(&s, &v0, "f", Some("A\nb\nc\n")), BTreeMap::from([(root.clone(), step("A"))]), None).unwrap();
    let theirs =
        h.add(&edit(&s, &v0, "f", Some("a\nb\nC\n")), BTreeMap::from([(root.clone(), step("C"))]), None).unwrap();
    assert!(h.append(&v0, Step::default(), None).is_err(), "two tips: append refuses");

    let Outcome::Done(m) = merge::merge_nodes(&s, b3(), &mut h, &ours, &theirs, &step("merge")).unwrap() else {
        panic!()
    };
    assert_eq!(h.nodes[&m].after.keys().cloned().collect::<Vec<_>>().len(), 2);
    assert_eq!(text_at(&s, &m.version(), "f").as_deref(), Some("A\nb\nC\n"));
    assert_eq!(h.tip(), Some(m.clone()));
    assert_eq!(merge::merge_nodes(&s, b3(), &mut h, &m, &ours, &step("x")).unwrap(), Outcome::UpToDate);

    // A real conflict records nothing.
    let g1 = h.add(&edit(&s, &m.version(), "g", Some("1\n")), BTreeMap::from([(m.clone(), step("1"))]), None).unwrap();
    let g2 = h.add(&edit(&s, &m.version(), "g", Some("2\n")), BTreeMap::from([(m.clone(), step("2"))]), None).unwrap();
    let before = h.clone();
    let Outcome::Conflicts(c) = merge::merge_nodes(&s, b3(), &mut h, &g1, &g2, &step("x")).unwrap() else { panic!() };
    assert_eq!(c.conflicts.iter().map(|c| c.path.to_string()).collect::<Vec<_>>(), ["g"]);
    assert_eq!(h, before);
}

#[test]
fn criss_cross_uses_a_virtual_base() {
    let s = store();
    let mut h = History::default();
    let after = |ps: &[&NodeId]| ps.iter().map(|p| ((*p).clone(), Step::default())).collect::<BTreeMap<_, _>>();
    let v0 = tree(&s, &[("x", "0\n"), ("y", "0\n")]);
    let r0 = h.add(&v0, after(&[]), None).unwrap();
    let va = edit(&s, &v0, "x", Some("a\n"));
    let vb = edit(&s, &v0, "y", Some("b\n"));
    let (ra, rb) = (h.add(&va, after(&[&r0]), None).unwrap(), h.add(&vb, after(&[&r0]), None).unwrap());
    let vab = edit(&s, &va, "y", Some("b\n"));
    let (m1, m2) = (h.add(&vab, after(&[&ra, &rb]), None).unwrap(), h.add(&vab, after(&[&rb, &ra]), None).unwrap());
    // Each side then edits a different file.
    let o = h.add(&edit(&s, &vab, "x", Some("o\n")), after(&[&m1]), None).unwrap();
    let t = h.add(&edit(&s, &vab, "y", Some("t\n")), after(&[&m2]), None).unwrap();
    let Outcome::Done(m) = merge::merge_nodes(&s, b3(), &mut h, &o, &t, &Step::default()).unwrap() else { panic!() };
    let v = m.version();
    assert_eq!((text_at(&s, &v, "x").as_deref(), text_at(&s, &v, "y").as_deref()), (Some("o\n"), Some("t\n")));
}

#[test]
fn private_work_is_rebased_and_published_as_one_step() {
    let s = store();
    let v0 = tree(&s, &[("f", "a\nb\nc\n"), ("g", "1\n")]);
    let mut public = History::default();
    let p0 = public.append(&v0, Step::default(), None).unwrap();

    // Private history: a copy of public plus small steps.
    let mut mine = public.clone();
    let v1 = edit(&s, &v0, "f", Some("A\nb\nc\n"));
    mine.append(&v1, step("wip 1"), None).unwrap();
    let v2 = edit(&s, &v1, "f", Some("A\nB\nc\n"));
    mine.append(&v2, step("wip 2"), None).unwrap();

    // Meanwhile public moved.
    let vp = edit(&s, &v0, "g", Some("2\n"));
    let p1 = public.append(&vp, step("g"), None).unwrap();
    assert!(matches!(
        merge::publish(&mine, &mut public.clone(), step("x"), None),
        Err(cas_version::Error::Diverged(_))
    ));

    // Rebase replays the two private steps onto the public tip.
    let (rebased, outcome) = merge::rebase(&s, b3(), &mine, &public).unwrap();
    let Outcome::Done(tip) = outcome else { panic!("{outcome:?}") };
    assert!(rebased.extends(&public));
    assert_eq!(rebased.nodes.len(), 4);
    assert_eq!(text_at(&s, &tip.version(), "f").as_deref(), Some("A\nB\nc\n"));
    assert_eq!(text_at(&s, &tip.version(), "g").as_deref(), Some("2\n"));
    assert_eq!(rebased.nodes[&tip].after.values().next().unwrap(), &step("wip 2"));

    // Publishing adds one node: the private steps stay private.
    let mut next = public.clone();
    let Outcome::Done(n) =
        merge::publish(&rebased, &mut next, Step { compatible: true, ..step("f: A and B") }, None).unwrap()
    else {
        panic!()
    };
    assert_eq!(next.nodes.len(), 3);
    assert!(next.extends(&public));
    assert_eq!(n.version(), tip.version());
    assert_eq!(next.nodes[&n].after.keys().collect::<Vec<_>>(), [&p1]);
    assert!(next.compatible(&p1, &n) && !next.compatible(&p0, &n));
    assert_eq!(merge::publish(&rebased, &mut next, step("again"), None).unwrap(), Outcome::UpToDate);

    // Rebasing onto a history that already has our base is a no-op replay.
    let (same, o) = merge::rebase(&s, b3(), &rebased, &public).unwrap();
    assert!(matches!(o, Outcome::Done(_)));
    assert!(same.extends(&rebased));

    // A conflicting private step stops the rebase with the conflict.
    let mut clash = History::default();
    clash.append(&v0, Step::default(), None).unwrap();
    clash.append(&edit(&s, &v0, "g", Some("3\n")), step("g3"), None).unwrap();
    let (_, o) = merge::rebase(&s, b3(), &clash, &next).unwrap();
    assert!(matches!(o, Outcome::Conflicts(_)), "{o:?}");
}

const NAMES: &[&str] = &["a", "b", "c"];

fn edits_strategy() -> impl Strategy<Value = Vec<(String, Option<String>)>> {
    let path = prop::collection::vec(prop::sample::select(NAMES), 1..3).prop_map(|n| n.join("/"));
    let what = prop_oneof![Just(None), Just(Some("/".to_string())), "[xy]".prop_map(|t| Some(format!("{t}\n")))];
    prop::collection::vec((path, what), 0..5)
}

fn apply(s: &Session, v: &Ref, edits: &[(String, Option<String>)]) -> Ref {
    let mut v = v.clone();
    for (path, what) in edits {
        let e = match what.as_deref() {
            None => Edit::Remove,
            Some("/") => Edit::Set(Node::Dir),
            Some(t) => Edit::Set(file(s, t)),
        };
        if let Ok(n) = update(s, b3(), &v, &BTreeMap::from([(p(path), e)])) {
            v = n;
        }
    }
    v
}

proptest! {
    #[test]
    fn merge_properties(base_e in edits_strategy(), oe in edits_strategy(), te in edits_strategy()) {
        let s = store();
        let empty = build(&s, b3(), formats::by_alias("dir").unwrap(), &BTreeMap::new()).unwrap();
        let base = apply(&s, &empty, &base_e);
        let (o, t) = (apply(&s, &base, &oe), apply(&s, &base, &te));
        // Trivial sides.
        prop_assert_eq!(merge_versions(&s, b3(), &base, &o, &base).unwrap().version, o.clone());
        prop_assert_eq!(merge_versions(&s, b3(), &base, &base, &t).unwrap().version, t.clone());
        prop_assert_eq!(merge_versions(&s, b3(), &base, &o, &o).unwrap().version, o.clone());
        // A clean merge is symmetric, and keeps every path neither side touched.
        let m = merge_versions(&s, b3(), &base, &o, &t).unwrap();
        let n = merge_versions(&s, b3(), &base, &t, &o).unwrap();
        prop_assert_eq!(m.conflicts.is_empty(), n.conflicts.is_empty());
        if m.conflicts.is_empty() {
            prop_assert_eq!(&m.version, &n.version);
            for item in walk(&s, &m.version, None) {
                let (path, node) = item.unwrap();
                let (ob, tb) = (lookup(&s, &o, &path).unwrap(), lookup(&s, &t, &path).unwrap());
                prop_assert!(Some(&node) == ob.as_ref() || Some(&node) == tb.as_ref() || matches!(node, Node::File(_)),
                    "{} is in neither side", path);
            }
        }
    }
}
