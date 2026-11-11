//! End to end: the `cas` binary through every stage.

use std::path::Path;
use std::process::{Command, Stdio};

fn cas(store: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_cas")).arg("--store").arg(store).args(args).output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(out.status.success(), "cas {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    text
}

/// As `cas`, on a history in the earlier format (the forge still keeps it):
/// `--nodes` starts one.
fn old(store: &Path, args: &[&str]) -> String {
    let mut all = vec!["--nodes"];
    all.extend(args);
    cas(store, &all)
}

#[test]
fn commit_log_diff_checkout_and_sync() {
    let tmp = tempfile::tempdir().unwrap();
    let (work, store, other) = (tmp.path().join("work"), tmp.path().join("store"), tmp.path().join("other"));
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::create_dir_all(work.join("empty")).unwrap();
    std::fs::write(work.join("README"), "hi\n").unwrap();
    std::fs::write(work.join("src/lib.rs"), "fn a() {}\n").unwrap();
    let w = work.to_str().unwrap();

    old(&store, &["commit", w]);
    std::fs::write(work.join("src/lib.rs"), "fn b() {}\n").unwrap();
    old(&store, &["commit", w]);

    let log = old(&store, &["log"]);
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2);

    assert_eq!(old(&store, &["diff"]).trim(), "raw      -> raw      src/lib.rs");
    assert_eq!(old(&store, &["summary"]).trim(), "{'dirs': '2', 'files': '2'}");
    assert_eq!(old(&store, &["path-log", "src"]).lines().count(), 2);
    assert_eq!(old(&store, &["path-log", "README"]).lines().count(), 1);
    assert!(old(&store, &["ls"]).contains("dir      empty"));

    let out = tmp.path().join("out");
    old(&store, &["checkout", out.to_str().unwrap(), "src"]);
    assert_eq!(std::fs::read_to_string(out.join("src/lib.rs")).unwrap(), "fn b() {}\n");
    assert!(!out.join("README").exists());

    // Sync to a second store directory, then over HTTPS to a third.
    assert!(old(&store, &["push", other.to_str().unwrap()]).starts_with("Created"));
    assert!(old(&store, &["push", other.to_str().unwrap()]).starts_with("UpToDate"));
    assert_eq!(old(&other, &["log"]), log);

    // Serve a third store under an access policy: anyone reads, alice writes.
    let (served, meta) = (tmp.path().join("served"), tmp.path().join("meta"));
    let m = meta.to_str().unwrap();
    let token = old(&served, &["--meta", m, "access", "user", "alice"]);
    let token_file = tmp.path().join("alice.token");
    std::fs::write(&token_file, token.trim()).unwrap();
    for g in ["read blobs", "list blobs", "read var:main"] {
        old(&served, &["--meta", m, "access", "grant", "*", g]);
    }
    for g in ["write blobs", "write var:main"] {
        old(&served, &["--meta", m, "access", "grant", "alice", g]);
    }
    let shown = old(&served, &["--meta", m, "access", "show"]);
    assert!(shown.contains("user  alice") && shown.contains("grant alice: write blobs, write var:main"), "{shown}");
    assert!(!shown.contains(token.trim()));
    let addr = "127.0.0.1:47391";
    let (cert, key) = cas_core::server::self_signed_pem();
    let (cert_file, key_file) = (tmp.path().join("cert.pem"), tmp.path().join("key.pem"));
    std::fs::write(&cert_file, cert).unwrap();
    std::fs::write(&key_file, key).unwrap();
    let ca = cert_file.to_str().unwrap();
    // Without a certificate there is nothing to serve: HTTPS is the only protocol.
    let no_cert =
        Command::new(env!("CARGO_BIN_EXE_cas")).arg("--store").arg(&served).args(["serve", addr]).output().unwrap();
    assert!(!no_cert.status.success());
    let mut server = Command::new(env!("CARGO_BIN_EXE_cas"))
        .arg("--store")
        .arg(&served)
        .args(["--meta", m, "serve", addr, "--cert", ca, "--key", key_file.to_str().unwrap()])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let url = format!("https://{addr}");
    // Only a benchmark build (feature bench-insecure) speaks plain HTTP.
    #[cfg(not(feature = "bench-insecure"))]
    {
        let plain = Command::new(env!("CARGO_BIN_EXE_cas"))
            .args(["--store", &format!("http://{addr}"), "log"])
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&plain.stderr).contains("https://"));
    }
    let tf = token_file.to_str().unwrap();
    let mut pushed = None;
    for _ in 0..50 {
        let o = Command::new(env!("CARGO_BIN_EXE_cas"))
            .arg("--store")
            .arg(&store)
            .args(["--token-file", tf, "--ca", ca, "push", &url])
            .output()
            .unwrap();
        if o.status.success() {
            pushed = Some(String::from_utf8(o.stdout).unwrap());
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(pushed.unwrap().starts_with("Created"));
    // Without the token the same push is denied.
    std::fs::write(work.join("README"), "changed\n").unwrap();
    old(&store, &["commit", w]);
    let anon = Command::new(env!("CARGO_BIN_EXE_cas"))
        .arg("--store")
        .arg(&store)
        .args(["--ca", ca, "push", &url])
        .output()
        .unwrap();
    assert!(!anon.status.success());
    let remote_log =
        Command::new(env!("CARGO_BIN_EXE_cas")).args(["--store", &url, "--ca", ca, "log"]).output().unwrap();
    server.kill().unwrap();
    server.wait().unwrap();
    assert_eq!(String::from_utf8(remote_log.stdout).unwrap(), log);
}

#[test]
fn private_history_rebase_publish_merge_and_gc() {
    let tmp = tempfile::tempdir().unwrap();
    let (work, store) = (tmp.path().join("work"), tmp.path().join("store"));
    std::fs::create_dir_all(&work).unwrap();
    let w = work.to_str().unwrap();
    let s = store.to_str().unwrap();
    std::fs::write(work.join("notes"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(work.join("other"), "1\n").unwrap();
    old(&store, &["commit", w]);
    old(&store, &["tag", "v1"]);
    // A private history starts as a copy of main.
    assert!(old(&store, &["push", s, "--as", "alon"]).starts_with("Created"));
    std::fs::write(work.join("notes"), "ONE\ntwo\nthree\n").unwrap();
    old(&store, &["commit", w, "-H", "alon"]);
    std::fs::write(work.join("notes"), "ONE\nTWO\nthree\n").unwrap();
    old(&store, &["commit", w, "-H", "alon"]);
    assert_eq!(old(&store, &["log", "-H", "alon"]).lines().count(), 3);

    // Main moves meanwhile: publishing needs a rebase first.
    let main_work = tmp.path().join("main-work");
    old(&store, &["checkout", main_work.to_str().unwrap()]);
    std::fs::write(main_work.join("other"), "2\n").unwrap();
    old(&store, &["commit", main_work.to_str().unwrap(), "--compatible"]);
    let no = Command::new(env!("CARGO_BIN_EXE_cas"))
        .arg("--store")
        .arg(&store)
        .args(["publish", "--as", "main", "-H", "alon"])
        .output()
        .unwrap();
    assert!(!no.status.success());
    assert!(old(&store, &["rebase", "main", "-H", "alon"]).starts_with("Done"));
    let changelog = tmp.path().join("changelog");
    std::fs::write(&changelog, "notes: upper case\n").unwrap();
    let c = changelog.to_str().unwrap();
    assert!(old(&store, &["publish", "--as", "main", "-H", "alon", "--changelog", c]).starts_with("Done"));
    let log = old(&store, &["log"]);
    assert_eq!(log.lines().count(), 3, "{log}");
    assert!(log.lines().next().unwrap().contains("notes: upper case") && log.contains("(v1)"), "{log}");
    assert_eq!(old(&store, &["compatible", "v1", "latest"]).trim(), "breaking");
    assert_eq!(old(&store, &["path-log", "notes"]).lines().count(), 2);
    let out = tmp.path().join("out");
    old(&store, &["checkout", out.to_str().unwrap()]);
    assert_eq!(std::fs::read_to_string(out.join("notes")).unwrap(), "ONE\nTWO\nthree\n");
    assert_eq!(std::fs::read_to_string(out.join("other")).unwrap(), "2\n");

    // A merge: two diverged copies of main, combined as one node.
    assert!(old(&store, &["push", s, "--as", "side"]).starts_with("Created"));
    std::fs::write(out.join("notes"), "ONE\nTWO\nTHREE\n").unwrap();
    old(&store, &["commit", out.to_str().unwrap(), "-H", "side"]);
    std::fs::write(out.join("notes"), "one\nTWO\nthree\n").unwrap();
    old(&store, &["commit", out.to_str().unwrap()]);
    assert!(old(&store, &["merge", "side"]).starts_with("Done"));
    std::fs::remove_dir_all(&out).unwrap();
    old(&store, &["checkout", out.to_str().unwrap()]);
    assert_eq!(std::fs::read_to_string(out.join("notes")).unwrap(), "one\nTWO\nTHREE\n");

    // Collection keeps everything reachable.
    for _ in 0..3 {
        old(&store, &["gc"]);
    }
    std::fs::remove_dir_all(&out).unwrap();
    old(&store, &["checkout", out.to_str().unwrap(), "notes"]);
    assert_eq!(std::fs::read_to_string(out.join("notes")).unwrap(), "one\nTWO\nTHREE\n");
}

#[test]
fn last_modified_merge_base_objects_and_fsck() {
    let tmp = tempfile::tempdir().unwrap();
    let (work, store) = (tmp.path().join("work"), tmp.path().join("store"));
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::write(work.join("README"), "hi\n").unwrap();
    std::fs::write(work.join("src/lib.rs"), "fn a() {}\n").unwrap();
    let w = work.to_str().unwrap();
    let first = old(&store, &["commit", w]);
    std::fs::write(work.join("src/lib.rs"), "fn b() {}\n").unwrap();
    let second = old(&store, &["commit", w]);
    let short = |id: &str| id.split(' ').nth(1).unwrap()[.."blake3:".len() + 12].to_string();
    let (a, b) = (short(&first), short(&second));

    assert_eq!(old(&store, &["last-modified"]), format!("{a}  README\n{b}  src\n"));
    assert_eq!(old(&store, &["last-modified", "src"]), format!("{b}  src/lib.rs\n"));
    assert_eq!(old(&store, &["merge-base", &a, &b]), first);

    let objects = old(&store, &["objects"]);
    assert!(objects.lines().all(|k| k.starts_with("blake3:")));
    assert!(old(&store, &["fsck"]).ends_with(", 0 missing, 0 bad\n"));

    // Lose the blob of the old src/lib.rs: fsck names it and fails.
    let key = blake3::hash(b"fn a() {}\n").to_hex().to_string();
    std::fs::remove_file(store.join("blobs/blake3").join(&key[..2]).join(&key)).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_cas")).arg("--store").arg(&store).arg("fsck").output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains(&format!("missing blake3:{key}")));
}

#[test]
fn status_reset_clean_and_stash() {
    let tmp = tempfile::tempdir().unwrap();
    let (work, store) = (tmp.path().join("work"), tmp.path().join("store"));
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::write(work.join("README"), "hi\n").unwrap();
    std::fs::write(work.join("src/lib.rs"), "fn a() {}\n").unwrap();
    let w = work.to_str().unwrap();
    old(&store, &["commit", w]);
    assert_eq!(old(&store, &["status", w]), "");

    std::fs::write(work.join("src/lib.rs"), "fn b() {}\n").unwrap();
    std::fs::write(work.join("junk"), "junk\n").unwrap();
    std::fs::remove_file(work.join("README")).unwrap();
    assert_eq!(old(&store, &["status", w]), "deleted  README\nadded    junk\nchanged  src/lib.rs\n");

    old(&store, &["clean", w]);
    assert_eq!(old(&store, &["status", w]), "deleted  README\nchanged  src/lib.rs\n");

    old(&store, &["stash", w]);
    assert_eq!(old(&store, &["status", w]), "");
    assert_eq!(std::fs::read_to_string(work.join("README")).unwrap(), "hi\n");
    old(&store, &["stash", "pop", w]);
    assert_eq!(old(&store, &["status", w]), "deleted  README\nchanged  src/lib.rs\n");

    old(&store, &["reset", w]);
    assert_eq!(old(&store, &["status", w]), "");
    assert_eq!(std::fs::read_to_string(work.join("src/lib.rs")).unwrap(), "fn a() {}\n");
}

#[test]
fn a_sparse_checkout_commits_only_its_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let (work, store, part) = (tmp.path().join("work"), tmp.path().join("store"), tmp.path().join("part"));
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::write(work.join("README"), "hi\n").unwrap();
    std::fs::write(work.join("src/lib.rs"), "fn a() {}\n").unwrap();
    old(&store, &["commit", work.to_str().unwrap()]);
    let p = part.to_str().unwrap();
    old(&store, &["sparse", p, "src"]);
    assert!(!part.join("README").exists());
    std::fs::write(part.join("src/lib.rs"), "fn b() {}\n").unwrap();
    assert_eq!(old(&store, &["status", p]), "changed  src/lib.rs\n");
    old(&store, &["commit", p]);
    assert!(old(&store, &["ls"]).contains("README"));
    assert_eq!(old(&store, &["status", p]), "");
}

#[test]
fn diff_and_merge_base_with_the_ancestry_gc_records() {
    let tmp = tempfile::tempdir().unwrap();
    let (work, store) = (tmp.path().join("work"), tmp.path().join("store"));
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("a"), "1\n").unwrap();
    let w = work.to_str().unwrap();
    let first = old(&store, &["commit", w]);
    std::fs::write(work.join("a"), "2\n").unwrap();
    let second = old(&store, &["commit", w]);
    let short = |id: &str| id.split(' ').nth(1).unwrap()["blake3:".len().."blake3:".len() + 12].to_string();
    old(&store, &["gc"]);
    assert_eq!(old(&store, &["diff"]).trim(), "raw      -> raw      a");
    assert_eq!(old(&store, &["merge-base", &short(&first), &short(&second)]), first);
}

/// A history of commits (spec/formats/commit.md): `directory` versions,
/// an author and a time on each commit, notes as an annotation.
#[test]
fn a_history_of_commits() {
    let tmp = tempfile::tempdir().unwrap();
    let (work, store, out) = (tmp.path().join("work"), tmp.path().join("store"), tmp.path().join("out"));
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::write(work.join("README"), "hi\n").unwrap();
    std::fs::write(work.join("src/lib.rs"), "fn a() {}\n").unwrap();
    let w = work.to_str().unwrap();

    let first = cas(&store, &["--author", "Dana", "commit", w]);
    assert!(first.starts_with("blake3:"));
    std::fs::write(work.join("src/lib.rs"), "fn a() {}\nfn b() {}\n").unwrap();
    assert_eq!(cas(&store, &["status", w]), "changed  src/lib.rs\n");

    // Notes are written on the diff to commit: one on the step, one on a line.
    let form = cas(&store, &["status", w, "--diff"]);
    let mut noted = String::from("# add b\n");
    for l in form.lines() {
        if l == "| +fn b() {}" {
            noted.push_str("# b is new\n");
        }
        noted.push_str(l);
        noted.push('\n');
    }
    let notes = tmp.path().join("notes");
    std::fs::write(&notes, noted).unwrap();
    cas(&store, &["--author", "Dana", "commit", w, "--notes", notes.to_str().unwrap()]);

    let log = cas(&store, &["log"]);
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].ends_with("  Dana  add b"), "{log}");
    assert!(lines[1].ends_with("  Dana  "), "{log}");
    assert_eq!(cas(&store, &["diff"]), "raw      -> raw      src/lib.rs\n");
    let shown = cas(&store, &["diff", "--annotated"]);
    assert!(shown.starts_with("# add b\n"), "{shown}");
    assert!(shown.contains("# b is new\n| +fn b() {}\n"), "{shown}");
    assert_eq!(cas(&store, &["ls"]), "raw      README\ndir      src\nraw      src/lib.rs\n");

    // The first commit by the start of its key.
    cas(&store, &["-n", &first.trim()[7..19], "checkout", out.to_str().unwrap()]);
    assert_eq!(std::fs::read_to_string(out.join("src/lib.rs")).unwrap(), "fn a() {}\n");
    assert_eq!(cas(&store, &["status", w]), "");
    assert!(cas(&store, &["fsck"]).contains("0 missing, 0 bad"));

    // A commit history has no tags of its own: a release names a version.
    let o = Command::new(env!("CARGO_BIN_EXE_cas")).arg("--store").arg(&store).args(["tag", "v1"]).output().unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("publish --tag"));
}

/// The rest of the daily commands on a history of commits, which a new
/// history is unless `--nodes` asks otherwise.
#[test]
fn commit_histories_stash_notes_summary_and_last_modified() {
    let tmp = tempfile::tempdir().unwrap();
    let (work, store) = (tmp.path().join("work"), tmp.path().join("store"));
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::write(work.join("README"), "hi\n").unwrap();
    std::fs::write(work.join("src/lib.rs"), "fn a() {}\n").unwrap();
    let w = work.to_str().unwrap();
    let first = cas(&store, &["--author", "Dana", "commit", w]);
    std::fs::write(work.join("src/lib.rs"), "fn a() {}\nfn b() {}\n").unwrap();

    // Notes on a file as it is: one note for each line.
    let form = tmp.path().join("lib.notes");
    std::fs::write(&form, "# the start\n| fn a() {}\n# added later\n| fn b() {}\n").unwrap();
    let annotate = format!("src/lib.rs={}", form.to_str().unwrap());
    let second = cas(&store, &["--author", "Dana", "commit", w, "--annotate", &annotate]);
    assert_eq!(cas(&store, &["notes"]), "src/lib.rs  \n");
    assert_eq!(cas(&store, &["notes", "src/lib.rs"]), std::fs::read_to_string(&form).unwrap());
    // A form whose text is not the file is refused.
    std::fs::write(&form, "| fn c() {}\n").unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_cas"))
        .arg("--store")
        .arg(&store)
        .args(["--author", "Dana", "commit", w, "--annotate", &annotate])
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("its text is not src/lib.rs as committed"));

    // Every hash is shown with the name of its hash directly before it.
    let short = |k: &str| k.trim()[.."blake3:".len() + 12].to_string();
    let (a, b) = (short(&first), short(&second));
    assert_eq!(cas(&store, &["last-modified"]), format!("{a}  README\n{b}  src\n"));
    assert_eq!(cas(&store, &["last-modified", "src"]), format!("{b}  src/lib.rs\n"));
    assert!(cas(&store, &["log"]).starts_with(&format!("{b}  [{a}]  ")));
    assert_eq!(cas(&store, &["summary"]), "1 directories, 2 files\n");

    // Two stashes, put back newest first; the last pop removes stash/main.
    std::fs::remove_file(work.join("README")).unwrap();
    cas(&store, &["--author", "Dana", "stash", w]);
    assert_eq!(cas(&store, &["status", w]), "");
    std::fs::write(work.join("junk"), "junk\n").unwrap();
    cas(&store, &["--author", "Dana", "stash", w]);
    assert_eq!(cas(&store, &["status", w]), "");
    cas(&store, &["stash", "pop", w]);
    assert_eq!(cas(&store, &["status", w]), "added    junk\n");
    cas(&store, &["clean", w]);
    cas(&store, &["stash", "pop", w]);
    assert_eq!(cas(&store, &["status", w]), "deleted  README\n");
    let o =
        Command::new(env!("CARGO_BIN_EXE_cas")).arg("--store").arg(&store).args(["stash", "pop", w]).output().unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("nothing stashed in stash/main"));
    assert!(cas(&store, &["fsck"]).contains("0 missing, 0 bad"));
}

/// Commit histories sync fast-forward only, and merge or rebase what
/// was made side by side.
#[test]
fn commit_histories_push_pull_merge_and_rebase() {
    let tmp = tempfile::tempdir().unwrap();
    let (work, store, other) = (tmp.path().join("work"), tmp.path().join("store"), tmp.path().join("other"));
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("a"), "1\n").unwrap();
    std::fs::write(work.join("b"), "1\n").unwrap();
    let (w, o) = (work.to_str().unwrap(), other.to_str().unwrap());

    cas(&store, &["--author", "Dana", "commit", w]);
    assert!(cas(&store, &["push", o]).starts_with("Created"));
    assert!(cas(&store, &["push", o]).starts_with("UpToDate"));

    // The other store moves on; this one pulls it.
    let work2 = tmp.path().join("work2");
    cas(&other, &["checkout", work2.to_str().unwrap()]);
    std::fs::write(work2.join("a"), "2\n").unwrap();
    cas(&other, &["--author", "Eli", "commit", work2.to_str().unwrap()]);
    assert!(cas(&store, &["pull", o]).starts_with("FastForward"));
    assert_eq!(cas(&store, &["log"]).lines().count(), 2);
    assert_eq!(cas(&store, &["path-log", "a"]).lines().count(), 2);
    assert_eq!(cas(&store, &["path-log", "b"]).lines().count(), 1);

    // Both move: a push is refused, a side history rebases, then merges.
    cas(&store, &["reset", w]);
    std::fs::write(work.join("b"), "3\n").unwrap();
    cas(&store, &["--author", "Dana", "commit", w]);
    std::fs::write(work2.join("a"), "4\n").unwrap();
    cas(&other, &["--author", "Eli", "commit", work2.to_str().unwrap()]);
    let refused =
        Command::new(env!("CARGO_BIN_EXE_cas")).arg("--store").arg(&store).args(["push", o]).output().unwrap();
    assert!(!refused.status.success());
    assert!(cas(&store, &["-H", "theirs", "pull", o, "--as", "main"]).starts_with("Created"));
    let before = cas(&store, &["log"]);
    assert!(cas(&store, &["--author", "Dana", "merge", "theirs"]).starts_with("Done"));
    let merged = cas(&store, &["log"]);
    assert_eq!(merged.lines().count(), 5, "{merged}");
    cas(&store, &["reset", w]);
    assert_eq!(std::fs::read_to_string(work.join("a")).unwrap(), "4\n");
    assert_eq!(std::fs::read_to_string(work.join("b")).unwrap(), "3\n");
    assert!(cas(&store, &["push", o]).starts_with("FastForward"));
    assert!(cas(&store, &["--author", "Dana", "merge", "theirs"]).starts_with("UpToDate"));
    assert!(!cas(&store, &["merge-base", "latest", &before.lines().next().unwrap()[..12]]).is_empty());

    // A rebase replays a side history's commit on top, keeping its author.
    let side = tmp.path().join("side");
    cas(&store, &["-n", &before.lines().nth(1).unwrap()[..12], "checkout", side.to_str().unwrap()]);
    let third = tmp.path().join("third");
    cas(&third, &["--author", "Dana", "commit", side.to_str().unwrap()]);
    std::fs::write(side.join("c"), "new\n").unwrap();
    cas(&third, &["--author", "Fay", "commit", side.to_str().unwrap()]);
    let conflict = Command::new(env!("CARGO_BIN_EXE_cas"))
        .arg("--store")
        .arg(&store)
        .args(["-H", "third", "pull", third.to_str().unwrap(), "--as", "main"])
        .output()
        .unwrap();
    assert!(conflict.status.success());
    // It shares no commit with main (its first commit was made anew).
    let none = Command::new(env!("CARGO_BIN_EXE_cas"))
        .arg("--store")
        .arg(&store)
        .args(["-H", "third", "rebase", "main"])
        .output()
        .unwrap();
    assert!(!none.status.success());
    assert!(cas(&store, &["fsck"]).contains("0 missing, 0 bad"));
}

/// Releases of a commit history: a tag on each, a kind of change and a
/// changelog from the one before.
#[test]
fn releases_have_tags_and_typed_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let (work, store, out) = (tmp.path().join("work"), tmp.path().join("store"), tmp.path().join("out"));
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("a"), "1\n").unwrap();
    let w = work.to_str().unwrap();
    let log = tmp.path().join("changelog");
    let l = log.to_str().unwrap();
    let fails = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_cas")).arg("--store").arg(&store).args(args).output().unwrap();
        assert!(!o.status.success(), "cas {args:?} should fail");
        String::from_utf8_lossy(&o.stderr).to_string()
    };

    cas(&store, &["--author", "Dana", "commit", w]);
    assert!(fails(&["publish"]).contains("--tag"));
    assert!(fails(&["publish", "--tag", "v1"]).contains("semantic version"));
    assert!(cas(&store, &["publish", "--tag", "1.0.0"]).starts_with("1.0.0 "));

    for (text, tag, change) in [("2\n", "1.0.1", "patch"), ("3\n", "1.1.0", "feature"), ("4\n", "2.0.0", "breaking")] {
        std::fs::write(work.join("a"), text).unwrap();
        cas(&store, &["--author", "Dana", "commit", w]);
        std::fs::write(&log, format!("What {tag} changes.\nIn two lines.\n")).unwrap();
        cas(&store, &["publish", "--tag", tag, "--change", change, "--changelog", l]);
    }
    assert!(fails(&["publish", "--tag", "2.0.1"]).contains("changelog"));
    assert!(fails(&["publish", "--tag", "1.1.0", "--change", "patch", "--changelog", l]).contains("already has"));
    assert!(fails(&["publish", "--tag", "2.0.1", "--change", "minor", "--changelog", l]).contains("not one of"));

    let shown = cas(&store, &["-H", "releases", "log"]);
    let lines: Vec<&str> = shown.lines().collect();
    assert_eq!(lines.len(), 7, "{shown}");
    assert!(lines[0].starts_with("2.0.0  "));
    assert_eq!(lines[1], "  breaking from 1.1.0: What 2.0.0 changes. / In two lines.");
    assert_eq!(lines[6].split("  ").next(), Some("1.0.0"));
    assert_eq!(cas(&store, &["-H", "releases", "compatible", "1.0.0", "1.1.0"]), "compatible\n");
    assert_eq!(cas(&store, &["-H", "releases", "compatible", "1.0.0", "2.0.0"]), "breaking\n");
    cas(&store, &["-H", "releases", "-n", "1.0.1", "checkout", out.to_str().unwrap()]);
    assert_eq!(std::fs::read_to_string(out.join("a")).unwrap(), "2\n");
    assert!(fails(&["-H", "releases", "commit", w]).contains("not for a release history"));
    assert!(cas(&store, &["fsck"]).contains("0 missing, 0 bad"));
}

#[test]
fn a_commit_has_a_manifest_and_needs_the_prover_for_a_proof() {
    let tmp = tempfile::tempdir().unwrap();
    let (work, store) = (tmp.path().join("work"), tmp.path().join("store"));
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::write(work.join("src/lib.rs"), "fn a() {}\n").unwrap();
    let w = work.to_str().unwrap();
    let first = cas(&store, &["commit", w, "--author", "Dana"]);
    std::fs::write(work.join("src/lib.rs"), "fn b() {}\n").unwrap();
    let second = cas(&store, &["commit", w, "--author", "Dana"]);

    // The first commit brings itself, its author and both directories.
    let m = cas(&store, &["-n", first.trim(), "manifest"]);
    let kinds: Vec<&str> = m.lines().filter_map(|l| l.split_whitespace().next()).collect();
    assert_eq!(kinds.iter().filter(|k| **k == "commit").count(), 1, "{m}");
    assert_eq!(kinds.iter().filter(|k| **k == "directory").count(), 2, "{m}");
    assert_eq!(kinds.iter().filter(|k| **k == "author").count(), 1, "{m}");
    assert!(m.lines().all(|l| l.contains("blake3:")), "{m}");
    assert!(m.lines().any(|l| l.contains(first.trim())), "{m}");
    // The second brings no author again, and asking twice gives the same.
    let m2 = cas(&store, &["manifest"]);
    assert!(!m2.contains("author"), "{m2}");
    assert!(m2.lines().any(|l| l.contains(second.trim())), "{m2}");
    assert_eq!(m2, cas(&store, &["manifest"]));

    // Without the prover on the path, verify and prove say what is missing.
    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_cas"))
            .env("PATH", "")
            .arg("--store")
            .arg(&store)
            .args(args)
            .output()
            .unwrap();
        (out.status.code(), String::from_utf8(out.stderr).unwrap())
    };
    let (code, err) = run(&["verify", "--all"]);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("has no proof: cas prove makes one"), "{err}");
    let (code, err) = run(&["prove"]);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("the prover is not installed"), "{err}");
}
