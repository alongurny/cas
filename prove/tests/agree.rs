//! The circuit and the parser are two descriptions of one format: on the
//! same blob they must give the same answer. Here: every byte a name could
//! hold and every kind a directory's child could be.

use cas_version::typed::by_type;
use flock_prover::cas_ir;

/// A directory of one entry: a key, a kind, a name.
fn entry(kind: u8, name: &[u8]) -> Vec<u8> {
    let mut b = vec![7u8; 32];
    b.push(kind);
    b.push(name.len() as u8);
    b.extend_from_slice(name);
    b.resize(b.len().next_multiple_of(64), 0);
    b
}

#[test]
fn the_circuit_and_the_parser_agree_on_directories() {
    let parser = by_type("directory").unwrap();
    let types = cas_ir::types();
    let t = types.iter().find(|t| t.alias == "directory").unwrap();
    let circuits = cas_ir::circuits(t);
    let mut blobs = Vec::new();
    for c in 0..=255u8 {
        // The byte alone, inside a name, and after a dot.
        blobs.push(entry(1, &[c]));
        blobs.push(entry(1, &[b'a', c, b'b']));
        blobs.push(entry(1, &[b'.', c]));
        // A long name, where the byte is in a later block.
        let mut long = vec![b'x'; 70];
        long[50] = c;
        blobs.push(entry(1, &long));
    }
    for kind in 0..=8u8 {
        blobs.push(entry(kind, b"name"));
    }
    let (mut valid, mut refused) = (0, 0);
    for b in &blobs {
        let by_parser = parser.check(b).is_ok();
        assert_eq!(cas_ir::check(t, &circuits, b), by_parser, "{b:?}");
        if by_parser {
            valid += 1;
        } else {
            refused += 1;
        }
    }
    // Letters, digits, `+`, `.`, `-`, `_` and the space, in four places (a dot
    // alone and a dot after a dot are refused), and four kinds.
    let valid_names = (26 + 26 + 10 + 5) * 4 - 2;
    assert_eq!(valid, valid_names + 4);
    assert!(refused > 600);
}

/// The code the circuit gives each key's target is the manifest's tag byte
/// of that type, or 0 for what the key lookup leaves alone: a file, and a
/// parent (a commit's parents, the releases a release follows).
#[test]
fn the_circuit_and_the_parser_agree_on_what_keys_name() {
    use cas_version::typed::PROVEN;
    let code = |alias: &str| match alias {
        "commit" | "release" => 0,
        a => PROVEN.iter().position(|p| *p == a).map_or(0, |i| i as u8 + 1),
    };
    let mut fields = 0;
    for t in cas_ir::types().iter().filter(|t| t.text.is_none()) {
        let parser = by_type(t.alias).unwrap();
        let recs = [
            (t.head.as_ref(), parser.head.as_ref()),
            (t.list.as_ref().map(|l| &l.0), parser.list.as_ref().map(|l| &l.0)),
        ];
        for (circuit, parsed) in recs {
            let (Some(c), Some(p)) = (circuit, parsed) else {
                assert!(circuit.is_none() && parsed.is_none(), "{}", t.alias);
                continue;
            };
            assert_eq!(c.keys.len(), p.keys.len(), "{}", t.alias);
            for (ck, pk) in c.keys.iter().zip(&p.keys) {
                let want: Vec<u8> = pk.targets.iter().map(|a| code(a)).collect();
                assert_eq!(ck.targets, &want[..], "{}.{}", t.alias, pk.name);
                assert_eq!(ck.optional, pk.optional, "{}.{}", t.alias, pk.name);
                fields += 1;
            }
        }
    }
    assert_eq!(fields, 16);
}
