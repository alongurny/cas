//! End to end: `cas lean` on a tiny environment, through an edit.

use std::path::Path;
use std::process::Command;

fn run(store: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_cas")).arg("--store").arg(store).args(args).output().unwrap();
    (out.status.success(), String::from_utf8(out.stdout).unwrap() + &String::from_utf8_lossy(&out.stderr))
}

fn cas(store: &Path, args: &[&str]) -> String {
    let (ok, text) = run(store, args);
    assert!(ok, "cas {args:?}: {text}");
    text
}

/// `axiom A : Sort <level>`, `axiom a : A`, `def b : A := a`, and
/// `theorem t : A := b` when asked (a theorem of a type that is not a
/// proposition, which Lean's kernel refuses).
fn export(level: u32, theorem: bool) -> String {
    let mut s = String::from("N 1 0 s 41\nL 2 z\n");
    let mut l = 2;
    for _ in 0..level {
        s.push_str(&format!("L {} s {l}\n", l + 1));
        l += 1;
    }
    s.push_str(&format!("E 20 s {l}\nD 1 20 0 | axiom\n"));
    s.push_str("N 21 0 s 61\nE 22 c 1\nD 21 22 0 | axiom\n");
    s.push_str("N 23 0 s 62\nE 24 c 21\nD 23 22 0 | def 24 o\n");
    if theorem {
        s.push_str("N 25 0 s 74\nE 26 c 23\nD 25 22 0 | thm 26\n");
    }
    s
}

#[test]
fn import_prove_check_and_an_edit() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, file) = (tmp.path().join("store"), tmp.path().join("export.txt"));
    let f = file.to_str().unwrap();
    std::fs::write(&file, export(1, false)).unwrap();
    assert!(cas(&store, &["lean", "import", f]).starts_with("3 declarations, 3 new"));

    // Nothing is proved yet: the check says so and fails.
    let (ok, text) = run(&store, &["lean", "check", "b"]);
    assert!(!ok && text.contains("no proof is stored"), "{text}");

    assert!(cas(&store, &["lean", "prove", "b"]).ends_with("1 proved, 0 kept, 0 failed\n"));
    assert!(cas(&store, &["lean", "check", "b"]).contains("1 accepted, 0 refused"));
    // The closure is not proved yet.
    assert!(!run(&store, &["lean", "check", "b", "--closure"]).0);
    assert!(cas(&store, &["lean", "prove", "b", "--closure"]).ends_with("2 proved, 1 kept, 0 failed\n"));
    assert!(cas(&store, &["lean", "check", "--all"]).contains("3 accepted, 0 refused"));
    assert_eq!(cas(&store, &["lean", "axioms", "b"]).lines().count(), 2);
    let shown = cas(&store, &["lean", "show", "b"]);
    assert!(shown.contains("kind     definition") && shown.contains("(ok, "), "{shown}");
    // A declaration is also named by its key.
    let key = shown.lines().find_map(|l| l.trim().strip_prefix("key")).unwrap().trim().to_string();
    assert!(cas(&store, &["lean", "check", &key]).contains("1 accepted"));

    // A changes: a and b have the same text, but their proofs were made
    // against the old A.
    std::fs::write(&file, export(2, false)).unwrap();
    assert!(cas(&store, &["lean", "import", f]).starts_with("3 declarations, 1 new"));
    let (ok, text) = run(&store, &["lean", "check", "b"]);
    assert!(!ok && text.contains("made against other declarations"), "{text}");
    assert!(cas(&store, &["lean", "report"]).contains("2 made against other declarations"));
    assert!(cas(&store, &["lean", "prove", "--all"]).ends_with("3 proved, 0 kept, 0 failed\n"));
    assert!(cas(&store, &["lean", "check", "--all", "--quiet"]).contains("3 accepted, 0 refused"));

    // A theorem that Lean's kernel refuses is not proved, and that is kept.
    std::fs::write(&file, export(2, true)).unwrap();
    cas(&store, &["lean", "import", f]);
    let (ok, text) = run(&store, &["lean", "prove", "--all"]);
    assert!(!ok && text.contains("0 proved, 3 kept, 1 failed"), "{text}");
    let (ok, text) = run(&store, &["lean", "check", "t"]);
    assert!(!ok && text.contains("not proved"), "{text}");
    assert!(cas(&store, &["lean", "report", "--failed"]).contains("  t\t"));

    // The store's own check reads the new formats.
    cas(&store, &["fsck"]);
}
