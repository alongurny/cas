//! Annotated text and step diffs (spec/formats/annotated.md, diff.md).

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Policy, Session, Store, Who};
use cas_version::annotated::{self, Annotated};
use cas_version::history::{self, Step};
use cas_version::stepnotes;
use cas_version::{Node, Path, View, build, formats, gc, put, read_blob, sync};
use std::collections::BTreeMap;

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn store() -> Session {
    CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous())
}

/// Step notes from `a` to `b`: `note` of each line of their diff.
fn step_notes(s: &dyn Store, a: &View, b: &View, note: impl Fn(&[u8]) -> &'static str) -> View {
    let (d, lens) = annotated::diff_with_lens(s, Some(a), b).unwrap();
    let notes: Vec<String> = annotated::lines(&d).into_iter().map(|l| note(l).to_string()).collect();
    let sn = stepnotes::from_lines(Some(a), b, &d, &lens, &notes).unwrap().unwrap();
    stepnotes::write(s, b3(), &sn).unwrap()
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
    let nv = step_notes(&s, &a, &b, |l| if l == b"+2\n" { "two as a digit" } else { "" });
    // Kept on the new file's second line; no diff is stored.
    let sn = stepnotes::verify(&s, nv.key()).unwrap();
    assert_eq!(sn.paths["f"].new, Some(vec![(1, String::new()), (1, "two as a digit".into())]));
    assert_eq!(sn.paths["f"].old, None);
    let id = history::commit(&s, b3(), "main", &b, Step { notes: Some(nv.clone()), ..Step::default() }, None).unwrap();
    formats::publish(&s).unwrap();
    let head = history::head(&s, "main").unwrap().unwrap();
    let h = history::read(&s, &head).unwrap();
    let step = h.nodes[&id].after.values().next().unwrap();
    assert_eq!(step.notes.as_ref(), Some(&nv));
    let shown = String::from_utf8(stepnotes::show(&s, nv.key()).unwrap()).unwrap();
    assert!(shown.contains("| -two\n# two as a digit\n| +2\n"), "{shown}");
    // The authoring form goes back to the same notes.
    let again = stepnotes::from_form(&s, Some(&a), &b, shown.as_bytes()).unwrap().unwrap();
    assert_eq!(again, sn);
    // Another store gets the notes with the history.
    let t = store();
    sync::copy(&s, &t, &head).unwrap();
    assert_eq!(stepnotes::show(&t, nv.key()).unwrap(), stepnotes::show(&s, nv.key()).unwrap());
    // gc keeps them.
    for _ in 0..3 {
        gc::collect(&s, b3()).unwrap();
    }
    assert!(stepnotes::verify(&s, nv.key()).is_ok());
}

/// A node's annotated files are kept with it, copied and kept by gc.
#[test]
fn node_notes_annotate_files_as_they_are() {
    let s = store();
    let v = version(&s, &[("f", b"alpha\nbeta\n")]);
    let file = cas_version::object_at(&s, &v, &Path::new("f").unwrap()).unwrap().unwrap();
    let n = annotated::write(&s, b3(), &file, &["the first".into(), String::new()]).unwrap();
    let notes = BTreeMap::from([("f".to_string(), n.clone())]);
    let id = history::commit_with_notes(&s, b3(), "main", &v, Step::default(), None, notes.clone()).unwrap();
    let head = history::head(&s, "main").unwrap().unwrap();
    let h = history::read(&s, &head).unwrap();
    assert_eq!(h.nodes[&id].notes, notes);
    assert!(h.has_notes());
    formats::publish(&s).unwrap();
    let t = store();
    sync::copy(&s, &t, &head).unwrap();
    assert_eq!(annotated::show(&t, n.key()).unwrap(), annotated::show(&s, n.key()).unwrap());
    for _ in 0..3 {
        gc::collect(&s, b3()).unwrap();
    }
    assert!(annotated::verify(&s, n.key()).is_ok());
    // A note on something that is not an annotation is refused.
    let bad = cas_version::uni::Value::map([(
        "nodes",
        cas_version::uni::Value::map([(
            id.as_str(),
            cas_version::uni::Value::map([(
                "notes",
                cas_version::uni::Value::map([("f", cas_version::uni::Value::str(file.to_string()))]),
            )]),
        )]),
    )]);
    assert!(history::History::from_value(&bad).is_err());
}

/// The release history gets versions and release notes, never notes.
#[test]
fn publishing_keeps_notes_out_of_the_release_history() {
    use cas_version::merge::{self, Outcome};
    let s = store();
    let a = version(&s, &[("f", b"1\n")]);
    let b = version(&s, &[("f", b"2\n")]);
    let mut mine = history::History::default();
    let ra = mine.add(&a, BTreeMap::new(), None).unwrap();
    let notes = step_notes(&s, &a, &b, |_| "why");
    mine.add(&b, BTreeMap::from([(ra, Step { notes: Some(notes.clone()), ..Step::default() })]), None).unwrap();
    assert!(mine.has_notes());
    let mut public = history::History::default();
    let first = Step { changelog: Some("Release 1".into()), ..Step::default() };
    let Outcome::Done(r1) = merge::publish(&mine, &mut public, first, None).unwrap() else { panic!() };
    assert!(!public.has_notes());
    assert_eq!(public.nodes[&r1].doc.as_deref(), Some("Release 1"), "the first release keeps its notes as doc");
    assert_eq!(r1.version(), b);
    // An annotated step, or a target with notes, is refused.
    let annotated_step = Step { notes: Some(notes), ..Step::default() };
    assert!(merge::publish(&mine, &mut history::History::default(), annotated_step, None).is_err());
    assert!(merge::publish(&mine, &mut mine.clone(), Step::default(), None).is_err());
}

/// A rebase carries notes over to the replayed step's diff and to files
/// after the merge, line by line.
#[test]
fn a_rebase_carries_notes_along() {
    use cas_version::merge::{self, Outcome};
    let s = store();
    let base = version(&s, &[("f", b"a\nb\nc\n"), ("g", b"g\n")]);
    let mut public = history::History::default();
    let r0 = public.add(&base, BTreeMap::new(), None).unwrap();
    let mut mine = public.clone();
    // Mine changes f; public meanwhile changes g.
    let ours = version(&s, &[("f", b"a\nB\nc\n"), ("g", b"g\n")]);
    let sn = step_notes(&s, &base, &ours, |l| if l == b"+B\n" { "capital" } else { "" });
    let file = cas_version::object_at(&s, &ours, &Path::new("f").unwrap()).unwrap().unwrap();
    let fnote = annotated::write(&s, b3(), &file, &["".into(), "the B line".into(), "".into()]).unwrap();
    let m1 =
        mine.add(&ours, BTreeMap::from([(r0.clone(), Step { notes: Some(sn), ..Step::default() })]), None).unwrap();
    mine.nodes.get_mut(&m1).unwrap().notes.insert("f".into(), fnote);
    let theirs = version(&s, &[("f", b"a\nb\nc\n"), ("g", b"G\n")]);
    public.add(&theirs, BTreeMap::from([(r0, Step::default())]), None).unwrap();
    let (rebased, outcome) = merge::rebase(&s, b3(), &mine, &public).unwrap();
    let Outcome::Done(tip) = outcome else { panic!("{outcome:?}") };
    let node = &rebased.nodes[&tip];
    let step = node.after.values().next().unwrap();
    let carried = String::from_utf8(stepnotes::show(&s, step.notes.as_ref().unwrap().key()).unwrap()).unwrap();
    assert!(carried.contains("# capital\n| +B\n"), "{carried}");
    let a = stepnotes::verify(&s, step.notes.as_ref().unwrap().key()).unwrap();
    assert_eq!((a.before, a.after), (Some(theirs), tip.version()), "the notes are on the new step's versions");
    let fa = annotated::verify(&s, node.notes["f"].key()).unwrap();
    assert_eq!(fa.notes(), ["", "the B line", ""]);
}

/// Diffs of many random edits parse and apply back. similar 2.7 once gave a
/// `Delete` after a `Replace` a stale new index, so hunk headers that
/// trusted it had the wrong counts.
#[test]
fn random_diffs_parse_and_apply() {
    let s = store();
    let mut seed: u64 = 7;
    let mut rnd = |n: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };
    for case in 0..400 {
        let len = 1 + rnd(60) as usize;
        let a: Vec<String> = (0..len).map(|_| format!("l{}\n", rnd(8))).collect();
        let mut b = a.clone();
        for _ in 0..1 + rnd(6) {
            let at = rnd(b.len() as u64 + 1) as usize;
            match rnd(3) {
                0 if at < b.len() => {
                    b.remove(at);
                }
                1 if at < b.len() => b[at] = format!("r{}\n", rnd(30)),
                _ => {
                    for k in 0..rnd(25) {
                        b.insert(at, format!("n{k}\n"));
                    }
                }
            }
        }
        let (a, b) = (a.concat().into_bytes(), b.concat().into_bytes());
        let (va, vb) = (version(&s, &[("f", &a)]), version(&s, &[("f", &b)]));
        let d = annotated::diff(&s, Some(&va), &vb).unwrap();
        let (_, _, files) = annotated::parse_diff(&d).unwrap_or_else(|e| panic!("case {case}: {e}"));
        if a != b {
            assert_eq!(annotated::apply(&a, &files[0]).unwrap(), b, "case {case}");
        }
    }
}

/// The shape of a real edit (each number a distinct line) for which
/// similar 2.7 gave the stale index.
#[test]
fn a_delete_after_replacements_keeps_its_counts() {
    let s = store();
    let text = |ids: &[u32]| ids.iter().map(|i| format!("l{i}\n")).collect::<String>().into_bytes();
    let a = text(&[
        0, 1, 2, 1, 3, 4, 1, 5, 6, 1, 7, 8, 9, 1, 10, 11, 1, 12, 13, 1, 14, 1, 7, 15, 16, 1, 17, 1, 18, 11, 1, 19, 1,
        20, 21, 22, 1, 23, 1, 24, 11, 1, 25, 1, 26, 1, 27, 28, 29, 30, 1, 31, 11, 1, 32, 33, 34, 1, 35, 1, 36, 1, 37,
        38, 39, 1, 40, 41, 42, 30, 1, 43, 11, 1, 44, 45, 46, 1, 47, 48, 1, 49, 50, 51, 1, 52, 1, 53, 11, 1, 54, 1, 55,
        56, 57, 1, 58, 1, 59, 1, 60, 1, 61, 1, 62,
    ]);
    let b = text(&[
        0, 1, 2, 1, 3, 4, 1, 5, 6, 1, 7, 8, 9, 1, 10, 11, 1, 12, 13, 1, 14, 1, 7, 15, 16, 1, 17, 1, 18, 11, 1, 19, 1,
        20, 21, 22, 1, 23, 1, 24, 11, 1, 25, 1, 26, 1, 27, 28, 29, 30, 1, 31, 11, 1, 32, 33, 34, 1, 35, 1, 36, 1, 37,
        38, 39, 1, 40, 41, 42, 30, 1, 43, 11, 1, 44, 45, 46, 1, 47, 48, 1, 49, 50, 51, 1, 52, 1, 53, 11, 1, 54, 1, 55,
        56, 57, 1, 58, 1, 59, 1, 63, 64, 1, 65, 1, 66, 67, 68, 69, 70, 71, 72, 73, 11, 1, 74, 1, 75, 1, 76, 77, 78, 79,
        80, 81, 82, 83, 84, 11, 1,
    ]);
    let (va, vb) = (version(&s, &[("f", &a)]), version(&s, &[("f", &b)]));
    let d = annotated::diff(&s, Some(&va), &vb).unwrap();
    let (_, _, files) = annotated::parse_diff(&d).unwrap();
    assert_eq!(annotated::apply(&a, &files[0]).unwrap(), b);
}

/// A step-notes blob is checked: runs cover the files, notes say something,
/// paths are in order.
#[test]
fn step_notes_are_checked() {
    let s = store();
    let a = version(&s, &[("f", b"1\n")]);
    let b = version(&s, &[("f", b"1\n2\n")]);
    let v = step_notes(&s, &a, &b, |l| if l == b"+2\n" { "two" } else { "" });
    let text = String::from_utf8(read_blob(&s, v.key()).unwrap()).unwrap();
    assert!(text.ends_with("\npath f\nnew\n1 \n1 two\n"), "{text}");
    for bad in [text.replace("1 two", "2 two"), text.replace("1 two", "1 "), text.replace("path f\n", "path g\n")] {
        let k = cas_version::write_blob(&s, b3(), bad.as_bytes()).unwrap();
        assert!(stepnotes::verify(&s, &k).is_err(), "{bad}");
    }
    assert!(stepnotes::check(text.replace("new\n1 \n1 two\n", "").as_bytes()).is_err(), "a path with no note");
    // fsck finds notes that do not cover their file.
    history::commit(&s, b3(), "main", &a, Step::default(), None).unwrap();
    let wrong = cas_version::write_blob(&s, b3(), text.replace("1 two", "2 two").as_bytes()).unwrap();
    let wrong = View::new(v.format(), wrong).unwrap();
    history::commit(&s, b3(), "main", &b, Step { notes: Some(wrong.clone()), ..Step::default() }, None).unwrap();
    formats::publish(&s).unwrap();
    let report = cas_version::fsck::fsck(&s).unwrap();
    assert_eq!(report.bad.len(), 1, "{:?}", report.bad);
    assert_eq!(report.bad[0].0, wrong.key());
    // A form whose text is not the diff is refused.
    assert!(stepnotes::from_form(&s, Some(&a), &b, b"| diff\n").is_err());
}
