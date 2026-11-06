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
    assert!(lines[0].contains("second") && lines[1].contains("first"), "{log}");

    assert_eq!(cas(&store, &["diff"]).trim(), "raw      -> raw      src/lib.rs");
    assert_eq!(cas(&store, &["summary"]).trim(), "{'dirs': '2', 'files': '2'}");
    assert_eq!(cas(&store, &["path-log", "src"]).lines().count(), 2);
    assert_eq!(cas(&store, &["path-log", "README"]).lines().count(), 1);
    assert!(cas(&store, &["ls"]).contains("dir      empty"));

    let out = tmp.path().join("out");
    cas(&store, &["checkout", out.to_str().unwrap(), "src"]);
    assert_eq!(std::fs::read_to_string(out.join("src/lib.rs")).unwrap(), "fn b() {}\n");
    assert!(!out.join("README").exists());

    // Sync to a second store directory, then over HTTP to a third.
    assert!(cas(&store, &["push", other.to_str().unwrap()]).starts_with("Created"));
    assert!(cas(&store, &["push", other.to_str().unwrap()]).starts_with("UpToDate"));
    assert_eq!(cas(&other, &["log"]), log);

    // Serve a third store under an access policy: anyone reads, alice writes.
    let (served, meta) = (tmp.path().join("served"), tmp.path().join("meta"));
    let m = meta.to_str().unwrap();
    let token = cas(&served, &["--meta", m, "access", "user", "alice"]);
    let token_file = tmp.path().join("alice.token");
    std::fs::write(&token_file, token.trim()).unwrap();
    for g in ["read blobs", "list blobs", "read tag:main"] {
        cas(&served, &["--meta", m, "access", "grant", "*", g]);
    }
    for g in ["write blobs", "write tag:main"] {
        cas(&served, &["--meta", m, "access", "grant", "alice", g]);
    }
    let shown = cas(&served, &["--meta", m, "access", "show"]);
    assert!(shown.contains("user  alice") && shown.contains("grant alice: write blobs, write tag:main"), "{shown}");
    assert!(!shown.contains(token.trim()));
    let addr = "127.0.0.1:47391";
    let mut server = Command::new(env!("CARGO_BIN_EXE_cas"))
        .arg("--store")
        .arg(&served)
        .args(["--meta", m, "serve", addr])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let url = format!("http://{addr}");
    let tf = token_file.to_str().unwrap();
    let mut pushed = None;
    for _ in 0..50 {
        let o = Command::new(env!("CARGO_BIN_EXE_cas"))
            .arg("--store")
            .arg(&store)
            .args(["--token-file", tf, "push", &url])
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
    let anon =
        Command::new(env!("CARGO_BIN_EXE_cas")).arg("--store").arg(&store).args(["push", &url]).output().unwrap();
    assert!(!anon.status.success());
    let remote_log = Command::new(env!("CARGO_BIN_EXE_cas")).args(["--store", &url, "log"]).output().unwrap();
    server.kill().unwrap();
    server.wait().unwrap();
    assert_eq!(String::from_utf8(remote_log.stdout).unwrap(), log);
}

#[test]
fn branch_merge_gc_and_purge() {
    let tmp = tempfile::tempdir().unwrap();
    let (work, store) = (tmp.path().join("work"), tmp.path().join("store"));
    std::fs::create_dir_all(&work).unwrap();
    let w = work.to_str().unwrap();
    std::fs::write(work.join("notes"), "one\ntwo\nthree\n").unwrap();
    cas(&store, &["commit", w, "-m", "base"]);
    cas(&store, &["branch", "feature"]);
    std::fs::write(work.join("notes"), "ONE\ntwo\nthree\n").unwrap();
    cas(&store, &["commit", w, "-m", "ours"]);
    std::fs::write(work.join("notes"), "one\ntwo\nTHREE\n").unwrap();
    std::fs::write(work.join("secret"), "hunter2\n").unwrap();
    cas(&store, &["commit", w, "-b", "feature", "-m", "theirs"]);

    assert!(cas(&store, &["merge", "feature"]).starts_with("Merged"));
    assert!(cas(&store, &["merge", "feature"]).starts_with("UpToDate"));
    let out = tmp.path().join("out");
    cas(&store, &["checkout", out.to_str().unwrap()]);
    assert_eq!(std::fs::read_to_string(out.join("notes")).unwrap(), "ONE\ntwo\nTHREE\n");
    let log = cas(&store, &["log"]);
    assert_eq!(log.lines().count(), 4, "{log}");

    // Purge the secret: the tree keeps the path, the content is gone.
    let key = format!("blake3:{}", blake3_hex(b"hunter2\n"));
    cas(&store, &["purge", &key, "leaked"]);
    assert!(cas(&store, &["ls"]).contains("secret"));
    let shown =
        Command::new(env!("CARGO_BIN_EXE_cas")).arg("--store").arg(&store).args(["show", &key]).output().unwrap();
    assert!(String::from_utf8_lossy(&shown.stderr).contains("purged"));

    // Collection keeps everything reachable.
    for _ in 0..3 {
        cas(&store, &["gc"]);
    }
    std::fs::remove_dir_all(&out).unwrap();
    cas(&store, &["checkout", out.to_str().unwrap(), "notes"]);
    assert_eq!(std::fs::read_to_string(out.join("notes")).unwrap(), "ONE\ntwo\nTHREE\n");
}

/// The blake3 hex digest the store names raw files by.
fn blake3_hex(bytes: &[u8]) -> String {
    cas_core::algo::lookup("blake3").unwrap().key(bytes).split_once(':').unwrap().1.to_string()
}
