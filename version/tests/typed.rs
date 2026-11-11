//! Formats given as types (spec/types.md): each blob has one encoding, a
//! damaged blob is refused, and the views a blob holds follow from its type.

use cas_version::typed::{self, NONE, RecV, Value, by_type, key_text};
use cas_version::{by_alias, by_id};

fn k(n: u8) -> [u8; 32] {
    let mut k = [n; 32];
    k[0] = n.wrapping_add(1);
    k
}

fn entry(key: [u8; 32], kind: u8, name: &str) -> RecV {
    RecV { keys: vec![key], kinds: vec![kind], name: name.as_bytes().to_vec(), ..Default::default() }
}

fn samples() -> Vec<(&'static str, Value)> {
    let long = "c-name-that-is-longer-than-thirty-bytes-and-runs-into-the-next-block.tex";
    vec![
        ("directory", Value::default()),
        (
            "directory",
            Value {
                head: None,
                list: vec![entry(k(1), 1, "a"), entry(k(2), 3, "b.d"), entry(k(3), 2, long), entry(k(4), 1, "z")],
            },
        ),
        (
            "commit",
            Value {
                head: Some(RecV { keys: vec![k(1), k(2), NONE], ints: vec![1_762_880_000], ..Default::default() }),
                list: vec![],
            },
        ),
        (
            "commit",
            Value {
                head: Some(RecV { keys: vec![k(1), k(2), k(3)], ints: vec![1_762_880_000], ..Default::default() }),
                list: vec![
                    RecV { keys: vec![k(4), k(5)], ..Default::default() },
                    RecV { keys: vec![k(6), NONE], ..Default::default() },
                ],
            },
        ),
        (
            "annotation",
            Value {
                head: Some(RecV { keys: vec![NONE], ..Default::default() }),
                list: vec![entry(k(1), 2, "main.tex"), entry(k(2), 1, "src")],
            },
        ),
        ("lines", Value { head: Some(RecV { keys: vec![k(1), NONE, k(2)], ..Default::default() }), list: vec![] }),
        (
            "runs",
            Value {
                head: None,
                list: vec![
                    RecV { keys: vec![NONE], ints: vec![12], ..Default::default() },
                    RecV { keys: vec![k(1)], ints: vec![3], ..Default::default() },
                    RecV { keys: vec![NONE], ints: vec![40], ..Default::default() },
                ],
            },
        ),
        (
            "release",
            Value {
                head: Some(RecV { keys: vec![k(1), k(2)], ..Default::default() }),
                list: vec![
                    RecV { keys: vec![k(3), k(4)], kinds: vec![1], ..Default::default() },
                    RecV { keys: vec![k(5), k(6)], kinds: vec![4], ..Default::default() },
                ],
            },
        ),
    ]
}

#[test]
fn a_value_has_one_encoding() {
    for (alias, v) in samples() {
        let t = by_type(alias).unwrap();
        let b = t.print(&v);
        assert_eq!(b.len() % 64, 0, "{alias}");
        assert_eq!(t.parse(&b).unwrap(), v, "{alias}");
        t.check(&b).unwrap();
        by_alias(alias).unwrap().check("blake3:00", &b).unwrap();
    }
}

#[test]
fn sizes_are_as_the_specs_say() {
    let s = samples();
    assert_eq!(by_type("directory").unwrap().print(&s[1].1).len(), 64 * 5);
    assert_eq!(by_type("commit").unwrap().print(&s[2].1).len(), 64 * 2);
    assert_eq!(by_type("commit").unwrap().print(&s[3].1).len(), 64 * 4);
    assert_eq!(by_type("lines").unwrap().print(&s[5].1).len(), 64 * 2);
    assert_eq!(by_type("runs").unwrap().print(&s[6].1).len(), 64 * 3);
    assert_eq!(by_type("release").unwrap().print(&s[7].1).len(), 64 * 5);
}

/// Every change of one byte, every dropped, doubled or swapped block and
/// every cut either leaves a blob that is valid as some other value, or is
/// refused; nothing is accepted that does not print back to itself.
#[test]
fn a_damaged_blob_is_refused_or_is_another_value() {
    for (alias, v) in samples() {
        let t = by_type(alias).unwrap();
        let good = t.print(&v);
        let mut damaged: Vec<Vec<u8>> = Vec::new();
        for i in 0..good.len() {
            for x in [0u8, 1, 2, 5, b'/', b'.', 0x1f, 0xff] {
                let mut m = good.clone();
                m[i] = x;
                damaged.push(m);
            }
        }
        for b in 0..good.len() / 64 {
            let mut m = good.clone();
            m.drain(b * 64..b * 64 + 64);
            damaged.push(m);
            let mut m = good.clone();
            m.extend_from_slice(&good[b * 64..b * 64 + 64]);
            damaged.push(m);
            let mut m = good.clone();
            m.truncate(b * 64 + 17);
            damaged.push(m);
            if b > 0 {
                let mut m = good.clone();
                for i in 0..64 {
                    m.swap(i, b * 64 + i);
                }
                damaged.push(m);
            }
        }
        let mut refused = 0;
        for m in damaged {
            match t.parse(&m) {
                Ok(v2) => assert_eq!(t.print(&v2) == m, t.check(&m).is_ok(), "{alias}"),
                Err(_) => refused += 1,
            }
            if let Ok(()) = t.check(&m) {
                assert_eq!(t.print(&t.parse(&m).unwrap()), m, "{alias}");
            }
        }
        assert!(good.is_empty() || refused > 0, "{alias}");
    }
}

#[test]
fn what_is_refused() {
    let dir = by_type("directory").unwrap();
    let bad = |list: Vec<RecV>| dir.check(&dir.print(&Value { head: None, list })).is_err();
    assert!(bad(vec![entry(k(1), 1, "b"), entry(k(2), 1, "a")]), "out of order");
    assert!(bad(vec![entry(k(1), 1, "a"), entry(k(2), 1, "a")]), "a name twice");
    assert!(bad(vec![entry(k(1), 4, "a")]), "a fourth kind");
    assert!(bad(vec![entry(k(1), 1, "..")]));
    assert!(bad(vec![entry(k(1), 1, "a/b")]));
    assert!(bad(vec![entry(NONE, 1, "a")]), "a child is not optional");

    let runs = by_type("runs").unwrap();
    let run = |key, n| RecV { keys: vec![key], ints: vec![n], ..Default::default() };
    assert!(runs.check(&[]).is_err(), "no runs");
    assert!(runs.check(&runs.print(&Value { head: None, list: vec![run(k(1), 0)] })).is_err(), "an empty run");
    assert!(
        runs.check(&runs.print(&Value { head: None, list: vec![run(k(1), 2), run(k(1), 3)] })).is_err(),
        "runs to join"
    );
    assert!(
        runs.check(&runs.print(&Value { head: None, list: vec![run(k(1), 2), run(NONE, 3), run(k(1), 1)] })).is_ok()
    );

    let mut b = by_type("lines").unwrap().print(&samples()[5].1);
    b[127] = 1;
    assert!(by_type("lines").unwrap().check(&b).is_err(), "padding is zero");
}

#[test]
fn text_formats() {
    typed::check_text("why this line changed: \u{5e9}\u{5dc}\u{5d5}\u{5dd}".as_bytes()).unwrap();
    assert!(typed::check_text(b"two\nlines").is_err());
    assert!(typed::check_text(&[0xff, 0xfe]).is_err());
    for ok in [
        "0.0.0",
        "1.4.0",
        "10.20.30",
        "2.0.0-rc.1",
        "1.0.0-alpha-1.0x7",
        "1.0.0+build.5",
        "1.0.0-0.3.7+exp.sha.5114f85",
    ] {
        typed::check_semver(ok.as_bytes()).unwrap_or_else(|e| panic!("{ok}: {e}"));
    }
    for bad in [
        "",
        "1",
        "1.2",
        "1.2.3.4",
        "01.2.3",
        "1.02.3",
        "1.2.3-",
        "1.2.3-01",
        "1.2.3-a..b",
        "1.2.3+",
        "v1.2.3",
        "1.2.3 ",
    ] {
        assert!(typed::check_semver(bad.as_bytes()).is_err(), "{bad}");
    }
}

#[test]
fn the_views_a_blob_holds_follow_from_its_type() {
    let id = |alias: &str| by_alias(alias).unwrap().id().to_string();
    let s = samples();
    let kids = by_type("directory").unwrap().children(&by_type("directory").unwrap().print(&s[1].1)).unwrap();
    let got: Vec<(String, String)> = kids.iter().map(|v| (v.format().to_string(), v.key().to_string())).collect();
    assert_eq!(
        got,
        vec![
            (id("raw"), key_text(&k(1))),
            (id("directory"), key_text(&k(2))),
            (id("symlink"), key_text(&k(3))),
            (id("raw"), key_text(&k(4)))
        ]
    );
    let kids = by_type("commit").unwrap().children(&by_type("commit").unwrap().print(&s[3].1)).unwrap();
    let aliases: Vec<&str> = kids.iter().map(|v| by_id(v.format()).unwrap().alias).collect();
    assert_eq!(aliases, ["directory", "author", "annotation", "commit", "annotation", "commit"]);
    let kids = by_type("release").unwrap().children(&by_type("release").unwrap().print(&s[7].1)).unwrap();
    let aliases: Vec<&str> = kids.iter().map(|v| by_id(v.format()).unwrap().alias).collect();
    assert_eq!(aliases, ["directory", "tag", "release", "note", "release", "note"]);
}
