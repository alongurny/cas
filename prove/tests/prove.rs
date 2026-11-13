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

/// The circuits' automata accept exactly what cas's own checks accept.
#[test]
fn the_text_automata_agree_with_the_checks() {
    use cas_version::typed::{check_prose, check_semver, check_text};
    use flock_prover::cas_ir::{prose_grammar, semver_grammar, text_grammar};
    let (text, prose, semver) = (text_grammar(), prose_grammar(), semver_grammar());
    // Every string of up to 5 symbols over an alphabet with each kind of
    // byte the grammars tell apart, and longer ones from a fixed generator.
    let alphabet: Vec<&[u8]> = vec![
        b"0",
        b"1",
        b"9",
        b"a",
        b"Z",
        b"-",
        b".",
        b"+",
        b"\n",
        b"\r",
        b"\t",
        b" ",
        b"\x7f",
        "é".as_bytes(),
        "\u{85}".as_bytes(),
        b"\xff",
    ];
    let mut cases: Vec<Vec<u8>> = vec![Vec::new()];
    let mut layer: Vec<Vec<u8>> = vec![Vec::new()];
    for _ in 0..5 {
        layer = layer.iter().flat_map(|p| alphabet.iter().map(move |a| [p.as_slice(), a].concat())).collect();
        cases.extend(layer.iter().cloned());
    }
    let mut x: u64 = 1;
    for _ in 0..200_000 {
        let len = (x % 24) as usize;
        let mut c = Vec::new();
        for _ in 0..len {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            c.extend_from_slice(alphabet[(x >> 33) as usize % 9]);
        }
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        cases.push(c);
    }
    for v in ["1.0.0", "0.0.0", "10.20.30", "1.0.0-alpha.1", "1.0.0-0a.01b", "1.0.0+001", "1.0.0-rc.1+b.2"] {
        cases.push(v.as_bytes().to_vec());
    }
    for c in &cases {
        assert_eq!(text.accepts(c), check_text(c).is_ok(), "text {c:?}");
        assert_eq!(prose.accepts(c), check_prose(c).is_ok(), "prose {c:?}");
        assert_eq!(semver.accepts(c), check_semver(c).is_ok(), "semver {:?}", String::from_utf8_lossy(c));
    }
}

/// A release history is proven: each release with its tag, its changelog
/// and the part of its version the release before it does not have.
#[test]
fn releases_are_proven() {
    use cas_version::releases;
    let s = store();
    let (_, v) = commit(&s);
    let first = releases::publish(&s, b3(), "releases", &v, "1.0.0", None).unwrap();
    let mut files = BTreeMap::new();
    files.insert(Path::new("README").unwrap(), Node::File(put(&s, b3(), "raw", b"bye\n").unwrap()));
    let v2 = build(&s, b3(), by_alias("directory").unwrap(), &files).unwrap();
    let changelog = "Says bye.\n\nThe rest is gone.\n";
    let second = releases::publish(&s, b3(), "releases", &v2, "2.0.0-rc.1", Some(("breaking", changelog))).unwrap();
    let r = releases::read(&s, &second).unwrap();
    let (log, _) = r.from.values().next().unwrap();
    assert_eq!(releases::changelog(&s, log).unwrap(), "Says bye.\n\nThe rest is gone.");

    let m = proofs::of_release(&s, &second).unwrap();
    let aliases: Vec<&str> = m.blobs.values().map(|(a, _)| *a).collect();
    for want in ["release", "tag", "prose", "directory"] {
        assert!(aliases.contains(&want), "{want} in {aliases:?}");
    }
    for k in [&first, &second] {
        let view = derived::derive(&s, b3(), &proofs::RELEASE_MANIFEST, &commits::view("release", k)).unwrap();
        cas_prove::prove_and_record(&s, view.key()).unwrap();
        cas_prove::verify(&s, view.key()).unwrap();
    }
}
