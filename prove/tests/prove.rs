//! Proofs of manifests: made, recorded, verified from the manifest alone,
//! and refused when anything is not what the manifest says.

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Policy, Session, Store, Who};
use cas_version::commits::{self, Commit};
use cas_version::proofs::{self, MANIFEST, Manifest};
use cas_version::typed::key_bytes;
use cas_version::{Node, Path, View, build, by_alias, derived, put, read_blob, write_blob};
use std::collections::BTreeMap;

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn store() -> Session {
    CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous())
}

/// A commit of a small tree; its key and its version.
fn commit(s: &dyn Store) -> (String, View) {
    let mut files = BTreeMap::new();
    for (path, text) in [("README", "hi\n"), ("src/a.rs", "fn a() {}\n"), ("src/deep/b.rs", "fn b() {}\n")] {
        files.insert(Path::new(path).unwrap(), Node::File(put(s, b3(), "raw", text.as_bytes()).unwrap()));
    }
    // Enough entries that the root directory is more than one BLAKE3 chunk.
    for i in 0..40 {
        let name = format!("a file with a fairly long name number {i:03}.txt");
        files.insert(Path::new(name).unwrap(), Node::File(put(s, b3(), "raw", b"x").unwrap()));
    }
    let v = build(s, b3(), by_alias("directory").unwrap(), &files).unwrap();
    let author = commits::put_text(s, b3(), "author", "Dana Ünïcode").unwrap();
    let c = Commit { version: v.key().to_string(), author, notes: None, time: 7, parents: BTreeMap::new() };
    (commits::write(s, b3(), &c).unwrap(), v)
}

#[test]
fn a_manifest_is_proven_and_verified_from_its_bytes_alone() {
    let s = store();
    let (c, v) = commit(&s);
    assert!(read_blob(&s, v.key()).unwrap().len() > 1024);
    let m = derived::derive(&s, b3(), &MANIFEST, &commits::view("commit", &c)).unwrap();
    assert!(cas_prove::verify(&s, m.key()).is_err(), "nothing is proven yet");
    let size = cas_prove::prove_and_record(&s, m.key()).unwrap();
    assert!(size > 100_000);
    cas_prove::verify(&s, m.key()).unwrap();

    // The verifier needs the manifest and the proof, and no blob.
    let manifest = read_blob(&s, m.key()).unwrap();
    let proof = cas_prove::recorded(&s, m.key()).unwrap().unwrap();
    cas_prove::verify_bytes(&manifest, &proof).unwrap();

    // The proof is of these keys, these types and these lengths only.
    let listed = Manifest::decode(&manifest).unwrap();
    let mut other_key = listed.clone();
    let (k, what) = other_key.blobs.pop_first().unwrap();
    let mut k2 = k;
    k2[31] ^= 1;
    other_key.blobs.insert(k2, what);
    assert!(cas_prove::verify_bytes(&other_key.encode(), &proof).is_err());
    let mut fewer = listed.clone();
    fewer.blobs.pop_last();
    assert!(cas_prove::verify_bytes(&fewer.encode(), &proof).is_err());
    let mut damaged = proof.clone();
    let at = damaged.len() / 2;
    damaged[at] ^= 1;
    assert!(cas_prove::verify_bytes(&manifest, &damaged).is_err());
    assert!(cas_prove::verify_bytes(&manifest, &proof[..proof.len() - 9]).is_err());
}

#[test]
fn what_is_not_valid_gets_no_proof() {
    let s = store();
    let (c, v) = commit(&s);
    let commit_key = key_bytes(&c).unwrap();
    let dir_key = key_bytes(v.key()).unwrap();
    let dir_len = read_blob(&s, v.key()).unwrap().len() as u64;
    let prove = |m: &Manifest| {
        let key = write_blob(&s, b3(), &m.encode()).unwrap();
        cas_prove::prove(&s, &key)
    };
    // A directory listed as a commit: its bytes are not a valid commit.
    let mut m = Manifest::default();
    m.blobs.insert(dir_key, ("commit", dir_len));
    assert!(prove(&m).unwrap_err().contains("is not a valid commit"));
    // The right type with the wrong length.
    let mut m = Manifest::default();
    m.blobs.insert(dir_key, ("directory", dir_len - 64));
    assert!(prove(&m).unwrap_err().contains("the manifest says"));
    // A blob that is not there.
    let mut m = Manifest::default();
    m.blobs.insert([9; 32], ("commit", 128));
    assert!(prove(&m).is_err());
    // Bytes that are almost a directory: two names out of order.
    let good = read_blob(&s, v.key()).unwrap();
    let mut bad = good.clone();
    let name_at = |b: &[u8], block: usize| b[64 * block + 34..64 * block + 40].to_vec();
    assert_ne!(name_at(&bad, 0), name_at(&bad, 2));
    bad[34] = b'z';
    let bad_key = write_blob(&s, b3(), &bad).unwrap();
    let mut m = Manifest::default();
    m.blobs.insert(key_bytes(&bad_key).unwrap(), ("directory", bad.len() as u64));
    assert!(prove(&m).unwrap_err().contains("is not a valid directory"));
    // And the honest manifest of the same store still proves.
    let mut m = Manifest::default();
    m.blobs.insert(commit_key, ("commit", read_blob(&s, &c).unwrap().len() as u64));
    m.blobs.insert(dir_key, ("directory", dir_len));
    let proof = prove(&m).unwrap();
    cas_prove::verify_bytes(&m.encode(), &proof).unwrap();
    assert!(proofs::read(&s, &write_blob(&s, b3(), &m.encode()).unwrap()).is_ok());
}
