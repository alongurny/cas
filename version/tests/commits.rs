//! Histories of commits: `directory` as a working directory format,
//! commits and their notes, and the rewrite of an earlier history.

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Policy, Session, Store, Who};
use cas_version::commits::{self, Commit, Converter, Notes};
use cas_version::history::{History, Step};
use cas_version::stepnotes::PathNotes;
use cas_version::{Node, Path, View, build, by_alias, diff, fs, put, walk};
use std::collections::BTreeMap;

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn store() -> Session {
    CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous())
}

fn tree(s: &dyn Store, format: &str, files: &[(&str, &str, &str)]) -> View {
    let mut m = BTreeMap::new();
    for (path, alias, text) in files {
        m.insert(Path::new(*path).unwrap(), Node::File(put(s, b3(), alias, text.as_bytes()).unwrap()));
    }
    build(s, b3(), by_alias(format).unwrap(), &m).unwrap()
}

fn paths(s: &dyn Store, v: &View) -> Vec<String> {
    walk(s, v, None)
        .map(|x| x.unwrap())
        .map(|(p, n)| match n {
            Node::Dir => format!("{p}/"),
            Node::File(f) => format!("{p}:{}", f.known_format().unwrap().alias),
        })
        .collect()
}

#[test]
fn a_directory_is_a_working_directory_format() {
    let s = store();
    let files = [("a.tex", "raw", "one\ntwo\n"), ("src/b.rs", "raw", "fn b() {}\n"), ("src/link", "symlink", "b.rs")];
    let v = tree(&s, "directory", &files);
    assert_eq!(v.known_format().unwrap().alias, "directory");
    assert_eq!(paths(&s, &v), ["a.tex:raw", "src/", "src/b.rs:raw", "src/link:symlink"]);
    // The same content in another directory format differs in directories only.
    let old = tree(&s, "tree", &files);
    assert_eq!(paths(&s, &old), paths(&s, &v));
    let mut c = Converter::new(&s, b3());
    assert_eq!(c.directory(&old).unwrap(), v.key());
    // An edit rewrites the directories on its path, in the same format.
    let edit = BTreeMap::from([(
        Path::new("src/c.rs").unwrap(),
        cas_version::Edit::Set(Node::File(put(&s, b3(), "raw", b"fn c() {}\n").unwrap())),
    )]);
    let v2 = cas_version::update(&s, b3(), &v, &edit).unwrap();
    let changes: Vec<String> = diff(&s, &v, &v2, None).unwrap().iter().map(|c| c.path.to_string()).collect();
    assert_eq!(changes, ["src/c.rs"]);
    // A file that may be run is a kind of its own.
    let exec =
        BTreeMap::from([(Path::new("run").unwrap(), Node::File(put(&s, b3(), "exec", b"#!/bin/sh\n").unwrap()))]);
    let e = build(&s, b3(), by_alias("directory").unwrap(), &exec).unwrap();
    assert_eq!(paths(&s, &e), ["run:exec"]);
    // What a directory cannot hold is refused, not written.
    let odd = BTreeMap::from([(Path::new("caf\u{e9}").unwrap(), Node::File(put(&s, b3(), "raw", b"").unwrap()))]);
    assert!(build(&s, b3(), by_alias("directory").unwrap(), &odd).is_err());
    let long = BTreeMap::from([(Path::new("n".repeat(256)).unwrap(), Node::File(put(&s, b3(), "raw", b"").unwrap()))]);
    assert!(build(&s, b3(), by_alias("directory").unwrap(), &long).is_err());
    let fits = BTreeMap::from([(Path::new("n".repeat(255)).unwrap(), Node::File(put(&s, b3(), "raw", b"").unwrap()))]);
    build(&s, b3(), by_alias("directory").unwrap(), &fits).unwrap();
}

#[test]
fn a_directory_goes_to_disk_and_back() {
    let s = store();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("bin")).unwrap();
    std::fs::write(dir.path().join("bin/run"), "#!/bin/sh\n").unwrap();
    std::fs::write(dir.path().join("a.tex"), "one\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir.path().join("bin/run"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let v = fs::import(&s, b3(), by_alias("directory").unwrap(), dir.path()).unwrap();
    assert_eq!(paths(&s, &v), ["a.tex:raw", "bin/", "bin/run:exec"]);
    let out = tempfile::tempdir().unwrap();
    fs::checkout(&s, &v, None, out.path()).unwrap();
    assert_eq!(std::fs::metadata(out.path().join("bin/run")).unwrap().permissions().mode() & 0o111, 0o111);
    assert_eq!(std::fs::read(out.path().join("bin/run")).unwrap(), b"#!/bin/sh\n");
    assert_eq!(fs::import(&s, b3(), by_alias("directory").unwrap(), out.path()).unwrap(), v);
}

fn notes() -> Notes {
    let run = |n: usize, t: &str| (n, t.to_string());
    Notes {
        note: "tighten the bound".into(),
        paths: BTreeMap::from([
            (
                "a.tex".to_string(),
                PathNotes { note: String::new(), old: None, new: Some(vec![run(1, ""), run(1, "the new line")]) },
            ),
            ("src".to_string(), PathNotes { note: "moved here".into(), old: None, new: None }),
            (
                "src/b.rs".to_string(),
                PathNotes {
                    note: "rewritten".into(),
                    old: Some(vec![run(1, "was wrong")]),
                    new: Some(vec![run(1, "tighten the bound")]),
                },
            ),
        ]),
    }
}

#[test]
fn notes_are_an_annotation_tree_and_read_back() {
    let s = store();
    let n = notes();
    let key = commits::write_notes(&s, b3(), &n, &|p| Ok(p == "src")).unwrap().unwrap();
    assert_eq!(commits::read_notes(&s, &key).unwrap(), n);
    assert_eq!(commits::top_note(&s, &key).unwrap(), "tighten the bound");
    // Every blob written is valid in its format, and reached from the top.
    let mut todo = vec![commits::view("annotation", &key)];
    let mut seen = 0;
    while let Some(v) = todo.pop() {
        let b = s.read(v.key()).unwrap().unwrap();
        v.known_format().unwrap().check(v.key(), &b).unwrap();
        todo.extend(cas_version::sync::children(&s, &v).unwrap());
        seen += 1;
    }
    // annotation, lines of a.tex, its runs, annotation of src, lines of
    // b.rs, two runs, and each of the notes where it is named.
    assert!(seen >= 7, "{seen}");
    // Nothing to say is no annotation.
    assert_eq!(commits::write_notes(&s, b3(), &Notes::default(), &|_| Ok(false)).unwrap(), None);
    // Runs must cover their file's lines.
    let before = tree(&s, "directory", &[("src/b.rs", "raw", "x\n")]);
    let after = tree(&s, "directory", &[("a.tex", "raw", "one\ntwo\n"), ("src/b.rs", "raw", "y\n")]);
    commits::fits(&s, &n, Some(&before), &after).unwrap();
    let short = tree(&s, "directory", &[("a.tex", "raw", "one\n"), ("src/b.rs", "raw", "y\n")]);
    assert!(commits::fits(&s, &n, Some(&before), &short).is_err());
}

#[test]
fn commits_make_a_history() {
    let s = store();
    let author = commits::put_text(&s, b3(), "author", "Dana <dana@example.org>").unwrap();
    let v1 = tree(&s, "directory", &[("a.tex", "raw", "one\n")]);
    let v2 = tree(&s, "directory", &[("a.tex", "raw", "one\ntwo\n")]);
    let first =
        Commit { version: v1.key().into(), author: author.clone(), notes: None, time: 100, parents: BTreeMap::new() };
    let k1 = commits::write(&s, b3(), &first).unwrap();
    assert_eq!(commits::read(&s, &k1).unwrap(), first);
    commits::advance(&s, "main", None, &k1).unwrap();
    let step =
        commits::write_notes(&s, b3(), &Notes { note: "add a line".into(), paths: BTreeMap::new() }, &|_| Ok(false))
            .unwrap();
    let second = Commit {
        version: v2.key().into(),
        author: author.clone(),
        notes: None,
        time: 200,
        parents: BTreeMap::from([(k1.clone(), step)]),
    };
    let k2 = commits::write(&s, b3(), &second).unwrap();
    // The history moves only from the commit it holds.
    assert!(commits::advance(&s, "main", None, &k2).is_err());
    commits::advance(&s, "main", Some(&k1), &k2).unwrap();
    assert_eq!(commits::tip(&s, "main").unwrap().as_deref(), Some(k2.as_str()));
    // A merge of two commits made side by side.
    let side = Commit { time: 150, parents: BTreeMap::from([(k1.clone(), None)]), ..second.clone() };
    let k3 = commits::write(&s, b3(), &side).unwrap();
    let merge =
        Commit { time: 300, parents: BTreeMap::from([(k2.clone(), None), (k3.clone(), None)]), ..second.clone() };
    let k4 = commits::write(&s, b3(), &merge).unwrap();
    let log: Vec<String> = commits::log(&s, &k4).unwrap().into_iter().map(|(k, _)| k).collect();
    assert_eq!(log, [k4.clone(), k2.clone(), k3.clone(), k1.clone()]);
    assert_eq!(commits::line(&s, &second).unwrap(), "add a line");
    assert_eq!(commits::text(&s, &author).unwrap(), "Dana <dana@example.org>");
    assert_eq!(commits::resolve(&s, &k4, "latest").unwrap(), Some(k4.clone()));
    assert_eq!(commits::resolve(&s, &k4, &k1[7..19]).unwrap(), Some(k1.clone()));
    assert_eq!(commits::resolve(&s, &k4, "ffffffffffff").unwrap(), None);
    // A history of the earlier format is not read as commits.
    let mut h = History::default();
    h.append(&tree(&s, "tree", &[("a.tex", "raw", "one\n")]), Step::default(), None).unwrap();
    cas_version::history::save(&s, b3(), "old", None, &h).unwrap();
    assert!(commits::tip(&s, "old").is_err());
}

#[test]
fn an_earlier_history_becomes_commits() {
    let s = store();
    let o1 = tree(&s, "tree", &[("a.tex", "raw", "one\n"), ("run", "exec", "#!/bin/sh\n")]);
    let o2 = tree(&s, "tree", &[("a.tex", "raw", "one\ntwo\n"), ("run", "exec", "#!/bin/sh\n")]);
    let mut h = History::default();
    let n1 = h.append(&o1, Step::default(), Some("the first\nversion".into())).unwrap();
    let sn = cas_version::stepnotes::StepNotes {
        before: Some(o1.clone()),
        after: o2.clone(),
        note: "add a line".into(),
        paths: BTreeMap::from([(
            "a.tex".to_string(),
            PathNotes { note: String::new(), old: None, new: Some(vec![(1, String::new()), (1, "add a line".into())]) },
        )]),
    };
    let step =
        Step { changelog: None, compatible: false, notes: Some(cas_version::stepnotes::write(&s, b3(), &sn).unwrap()) };
    let n2 = h.append(&o2, step, None).unwrap();
    let made = Converter::new(&s, b3()).history(&h, &|id| ("Dana".into(), if *id == n1 { 100 } else { 200 })).unwrap();
    let c2 = commits::read(&s, &made[&n2]).unwrap();
    assert_eq!(c2.time, 200);
    assert_eq!(commits::text(&s, &c2.author).unwrap(), "Dana");
    assert_eq!(c2.parents.keys().collect::<Vec<_>>(), [&made[&n1]]);
    assert_eq!(paths(&s, &c2.version()), ["a.tex:raw", "run:exec"]);
    let step = c2.parents[&made[&n1]].clone().unwrap();
    let n = commits::read_notes(&s, &step).unwrap();
    assert_eq!((n.note.as_str(), &n.paths), ("add a line", &sn.paths));
    let c1 = commits::read(&s, &made[&n1]).unwrap();
    commits::fits(&s, &n, Some(&c1.version()), &c2.version()).unwrap();
    assert_eq!(commits::line(&s, &c1).unwrap(), "the first / version");
    assert_eq!(commits::line(&s, &c2).unwrap(), "add a line");
}

#[test]
fn a_rebase_replays_a_chain_and_a_merge_joins_two() {
    let s = store();
    let author = commits::put_text(&s, b3(), "author", "Dana").unwrap();
    let commit = |files: &[(&str, &str, &str)], time: u64, parent: Option<(&str, Option<String>)>| {
        let c = Commit {
            version: tree(&s, "directory", files).key().into(),
            author: author.clone(),
            notes: None,
            time,
            parents: parent.into_iter().map(|(p, n)| (p.to_string(), n)).collect(),
        };
        commits::write(&s, b3(), &c).unwrap()
    };
    let base = commit(&[("a", "raw", "1\n")], 1, None);
    let onto = commit(&[("a", "raw", "1\n"), ("c", "raw", "c\n")], 2, Some((&base, None)));
    let n = Notes {
        note: "add b".into(),
        paths: BTreeMap::from([(
            "b".to_string(),
            PathNotes { note: String::new(), old: None, new: Some(vec![(1, "new".into())]) },
        )]),
    };
    let step = commits::write_notes(&s, b3(), &n, &|_| Ok(false)).unwrap();
    let mine = commit(&[("a", "raw", "1\n"), ("b", "raw", "b\n")], 3, Some((&base, step.clone())));
    let mine2 = commit(&[("a", "raw", "1\n"), ("b", "raw", "b\nb\n")], 4, Some((&mine, None)));

    let commits::Outcome::Done(tip) = commits::rebase(&s, b3(), &mine2, &onto).unwrap() else { panic!("not done") };
    let log = commits::log(&s, &tip).unwrap();
    assert_eq!(log.len(), 4);
    assert_eq!(log.iter().map(|(_, c)| c.time).collect::<Vec<_>>(), [4, 3, 2, 1]);
    assert_eq!(log[2].0, onto);
    assert_eq!(paths(&s, &log[0].1.version()), ["a:raw", "b:raw", "c:raw"]);
    // The step's notes still fit, so the annotation is the same blob.
    assert_eq!(log[1].1.parents[&onto], step);
    assert_eq!(commits::rebase(&s, b3(), &tip, &onto).unwrap(), commits::Outcome::UpToDate);
    assert_eq!(commits::rebase(&s, b3(), &onto, &tip).unwrap(), commits::Outcome::FastForward(tip.clone()));

    // A merge of the two sides gives the same version, with both as parents.
    let commits::Outcome::Done(m) = commits::merge(&s, b3(), &mine2, &onto, "Dana", 9).unwrap() else {
        panic!("not done")
    };
    let mc = commits::read(&s, &m).unwrap();
    assert_eq!(mc.version, log[0].1.version);
    assert_eq!(mc.parents.len(), 2);
    assert_eq!(commits::merge_base(&s, &mine2, &onto).unwrap(), Some(base.clone()));
    // Both sides changing one file differently is a conflict; nothing is written.
    let clash = commit(&[("a", "raw", "2\n")], 5, Some((&base, None)));
    let other = commit(&[("a", "raw", "3\n")], 6, Some((&base, None)));
    assert!(matches!(commits::merge(&s, b3(), &clash, &other, "Dana", 9).unwrap(), commits::Outcome::Conflicts(_)));
    assert!(matches!(commits::rebase(&s, b3(), &clash, &other).unwrap(), commits::Outcome::Conflicts(_)));
}
