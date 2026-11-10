//! The stored tree of a history (spec/formats/history-part.md).

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Expect, Policy, Session, Store, Who};
use cas_version::history::{self, Entry, History, NodeId, Part, Root, Step, Stored};
use cas_version::{View, put};
use proptest::prelude::*;
use std::collections::BTreeMap;

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn store() -> Session {
    CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous())
}

fn step(i: usize) -> Step {
    Step { changelog: Some(format!("step {i}")), compatible: i.is_multiple_of(3), notes: None }
}

/// A linear history of `n` raw versions, built in memory.
fn linear(s: &Session, n: usize) -> History {
    let mut h = History { doc: Some("demo".into()), ..History::default() };
    for i in 0..n {
        let v = put(s, b3(), "raw", format!("v{i}").as_bytes()).unwrap();
        h.append(&v, step(i), None).unwrap();
    }
    h
}

/// Every part under `v`, with its level.
fn parts(s: &Session, v: &View, out: &mut Vec<(u32, Part)>) {
    let p = Part::decode(&s.read(v.key()).unwrap().unwrap()).unwrap();
    if let Part::Inner { level, parts: kids } = &p {
        for k in kids.values() {
            parts(s, k, out);
        }
        out.push((*level, p));
    } else {
        out.push((0, p));
    }
}

fn top(s: &Session, r: &View) -> Option<View> {
    Root::decode(&s.read(r.key()).unwrap().unwrap()).unwrap().nodes
}

#[test]
fn commits_one_by_one_give_the_tree_of_the_whole_history() {
    let s = store();
    let h = linear(&s, 5000);
    let whole = history::write(&s, b3(), &h).unwrap();
    for id in h.log().into_iter().rev() {
        let n = &h.nodes[&id];
        let st = step_of(n);
        history::commit(&s, b3(), "main", &id.version(), st, None).unwrap();
    }
    // The doc differs (commit writes none); set it and compare.
    let (old, mut built) = history::load(&s, "main").unwrap();
    assert_eq!(built.nodes, h.nodes);
    built.doc = h.doc.clone();
    assert_eq!(history::save(&s, b3(), "main", old.as_ref(), &built).unwrap(), whole);
    // 5000 nodes: leaves, inner parts at level 1, and a top at level 2.
    let mut all = Vec::new();
    parts(&s, &top(&s, &whole).unwrap(), &mut all);
    let leaves = all.iter().filter(|(l, _)| *l == 0).count();
    assert!((40..140).contains(&leaves), "{leaves} leaves");
    assert_eq!(all.last().unwrap().0, 2);
}

fn step_of(n: &history::HistNode) -> Step {
    n.after.values().next().cloned().unwrap_or_default()
}

/// A commit reads and writes a few parts, not the history.
#[test]
fn a_commit_touches_a_few_parts() {
    let s = store();
    let h = linear(&s, 3000);
    let r = history::write(&s, b3(), &h).unwrap();
    s.write("main", r.to_string().as_bytes(), &Expect::Absent).unwrap();
    let before: usize = count(&s);
    let v = put(&s, b3(), "raw", b"new").unwrap();
    history::commit(&s, b3(), "main", &v, Step::default(), None).unwrap();
    // The blob, a leaf or two, one or two inner parts, the root.
    let written = count(&s) - before;
    assert!(written <= 6, "{written} blobs written");
}

fn count(s: &Session) -> usize {
    cas_core::list_all(s, &cas_core::Span::all()).unwrap().len()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// Any sequence of updates (adds, changes, removals) gives the tree a
    /// whole write of the result gives, and reads back as that history.
    #[test]
    fn updates_agree_with_a_whole_write(
        n in 0usize..400,
        edits in proptest::collection::vec((0usize..600, 0u8..3), 0..60),
    ) {
        let s = store();
        let mut h = History::default();
        let ids: Vec<NodeId> = (0..600)
            .map(|i| NodeId::of(&put(&s, b3(), "raw", format!("v{i}").as_bytes()).unwrap()))
            .collect();
        for id in ids.iter().take(n) {
            h.nodes.insert(id.clone(), Default::default());
        }
        let mut r = history::write(&s, b3(), &h).unwrap();
        for (i, op) in edits {
            let id = &ids[i];
            let change = match op {
                0 => None,
                1 => Some(Default::default()),
                _ => Some(history::HistNode { doc: Some(format!("doc {i}")), ..Default::default() }),
            };
            match &change {
                None => h.nodes.remove(id),
                Some(c) => h.nodes.insert(id.clone(), c.clone()),
            };
            let st = Stored::open(&s, Some(&r)).unwrap();
            let root = Root { tips: h.tips(), ..Root::default() };
            r = st.update(b3(), root, &BTreeMap::from([(id.clone(), change)])).unwrap();
            prop_assert_eq!(&r, &history::write(&s, b3(), &h).unwrap());
        }
        prop_assert_eq!(history::read(&s, &r).unwrap(), h);
    }
}

#[test]
fn changed_reads_only_the_differing_leaves() {
    let s = store();
    let mut h = linear(&s, 2000);
    let a = history::write(&s, b3(), &h).unwrap();
    let v = put(&s, b3(), "raw", b"extra").unwrap();
    let id = h.append(&v, Step::default(), None).unwrap();
    let b = history::write(&s, b3(), &h).unwrap();
    let (old, new) = (Stored::open(&s, Some(&a)).unwrap(), Stored::open(&s, Some(&b)).unwrap());
    let c = old.changed(&new).unwrap();
    // The old tip gained no step, so only the new node differs.
    assert!(c.old.is_empty(), "{:?}", c.old);
    assert_eq!(c.added.keys().collect::<Vec<_>>(), vec![&id]);
    assert!(!c.same.is_empty() && c.same.len() < 200);
}

#[test]
fn a_damaged_tree_is_refused_on_a_whole_read() {
    let s = store();
    let h = linear(&s, 500);
    let r = history::write(&s, b3(), &h).unwrap();
    let root = Root::decode(&s.read(r.key()).unwrap().unwrap()).unwrap();
    let write_root = |root: &Root| {
        let bytes = root.encode();
        let key = b3().key(&bytes);
        s.write(&key, &bytes, &Expect::Absent).unwrap();
        View::new(r.format(), &key).unwrap()
    };
    // Wrong tips.
    let bad = write_root(&Root { tips: [h.log().last().unwrap().clone()].into(), ..root.clone() });
    assert!(history::read(&s, &bad).is_err());
    // All the nodes in one leaf: not chunked by id.
    let g = history::generations(&h);
    let one =
        Part::Leaf(h.nodes.iter().map(|(k, n)| (k.clone(), Entry { node: n.clone(), generation: g[k] })).collect());
    let bytes = one.encode();
    let key = b3().key(&bytes);
    s.write(&key, &bytes, &Expect::Absent).unwrap();
    let leaf = View::new(root.nodes.as_ref().unwrap().format(), &key).unwrap();
    let bad = write_root(&Root { nodes: Some(leaf), ..root.clone() });
    assert!(history::read(&s, &bad).is_err());
    // The good one still reads.
    assert_eq!(history::read(&s, &write_root(&root)).unwrap(), h);
}

/// A random DAG: node i follows up to two earlier nodes.
fn dag(s: &Session, edges: &[(usize, usize)]) -> (History, Vec<NodeId>) {
    let mut h = History::default();
    let mut ids: Vec<NodeId> = Vec::new();
    for (i, &(a, b)) in edges.iter().enumerate() {
        let v = put(s, b3(), "raw", format!("d{i}").as_bytes()).unwrap();
        let mut after = BTreeMap::new();
        if i > 0 {
            // Changelogs on some steps, compatible on others (for read_log's notes).
            let st = |k: usize| Step {
                changelog: k.is_multiple_of(2).then(|| format!("s{i}.{k}")),
                compatible: k.is_multiple_of(3),
                notes: None,
            };
            after.insert(ids[a % i].clone(), st(a));
            after.insert(ids[b % i].clone(), st(b));
        }
        ids.push(h.add(&v, after, None).unwrap());
    }
    (h, ids)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// The walk by generation over the stored tree finds the merge bases
    /// the whole history does.
    #[test]
    fn stored_merge_bases_agree(
        edges in proptest::collection::vec((0usize..1000, 0usize..1000), 1..120),
        x in 0usize..1000,
        y in 0usize..1000,
    ) {
        let s = store();
        let (h, ids) = dag(&s, &edges);
        let r = history::write(&s, b3(), &h).unwrap();
        let st = Stored::open(&s, Some(&r)).unwrap();
        let (a, b) = (&ids[x % ids.len()], &ids[y % ids.len()]);
        prop_assert_eq!(st.merge_bases(a, b).unwrap(), h.merge_bases(a, b));
        prop_assert_eq!(history::read(&s, &r).unwrap(), h);
    }

    /// read_log gives the log, predecessors by position and notes the
    /// whole history gives.
    #[test]
    fn read_log_agrees_with_the_whole_history(
        edges in proptest::collection::vec((0usize..1000, 0usize..1000), 1..120),
    ) {
        let s = store();
        let (h, _) = dag(&s, &edges);
        let r = history::write(&s, b3(), &h).unwrap();
        let got = Stored::open(&s, Some(&r)).unwrap().read_log().unwrap();
        let log = h.log();
        let pos: BTreeMap<&NodeId, usize> = log.iter().enumerate().map(|(i, id)| (id, i)).collect();
        prop_assert_eq!(got.len(), log.len());
        for (rec, id) in got.iter().zip(&log) {
            prop_assert_eq!(&rec.id, id);
            let n = &h.nodes[id];
            let after: Vec<(usize, bool)> = n.after.iter().map(|(p, st)| (pos[p], st.compatible)).collect();
            prop_assert_eq!(&rec.after, &after);
            let notes: Vec<&str> = n.after.values().filter_map(|st| st.changelog.as_deref()).collect();
            prop_assert_eq!(&rec.notes, &notes.join(" / "));
        }
    }
}

#[test]
fn a_wrong_generation_is_refused() {
    let s = store();
    let h = linear(&s, 10);
    let r = history::write(&s, b3(), &h).unwrap();
    let root = Root::decode(&s.read(r.key()).unwrap().unwrap()).unwrap();
    let top = root.nodes.clone().unwrap();
    let Part::Leaf(mut nodes) = Part::decode(&s.read(top.key()).unwrap().unwrap()).unwrap() else { panic!() };
    nodes.values_mut().next().unwrap().generation += 1;
    let bytes = Part::Leaf(nodes).encode();
    let key = b3().key(&bytes);
    s.write(&key, &bytes, &Expect::Absent).unwrap();
    let bytes = Root { nodes: Some(View::new(top.format(), &key).unwrap()), ..root }.encode();
    let rk = b3().key(&bytes);
    s.write(&rk, &bytes, &Expect::Absent).unwrap();
    let bad = View::new(r.format(), &rk).unwrap();
    let err = history::read(&s, &bad).unwrap_err().to_string();
    assert!(err.contains("generation"), "{err}");
    let err = Stored::open(&s, Some(&bad)).unwrap().read_log().err().unwrap().to_string();
    assert!(err.contains("generation"), "{err}");
}
