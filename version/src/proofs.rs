//! What a proof of validity covers (spec/formats/manifest.md), and the
//! manifest of a commit (spec/functions/manifest.md). The prover itself is
//! not in this crate: it takes a manifest and the blobs it lists, and its
//! output is kept as the derived result [`PROOF`].

use crate::derived::Function;
use crate::typed::{NONE, PROVEN, Rec, RecV, Type, Value, by_type, key_bytes, key_text};
use crate::{Error, Result, View, malformed, read_blob};
use cas_core::Store;
use cas_core::algo::Algorithm;
use std::collections::BTreeMap;

/// The blobs a proof covers: for each key, its type and its length in bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    pub blobs: BTreeMap<[u8; 32], (&'static str, u64)>,
}

impl Manifest {
    pub fn encode(&self) -> Vec<u8> {
        let list = self
            .blobs
            .iter()
            .map(|(k, (alias, len))| {
                let tag = PROVEN.iter().position(|a| a == alias).expect("a type a proof can cover") as u8 + 1;
                RecV { keys: vec![*k], kinds: vec![tag], ints: vec![*len], ..Default::default() }
            })
            .collect();
        by_type("manifest").unwrap().print(&Value { head: None, list })
    }

    pub fn decode(b: &[u8]) -> std::result::Result<Manifest, String> {
        let t = by_type("manifest").unwrap();
        let v = t.parse(b)?;
        if t.print(&v) != b {
            return Err("not the one encoding of its value".into());
        }
        Ok(Manifest {
            blobs: v.list.iter().map(|r| (r.keys[0], (PROVEN[r.kinds[0] as usize - 1], r.ints[0]))).collect(),
        })
    }

    /// The bytes all the listed blobs have together.
    pub fn bytes(&self) -> u64 {
        self.blobs.values().map(|(_, len)| len).sum()
    }
}

pub fn read(store: &dyn Store, key: &str) -> Result<Manifest> {
    Manifest::decode(&read_blob(store, key)?).map_err(|e| malformed(key, e))
}

struct Walk<'a> {
    store: &'a dyn Store,
    out: Manifest,
    /// Counterparts already parsed, by key.
    parsed: BTreeMap<[u8; 32], Option<Value>>,
}

/// The target of the `i`th key of `r` in the record value `v`.
fn target(r: &Rec, v: &RecV, i: usize) -> &'static str {
    let k = &r.keys[i];
    if k.targets.len() == 1 {
        return k.targets[0];
    }
    // Tags are in key order, one for each key with several targets.
    let nth = r.keys[..i].iter().filter(|k| k.targets.len() > 1).count();
    k.targets[v.kinds[nth] as usize - 1]
}

impl Walk<'_> {
    fn value(&mut self, t: &Type, key: &[u8; 32]) -> Result<&Option<Value>> {
        if !self.parsed.contains_key(key) {
            let text = key_text(key);
            let bytes = read_blob(self.store, &text)?;
            let value = Some(t.parse(&bytes).map_err(|e| malformed(&text, e))?);
            self.parsed.insert(*key, value);
        }
        Ok(&self.parsed[key])
    }

    /// List `key`, read as `alias`, unless one of `others` (what the
    /// parents have in this place) is the same blob; then go down.
    fn visit(&mut self, alias: &'static str, key: [u8; 32], others: &[[u8; 32]]) -> Result<()> {
        if others.contains(&key) || self.out.blobs.contains_key(&key) {
            return Ok(());
        }
        let text = key_text(&key);
        let bytes = read_blob(self.store, &text)?;
        self.out.blobs.insert(key, (alias, bytes.len() as u64));
        // A text holds no keys.
        let Some(t) = by_type(alias) else { return Ok(()) };
        let v = t.parse(&bytes).map_err(|e| malformed(&text, e))?;
        let mut theirs: Vec<Value> = Vec::new();
        for o in others {
            if let Some(ov) = self.value(t, o)?.clone() {
                theirs.push(ov);
            }
        }
        if let (Some(r), Some(hv)) = (&t.head, &v.head) {
            let heads: Vec<&RecV> = theirs.iter().filter_map(|o| o.head.as_ref()).collect();
            self.fields(r, hv, &heads)?;
        }
        if let Some((r, _)) = &t.list {
            for e in &v.list {
                let same: Vec<&RecV> =
                    theirs.iter().flat_map(|o| o.list.iter()).filter(|x| !r.name || x.name == e.name).collect();
                self.fields(r, e, &same)?;
            }
        }
        Ok(())
    }

    /// Visit each key of the record `v`, against the same field of `others`.
    fn fields(&mut self, r: &Rec, v: &RecV, others: &[&RecV]) -> Result<()> {
        for (i, key) in v.keys.iter().enumerate() {
            let alias = target(r, v, i);
            // A file is never listed; a parent commit has a manifest of its own.
            if *key == NONE || !PROVEN.contains(&alias) || alias == "commit" || alias == "release" {
                continue;
            }
            let there: Vec<[u8; 32]> =
                others.iter().filter(|o| target(r, o, i) == alias && o.keys[i] != NONE).map(|o| o.keys[i]).collect();
            self.visit(alias, *key, &there)?;
        }
        Ok(())
    }
}

/// The manifest of the commit `key`: what it brings that its parents do not
/// have in the same place.
pub fn of_commit(store: &dyn Store, key: &str) -> Result<Manifest> {
    crate::commits::read(store, key)?;
    of_node(store, "commit", key)
}

/// The manifest of the release `key` (spec/functions/release-manifest.md):
/// what it brings that the releases it follows do not have in the same
/// place.
pub fn of_release(store: &dyn Store, key: &str) -> Result<Manifest> {
    crate::releases::read(store, key)?;
    of_node(store, "release", key)
}

/// The manifest of a commit or a release: a head compared with the heads of
/// the nodes its list names first (its parents), and a list whose other
/// keys are its own.
fn of_node(store: &dyn Store, alias: &'static str, key: &str) -> Result<Manifest> {
    let k = key_bytes(key).ok_or_else(|| Error::BadKey(key.to_string()))?;
    let t = by_type(alias).unwrap();
    let mut w = Walk { store, out: Manifest::default(), parsed: BTreeMap::new() };
    let bytes = read_blob(store, key)?;
    w.out.blobs.insert(k, (alias, bytes.len() as u64));
    let v = t.parse(&bytes).map_err(|e| malformed(key, e))?;
    // The node's fields are compared with its parents' fields.
    let mut heads = Vec::new();
    for e in &v.list {
        if let Some(pv) = w.value(t, &e.keys[0])?.clone() {
            heads.extend(pv.head);
        }
    }
    let r = t.head.as_ref().unwrap();
    w.fields(r, v.head.as_ref().unwrap(), &heads.iter().collect::<Vec<_>>())?;
    // The notes of each step, or each changelog, are this node's own.
    let (lr, _) = t.list.as_ref().unwrap();
    for e in &v.list {
        w.fields(lr, e, &[])?;
    }
    Ok(w.out)
}

fn manifest(store: &dyn Store, _: &'static Algorithm, input: &View) -> Result<Vec<u8>> {
    Ok(of_commit(store, input.key())?.encode())
}

fn release_manifest(store: &dyn Store, _: &'static Algorithm, input: &View) -> Result<Vec<u8>> {
    Ok(of_release(store, input.key())?.encode())
}

fn no_prover(_: &dyn Store, _: &'static Algorithm, _: &View) -> Result<Vec<u8>> {
    Err(Error::Io("this build has no prover: a proof is made by cas prove".into()))
}

/// What a commit brings, as a manifest.
pub static MANIFEST: Function = Function {
    name: "manifest",
    input: "commit",
    output: "manifest",
    spec: include_str!("../../spec/functions/manifest.md"),
    compute: manifest,
};

/// What a release brings, as a manifest.
pub static RELEASE_MANIFEST: Function = Function {
    name: "release-manifest",
    input: "release",
    output: "manifest",
    spec: include_str!("../../spec/functions/release-manifest.md"),
    compute: release_manifest,
};

/// A proof that the blobs of a manifest are valid. Made by the prover and
/// recorded with [`crate::derived::record`]; read with
/// [`crate::derived::cached`].
pub static PROOF: Function = Function {
    name: "proof",
    input: "manifest",
    output: "raw",
    spec: include_str!("../../spec/functions/proof.md"),
    compute: no_prover,
};

/// Copy the proof of `manifest` from `other` into `store`, where the same
/// function names it. False when `other` has none (or does not let it be
/// read). The proof is not checked here: checking it is the verifier's part.
pub fn fetch(store: &dyn Store, alg: &'static Algorithm, other: &dyn Store, manifest: &View) -> Result<bool> {
    let Some(proof) = crate::derived::readable_cache(other, &PROOF, manifest)? else { return Ok(false) };
    let bytes = read_blob(other, proof.key())?;
    crate::derived::record(store, alg, &PROOF, manifest, &bytes);
    Ok(crate::derived::cached(store, &PROOF, manifest)?.is_some())
}
