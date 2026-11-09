//! End to end: the `cas` binary through every stage.

use std::path::Path;
use std::process::{Command, Stdio};

fn cas(store: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_cas")).arg("--store").arg(store).args(args).output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(out.status.success(), "cas {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    text
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

    cas(&store, &["commit", w, "-m", "first"]);
    std::fs::write(work.join("src/lib.rs"), "fn b() {}\n").unwrap();
    cas(&store, &["commit", w, "-m", "second"]);

    let log = cas(&store, &["log"]);
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("second") && !lines[1].contains("first"), "{log}");

    assert_eq!(cas(&store, &["diff"]).trim(), "raw      -> raw      src/lib.rs");
    assert_eq!(cas(&store, &["summary"]).trim(), "{'dirs': '2', 'files': '2'}");
    assert_eq!(cas(&store, &["path-log", "src"]).lines().count(), 2);
    assert_eq!(cas(&store, &["path-log", "README"]).lines().count(), 1);
    assert!(cas(&store, &["ls"]).contains("dir      empty"));

    let out = tmp.path().join("out");
    cas(&store, &["checkout", out.to_str().unwrap(), "src"]);
    assert_eq!(std::fs::read_to_string(out.join("src/lib.rs")).unwrap(), "fn b() {}\n");
    assert!(!out.join("README").exists());

    // Sync to a second store directory, then over HTTPS to a third.
    assert!(cas(&store, &["push", other.to_str().unwrap()]).starts_with("Created"));
    assert!(cas(&store, &["push", other.to_str().unwrap()]).starts_with("UpToDate"));
    assert_eq!(cas(&other, &["log"]), log);

    // Serve a third store under an access policy: anyone reads, alice writes.
    let (served, meta) = (tmp.path().join("served"), tmp.path().join("meta"));
    let m = meta.to_str().unwrap();
    let token = cas(&served, &["--meta", m, "access", "user", "alice"]);
    let token_file = tmp.path().join("alice.token");
    std::fs::write(&token_file, token.trim()).unwrap();
    for g in ["read blobs", "list blobs", "read var:main"] {
        cas(&served, &["--meta", m, "access", "grant", "*", g]);
    }
    for g in ["write blobs", "write var:main"] {
        cas(&served, &["--meta", m, "access", "grant", "alice", g]);
    }
    let shown = cas(&served, &["--meta", m, "access", "show"]);
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
    cas(&store, &["commit", w, "-m", "third"]);
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
    cas(&store, &["commit", w, "--doc", "the first version"]);
    cas(&store, &["tag", "v1"]);
    // A private history starts as a copy of main.
    assert!(cas(&store, &["push", s, "--as", "alon"]).starts_with("Created"));
    std::fs::write(work.join("notes"), "ONE\ntwo\nthree\n").unwrap();
    cas(&store, &["commit", w, "-H", "alon", "-m", "wip 1"]);
    std::fs::write(work.join("notes"), "ONE\nTWO\nthree\n").unwrap();
    cas(&store, &["commit", w, "-H", "alon", "-m", "wip 2"]);
    assert_eq!(cas(&store, &["log", "-H", "alon"]).lines().count(), 3);

    // Main moves meanwhile: publishing needs a rebase first.
    let main_work = tmp.path().join("main-work");
    cas(&store, &["checkout", main_work.to_str().unwrap()]);
    std::fs::write(main_work.join("other"), "2\n").unwrap();
    cas(&store, &["commit", main_work.to_str().unwrap(), "-m", "other: 2", "--compatible"]);
    let no = Command::new(env!("CARGO_BIN_EXE_cas"))
        .arg("--store")
        .arg(&store)
        .args(["publish", "main", "-H", "alon"])
        .output()
        .unwrap();
    assert!(!no.status.success());
    assert!(cas(&store, &["rebase", "main", "-H", "alon"]).starts_with("Done"));
    assert!(cas(&store, &["publish", "main", "-H", "alon", "-m", "notes: upper case"]).starts_with("Done"));
    let log = cas(&store, &["log"]);
    assert_eq!(log.lines().count(), 3, "{log}");
    assert!(log.lines().next().unwrap().contains("notes: upper case") && log.contains("(v1)"), "{log}");
    assert_eq!(cas(&store, &["compatible", "v1", "latest"]).trim(), "breaking");
    assert_eq!(cas(&store, &["path-log", "notes"]).lines().count(), 2);
    let out = tmp.path().join("out");
    cas(&store, &["checkout", out.to_str().unwrap()]);
    assert_eq!(std::fs::read_to_string(out.join("notes")).unwrap(), "ONE\nTWO\nthree\n");
    assert_eq!(std::fs::read_to_string(out.join("other")).unwrap(), "2\n");

    // A merge: two diverged copies of main, combined as one node.
    assert!(cas(&store, &["push", s, "--as", "side"]).starts_with("Created"));
    std::fs::write(out.join("notes"), "ONE\nTWO\nTHREE\n").unwrap();
    cas(&store, &["commit", out.to_str().unwrap(), "-H", "side"]);
    std::fs::write(out.join("notes"), "one\nTWO\nthree\n").unwrap();
    cas(&store, &["commit", out.to_str().unwrap()]);
    assert!(cas(&store, &["merge", "side", "-m", "merge side"]).starts_with("Done"));
    std::fs::remove_dir_all(&out).unwrap();
    cas(&store, &["checkout", out.to_str().unwrap()]);
    assert_eq!(std::fs::read_to_string(out.join("notes")).unwrap(), "one\nTWO\nTHREE\n");

    // Collection keeps everything reachable.
    for _ in 0..3 {
        cas(&store, &["gc"]);
    }
    std::fs::remove_dir_all(&out).unwrap();
    cas(&store, &["checkout", out.to_str().unwrap(), "notes"]);
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
    let first = cas(&store, &["commit", w, "-m", "first"]);
    std::fs::write(work.join("src/lib.rs"), "fn b() {}\n").unwrap();
    let second = cas(&store, &["commit", w, "-m", "second"]);
    let short = |id: &str| id.split(' ').nth(1).unwrap()["blake3:".len().."blake3:".len() + 12].to_string();
    let (a, b) = (short(&first), short(&second));

    assert_eq!(cas(&store, &["last-modified"]), format!("{a}  README\n{b}  src\n"));
    assert_eq!(cas(&store, &["last-modified", "src"]), format!("{b}  src/lib.rs\n"));
    assert_eq!(cas(&store, &["merge-base", &a, &b]), first);

    let objects = cas(&store, &["objects"]);
    assert!(objects.lines().all(|k| k.starts_with("blake3:")));
    assert!(cas(&store, &["fsck"]).ends_with(", 0 missing, 0 bad\n"));

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
    cas(&store, &["commit", w, "-m", "first"]);
    assert_eq!(cas(&store, &["status", w]), "");

    std::fs::write(work.join("src/lib.rs"), "fn b() {}\n").unwrap();
    std::fs::write(work.join("junk"), "junk\n").unwrap();
    std::fs::remove_file(work.join("README")).unwrap();
    assert_eq!(cas(&store, &["status", w]), "deleted  README\nadded    junk\nchanged  src/lib.rs\n");

    cas(&store, &["clean", w]);
    assert_eq!(cas(&store, &["status", w]), "deleted  README\nchanged  src/lib.rs\n");

    cas(&store, &["stash", w]);
    assert_eq!(cas(&store, &["status", w]), "");
    assert_eq!(std::fs::read_to_string(work.join("README")).unwrap(), "hi\n");
    cas(&store, &["stash", "pop", w]);
    assert_eq!(cas(&store, &["status", w]), "deleted  README\nchanged  src/lib.rs\n");

    cas(&store, &["reset", w]);
    assert_eq!(cas(&store, &["status", w]), "");
    assert_eq!(std::fs::read_to_string(work.join("src/lib.rs")).unwrap(), "fn a() {}\n");
}

#[test]
fn a_sparse_checkout_commits_only_its_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let (work, store, part) = (tmp.path().join("work"), tmp.path().join("store"), tmp.path().join("part"));
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::write(work.join("README"), "hi\n").unwrap();
    std::fs::write(work.join("src/lib.rs"), "fn a() {}\n").unwrap();
    cas(&store, &["commit", work.to_str().unwrap(), "-m", "first"]);
    let p = part.to_str().unwrap();
    cas(&store, &["sparse", p, "src"]);
    assert!(!part.join("README").exists());
    std::fs::write(part.join("src/lib.rs"), "fn b() {}\n").unwrap();
    assert_eq!(cas(&store, &["status", p]), "changed  src/lib.rs\n");
    cas(&store, &["commit", p, "-m", "second"]);
    assert!(cas(&store, &["ls"]).contains("README"));
    assert_eq!(cas(&store, &["status", p]), "");
}
