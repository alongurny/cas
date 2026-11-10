//! Annotated text and step diffs (spec/formats/annotated.md, diff.md).

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Policy, Session, Store, Who};
use cas_version::annotated::{self, Annotated};
use cas_version::history::{self, Step};
use cas_version::{Node, Path, View, build, formats, gc, put, read_blob, sync};
use std::collections::BTreeMap;

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn store() -> Session {
    CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous())
}

fn version(s: &dyn Store, files: &[(&str, &[u8])]) -> View {
    let mut m = BTreeMap::new();
    for (p, c) in files {
        m.insert(Path::new(*p).unwrap(), Node::File(put(s, b3(), "raw", c).unwrap()));
    }
    build(s, b3(), formats::by_alias("tree").unwrap(), &m).unwrap()
}

#[test]
fn the_authoring_form_round_trips() {
    let text = b"one\n\ntwo\nlast without newline".to_vec();
    let notes = ["why one", "", "", "and the end"];
    let form = annotated::render(&text, &notes);
    let (t, n) = annotated::parse(&form).unwrap();
    assert_eq!(t, text);
    assert_eq!(n, notes.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert!(annotated::parse(b"| a\nstray\n").is_err());
    assert!(annotated::parse(b"\\\n").is_err());
    assert!(annotated::parse(b"| a\n\\\n| b\n").is_err(), "text after the last line");
}

#[test]
fn annotations_are_runs_and_cover_every_line() {
    let s = store();
    let target = put(&s, b3(), "raw", b"a\nb\nc\nd\n").unwrap();
    let notes: Vec<String> = ["x", "x", "", "y"].iter().map(|s| s.to_string()).collect();
    let v = annotated::write(&s, b3(), &target, &notes).unwrap();
    let bytes = read_blob(&s, v.key()).unwrap();
    assert_eq!(bytes, format!("annotated {target}\n2 x\n1 \n1 y\n").into_bytes());
    let a = annotated::verify(&s, v.key()).unwrap();
    assert_eq!(a.notes(), ["x", "x", "", "y"]);
    // Too few notes, adjacent equal runs, a control character: refused.
    assert!(annotated::write(&s, b3(), &target, &notes[..3]).is_err());
    assert!(Annotated::decode(format!("annotated {target}\n1 x\n1 x\n").as_bytes()).is_err());
    assert!(Annotated::decode(format!("annotated {target}\n01 x\n").as_bytes()).is_err());
    assert!(annotated::write(&s, b3(), &target, &["a\tb".into(), "".into(), "".into(), "".into()]).is_err());
}

/// A diff applies: every file's hunks turn the old text into the new.
#[test]
fn diffs_apply_to_the_old_files() {
    let s = store();
    let mut seed = 7u64;
    let mut rand = |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    for round in 0..40 {
        let old: Vec<String> = (0..rand(40)).map(|i| format!("line {i}\n")).collect();
        let mut new = old.clone();
        for _ in 0..rand(6) {
            let at = rand(new.len() as u64 + 1) as usize;
            match rand(3) {
                0 if at < new.len() => {
                    new.remove(at);
                }
                1 if at < new.len() => new[at] = format!("changed {round}\n"),
                _ => new.insert(at, format!("inserted {round}\n")),
            }
        }
        let mut new = new.concat().into_bytes();
        if rand(2) == 0 {
            new.pop();
        }
        let old = old.concat().into_bytes();
        let (a, b) = (version(&s, &[("f", &old), ("same", b"x\n")]), version(&s, &[("f", &new), ("same", b"x\n")]));
        let d = annotated::diff(&s, Some(&a), &b).unwrap();
        let (da, db, files) = annotated::parse_diff(&d).unwrap();
        assert_eq!((da.as_ref(), &db), (Some(&a), &b));
        if old == new {
            assert!(files.is_empty());
            continue;
        }
        assert_eq!(files.len(), 1, "{}", String::from_utf8_lossy(&d));
        assert_eq!(annotated::apply(&old, &files[0]).unwrap(), new, "{}", String::from_utf8_lossy(&d));
    }
}

#[test]
fn a_diff_lists_added_removed_and_binary_files() {
    let s = store();
    let a = version(&s, &[("gone", b"bye\n"), ("bin", b"a\0b"), ("keep", b"k\n")]);
    let b = version(&s, &[("bin", b"a\0c"), ("keep", b"k\n"), ("new/f", b"hi\n")]);
    let d = String::from_utf8(annotated::diff(&s, Some(&a), &b).unwrap()).unwrap();
    let paths: Vec<&str> = d.lines().filter_map(|l| l.strip_prefix("path ")).collect();
    assert_eq!(paths, ["bin", "gone", "new", "new/f"]);
    assert!(d.contains("path new\nold -\nnew dir\npath new/f\n"));
    assert!(d.contains("binary\n"));
    assert!(d.contains("@@ -1,1 +0,0 @@\n-bye\n"));
    assert!(d.contains("@@ -0,0 +1,1 @@\n+hi\n"));
    // From nothing: everything is added.
    let root = String::from_utf8(annotated::diff(&s, None, &a).unwrap()).unwrap();
    assert!(root.starts_with(&format!("diff - {a}\n")));
    assert_eq!(root.matches("path ").count(), 3, "{root}");
    assert!(annotated::check_diff(root.as_bytes()).is_ok());
}

/// A step's notes are kept with the history: read back, copied by a sync,
/// kept by gc.
#[test]
fn step_notes_live_in_the_history() {
    let s = store();
    let a = version(&s, &[("f", b"one\ntwo\n")]);
    let b = version(&s, &[("f", b"one\n2\n")]);
    history::commit(&s, b3(), "main", &a, Step::default(), None).unwrap();
    let d = annotated::write_diff(&s, b3(), Some(&a), &b).unwrap();
    let n = annotated::lines(&read_blob(&s, d.key()).unwrap()).len();
    let mut notes = vec![String::new(); n];
    notes[n - 1] = "two as a digit".into();
    let nv = annotated::write(&s, b3(), &d, &notes).unwrap();
    let id = history::commit(&s, b3(), "main", &b, Step { notes: Some(nv.clone()), ..Step::default() }, None).unwrap();
    formats::publish(&s).unwrap();
    let head = history::head(&s, "main").unwrap().unwrap();
    let h = history::read(&s, &head).unwrap();
    let step = h.nodes[&id].after.values().next().unwrap();
    assert_eq!(step.notes.as_ref(), Some(&nv));
    assert!(String::from_utf8(annotated::show(&s, nv.key()).unwrap()).unwrap().contains("# two as a digit\n| +2\n"));
    // Another store gets the notes, their diff and its text with the history.
    let t = store();
    sync::copy(&s, &t, &head).unwrap();
    assert_eq!(annotated::show(&t, nv.key()).unwrap(), annotated::show(&s, nv.key()).unwrap());
    // gc keeps them.
    for _ in 0..3 {
        gc::collect(&s, b3()).unwrap();
    }
    assert!(annotated::verify(&s, nv.key()).is_ok());
    assert!(sync::has(&s, d.key()).unwrap());
}
