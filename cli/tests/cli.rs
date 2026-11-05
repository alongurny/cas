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

    let served = tmp.path().join("served");
    let addr = "127.0.0.1:47391";
    let mut server = Command::new(env!("CARGO_BIN_EXE_cas"))
        .arg("--store")
        .arg(&served)
        .args(["serve", addr])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let url = format!("http://{addr}");
    let mut pushed = None;
    for _ in 0..50 {
        let o =
            Command::new(env!("CARGO_BIN_EXE_cas")).arg("--store").arg(&store).args(["push", &url]).output().unwrap();
        if o.status.success() {
            pushed = Some(String::from_utf8(o.stdout).unwrap());
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(pushed.unwrap().starts_with("Created"));
    let remote_log = Command::new(env!("CARGO_BIN_EXE_cas")).args(["--store", &url, "log"]).output().unwrap();
    server.kill().unwrap();
    assert_eq!(String::from_utf8(remote_log.stdout).unwrap(), log);
}
