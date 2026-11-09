//! The ancestry blob (spec/functions/ancestry.md): a history's nodes with
//! generations and predecessors, for merge-base without the leaves.

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Policy, Session, Who};
use cas_version::ancestry::{self, Ancestry};
use cas_version::history::{self, History, NodeId, Step, Stored};
use cas_version::{View, build, formats, put};
use proptest::prelude::*;
use std::collections::BTreeMap;

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn store() -> Session {
    CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous())
}

fn version(s: &Session, i: usize) -> View {
    let f = put(s, b3(), "raw", format!("{i}").as_bytes()).unwrap();
    let m = BTreeMap::from([(cas_version::Path::new("f").unwrap(), cas_version::Node::File(f))]);
    build(s, b3(), formats::by_alias("tree").unwrap(), &m).unwrap()
}

/// A history of `n` nodes, each after one to three earlier ones (picked by
/// `picks`); some versions occur twice, so some ids carry `#2`.
fn dag(s: &Session, n: usize, picks: &[(usize, usize, usize)]) -> (History, Vec<NodeId>) {
    let mut h = History::default();
    let mut ids: Vec<NodeId> = Vec::new();
    for i in 0..n {
        let v = version(s, if i % 7 == 6 { i - 3 } else { i });
        let after: BTreeMap<NodeId, Step> = if ids.is_empty() {
            BTreeMap::new()
        } else {
            let (a, b, c) = picks[i % picks.len()];
            let mut m = BTreeMap::new();
            m.insert(ids[ids.len() - 1 - a % ids.len()].clone(), Step::default());
            if b % 3 == 0 {
                m.insert(ids[b % ids.len()].clone(), Step::default());
            }
            if c % 5 == 0 {
                m.insert(ids[c % ids.len()].clone(), Step::default());
            }
            m
        };
        ids.push(h.add(&v, after, None).unwrap());
    }
    (h, ids)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// The blob holds every node with its generation, and merge bases from
    /// it alone are the history's.
    #[test]
    fn merge_bases_from_the_blob_are_the_historys(
        n in 1usize..80,
        picks in proptest::collection::vec((0usize..6, 0usize..100, 0usize..100), 1..20),
        pairs in proptest::collection::vec((0usize..80, 0usize..80), 1..8),
    ) {
        let s = store();
        let (h, ids) = dag(&s, n, &picks);
        let a = Ancestry::decode(ancestry::encode(&h).unwrap()).unwrap();
        prop_assert_eq!(a.len(), h.nodes.len());
        let gens = history::generations(&h);
        for (id, g) in ancestry::generations(&a) {
            prop_assert_eq!(u64::from(g), gens[&id]);
        }
        for (x, y) in pairs {
            let (x, y) = (&ids[x % n], &ids[y % n]);
            let want: Vec<NodeId> = h.merge_bases(x, y).into_iter().collect();
            let mut got = ancestry::merge_bases_in(&a, x, y).unwrap();
            got.sort();
            prop_assert_eq!(got, want);
        }
    }

    /// Any byte changed is refused, or decodes to an index whose nodes and
    /// generations still fit together (the bytes are checked, not trusted).
    #[test]
    fn a_damaged_blob_is_refused_or_consistent(at in 0usize..4000, byte in 0u8..=255) {
        let s = store();
        let (h, _) = dag(&s, 30, &[(0, 3, 5), (2, 6, 1), (1, 9, 10)]);
        let mut b = ancestry::encode(&h).unwrap();
        let at = at % b.len();
        b[at] = byte;
        if let Ok(a) = Ancestry::decode(b) {
            for i in 0..a.len() {
                let want = a.after(i).map(|p| a.generation(p) + 1).max().unwrap_or(1);
                prop_assert_eq!(a.generation(i), want);
                prop_assert_eq!(a.find(&a.id(i)), Some(i));
            }
        }
    }
}

#[test]
fn ids_are_found_by_key_prefix() {
    let s = store();
    let (h, ids) = dag(&s, 40, &[(0, 1, 1)]);
    let a = Ancestry::decode(ancestry::encode(&h).unwrap()).unwrap();
    let tree = formats::by_alias("tree").unwrap().id();
    for id in &ids {
        let key = id.version().key().to_string();
        for len in [8, 9, 20, key.len()] {
            let got = a.with_key_prefix(tree, &key[..len], 10).unwrap();
            let st_want: Vec<NodeId> = h
                .nodes
                .keys()
                .filter(|n| n.as_str().starts_with(&format!("{tree} {}", &key[..len])))
                .take(10)
                .cloned()
                .collect();
            assert_eq!(got, st_want);
        }
        assert_eq!(a.find(id).map(|i| a.id(i)), Some(id.clone()));
    }
    assert_eq!(a.with_key_prefix("blake3:00", "blake3:ab", 2), Some(vec![]));
    assert!(a.with_key_prefix(tree, "nocolon", 2).is_none());
}

/// An ancestry kept for an older history serves a newer one: nodes added
/// or changed since are read from the history instead.
#[test]
fn an_older_historys_ancestry_serves_with_the_changes_read() {
    let s = store();
    let (mut h, ids) = dag(&s, 50, &[(0, 3, 5), (1, 4, 7)]);
    let old = history::save(&s, b3(), "main", None, &h).unwrap();
    ancestry::record(&s, b3(), &old, &h);
    // Two new nodes, one after a merge of far-apart ones.
    let x = h.add(&version(&s, 1000), BTreeMap::from([(ids[49].clone(), Step::default())]), None).unwrap();
    let y = h
        .add(
            &version(&s, 1001),
            BTreeMap::from([(ids[10].clone(), Step::default()), (ids[30].clone(), Step::default())]),
            None,
        )
        .unwrap();
    let new = history::save(&s, b3(), "main", Some(&old), &h).unwrap();
    let st = Stored::open(&s, Some(&new)).unwrap();
    let usable = ancestry::cached(&s, &st, &new, 8).expect("the old history's ancestry");
    assert!(usable.stale.contains(&x) && usable.stale.contains(&y));
    for (a, b) in [(&x, &y), (&x, &ids[5]), (&y, &ids[40]), (&ids[20], &ids[45])] {
        let want: Vec<NodeId> = h.merge_bases(a, b).into_iter().collect();
        let mut got = ancestry::merge_bases(&st, Some(&usable), a, b).unwrap();
        got.sort();
        assert_eq!(got, want);
    }
    assert!(ancestry::exact(&s, &new).is_none());
}
