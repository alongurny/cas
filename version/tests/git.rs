//! Checks for importing a git repository (spec/git-import.md).

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Policy, Session, Who};
use cas_version::{Node, Path, fs, git, lookup, read_blob};
use std::process::Command;

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn store() -> Session {
    CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous())
}

fn run(dir: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn a_git_history_imports_with_its_shape_tags_and_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let r = tmp.path();
    run(r, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(r.join("src/empty-not-kept")).unwrap();
    std::fs::write(r.join("README"), "hi\n").unwrap();
    std::fs::write(r.join("src/run.sh"), "#!/bin/sh\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(r.join("src/run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    run(r, &["add", "README", "src/run.sh"]);
    run(r, &["commit", "-q", "-m", "first\n\nwith a body"]);
    run(r, &["tag", "-a", "v1", "-m", "release 1"]);
    run(r, &["checkout", "-q", "-b", "side"]);
    std::fs::write(r.join("side.txt"), "s\n").unwrap();
    std::os::unix::fs::symlink("README", r.join("link")).unwrap();
    run(r, &["add", "side.txt", "link"]);
    run(r, &["commit", "-q", "-m", "side"]);
    run(r, &["checkout", "-q", "main"]);
    std::fs::write(r.join("README"), "hello\n").unwrap();
    run(r, &["commit", "-q", "-am", "main edit"]);
    run(r, &["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
    run(r, &["commit", "-q", "--allow-empty", "-m", "empty"]);
    run(r, &["tag", "light"]);

    let s = store();
    let (h, n) = git::import(&s, b3(), r, "HEAD").unwrap();
    assert_eq!(n.commits, 5);
    assert_eq!(h.nodes.len(), 5);
    let tip = h.tip().unwrap();
    // The empty commit repeats its parent's version.
    assert!(tip.as_str().ends_with("#2"), "{tip}");
    let merge = h.nodes[&tip].after.keys().next().unwrap().clone();
    assert_eq!(h.nodes[&merge].after.len(), 2);
    assert!(h.nodes[&merge].after.values().all(|s| s.changelog.as_deref() == Some("merge side")));
    let root = h.log().pop().unwrap();
    assert_eq!(h.nodes[&root].doc.as_deref(), Some("first\n\nwith a body"));
    assert_eq!(h.tags["v1"], root);
    assert_eq!(h.tags["light"], tip);

    // The tip's version is the working tree: formats follow git modes.
    let v = tip.version();
    let at = |p: &str| match lookup(&s, &v, &Path::new(p).unwrap()).unwrap() {
        Some(Node::File(f)) => {
            (f.known_format().unwrap().alias, String::from_utf8(read_blob(&s, f.key()).unwrap()).unwrap())
        }
        other => panic!("{p}: {other:?}"),
    };
    assert_eq!(at("README"), ("raw", "hello\n".into()));
    assert_eq!(at("src/run.sh"), ("exec", "#!/bin/sh\n".into()));
    assert_eq!(at("link"), ("symlink", "README".into()));
    assert_eq!(at("side.txt"), ("raw", "s\n".into()));
    let out = tmp.path().join("out");
    assert_eq!(fs::checkout(&s, &v, None, &out).unwrap(), 5);
}
