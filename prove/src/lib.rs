//! Proofs that the blobs of a manifest are valid (cas spec/functions/proof.md),
//! made and checked with Flock. The circuit is built from the manifest's
//! records alone: the type, length and role of each blob, in the manifest's
//! order.

use cas_core::Store;
use cas_version::proofs::{self, Manifest, PROOF};
use cas_version::typed::{PROVEN, key_text};
use cas_version::{View, derived, read_blob};
use flock_prover::cas_ir::{self, Type};
use flock_prover::cas_typed::{Blob, Role, TypedCircuit, TypedProof};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// The circuit's type for a cas format: the texts share one.
fn circuit_type<'a>(types: &'a [Type], alias: &str) -> Result<&'a Type, String> {
    let name = if alias == "note" || alias == "author" { "text" } else { alias };
    types.iter().find(|t| t.alias == name).ok_or(format!("no circuit for the type {alias}"))
}

/// The circuit of a manifest. `native` also builds what proving needs.
fn circuit(types: &[Type], m: &Manifest, native: bool) -> Result<TypedCircuit, String> {
    let mut blobs = Vec::new();
    for (key, alias, len, role) in m.records() {
        let t = circuit_type(types, alias)?;
        // A length no blob of the type can have gets no circuit at all.
        if cas_ir::plan(t, len as usize).is_none() {
            return Err(format!("{} cannot be a {alias}: {len} bytes", key_text(&key)));
        }
        let role = [Role::New, Role::Opened, Role::Parent][role as usize];
        let code = PROVEN.iter().position(|a| *a == alias).expect("a type a proof covers") as u8 + 1;
        blobs.push(Blob { t, len: len as usize, role, code });
    }
    catch_unwind(AssertUnwindSafe(|| TypedCircuit::with_lookup(&blobs, native)))
        .map_err(|_| "the circuit could not be built".into())
}

fn encode(p: &TypedProof) -> Result<Vec<u8>, String> {
    bincode::serialize(&(&p.proof, &p.commitment)).map_err(|e| e.to_string())
}

fn decode(b: &[u8]) -> Result<TypedProof, String> {
    let (proof, commitment) = bincode::deserialize(b).map_err(|e| format!("not a proof: {e}"))?;
    Ok(TypedProof { proof, commitment })
}

/// Prove the manifest `key` from the blobs in `store`; the proof's bytes.
/// A blob that is not valid, or is not what the manifest says, is named.
pub fn prove(store: &dyn Store, key: &str) -> Result<Vec<u8>, String> {
    let m = proofs::read(store, key).map_err(|e| e.to_string())?;
    let types = cas_ir::types();
    let (mut keys, mut blobs) = (Vec::new(), Vec::new());
    for (k, alias, len, _) in m.records() {
        let text = key_text(&k);
        let b = read_blob(store, &text).map_err(|e| e.to_string())?;
        if b.len() as u64 != len {
            return Err(format!("{text} has {} bytes, the manifest says {len}", b.len()));
        }
        let t = circuit_type(&types, alias)?;
        if !cas_ir::check(t, &cas_ir::circuits(t), &b) {
            return Err(format!("{text} is not a valid {alias}"));
        }
        keys.push(k);
        blobs.push(b);
    }
    let c = circuit(&types, &m, true)?;
    let proof = catch_unwind(AssertUnwindSafe(|| c.prove(&c.witness(&keys, &blobs)))).map_err(|_| {
        "the prover stopped: a blob does not satisfy its circuit, or holds a key that is neither brought nor in a parent"
            .to_string()
    })?;
    // A proof that would not verify is never handed out.
    c.verify(&keys, &proof).map_err(|e| format!("the proof made does not verify: {e:?}"))?;
    encode(&proof)
}

/// Check `proof` against the manifest's bytes alone: no blob is read. What
/// it shows holds given that the manifest's parents are valid, which their
/// own proofs show.
pub fn verify_bytes(manifest: &[u8], proof: &[u8]) -> Result<(), String> {
    let m = Manifest::decode(manifest)?;
    let types = cas_ir::types();
    let c = circuit(&types, &m, false)?;
    let keys: Vec<[u8; 32]> = m.records().iter().map(|r| r.0).collect();
    let proof = decode(proof)?;
    match catch_unwind(AssertUnwindSafe(|| c.verify(&keys, &proof))) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(format!("the proof does not verify: {e:?}")),
        Err(_) => Err("the proof does not verify".into()),
    }
}

fn manifest_view(key: &str) -> View {
    cas_version::commits::view("manifest", key)
}

/// The recorded proof of the manifest `key`, if the store has one.
pub fn recorded(store: &dyn Store, key: &str) -> Result<Option<Vec<u8>>, String> {
    match derived::cached(store, &PROOF, &manifest_view(key)).map_err(|e| e.to_string())? {
        Some(v) => Ok(Some(read_blob(store, v.key()).map_err(|e| e.to_string())?)),
        None => Ok(None),
    }
}

/// Prove the manifest `key` and record the proof as derived data.
pub fn prove_and_record(store: &dyn Store, key: &str) -> Result<usize, String> {
    let bytes = prove(store, key)?;
    let b3 = cas_core::algo::lookup("blake3").unwrap();
    derived::publish(store, &PROOF).map_err(|e| e.to_string())?;
    derived::record(store, b3, &PROOF, &manifest_view(key), &bytes);
    match recorded(store, key)? {
        Some(_) => Ok(bytes.len()),
        None => Err("the store did not take the proof".into()),
    }
}

/// Check the recorded proof of the manifest `key`.
pub fn verify(store: &dyn Store, key: &str) -> Result<(), String> {
    let manifest = read_blob(store, key).map_err(|e| e.to_string())?;
    let proof = recorded(store, key)?.ok_or(format!("no proof of {key}"))?;
    verify_bytes(&manifest, &proof)
}
