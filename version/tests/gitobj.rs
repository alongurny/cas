//! Git's own objects in cas (spec/formats/git-*.md): packs in and out,
//! the objects' ids kept, gc keeping what a variable reaches, and
//! conversion to and from cas histories.

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Expect, Policy, Session, Store, Who};
use cas_version::gitobj;
use std::collections::BTreeSet;
use std::io::Write;
use std::process::{Command, Stdio};

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn store() -> Session {
    CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous())
}

fn git(dir: &std::path::Path, args: &[&str], stdin: Option<&[u8]>) -> Vec<u8> {
    let mut c = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut w = c.stdin.take().unwrap();
    if let Some(b) = stdin {
        w.write_all(b).unwrap();
    }
    drop(w);
    let out = c.wait_with_output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

fn text(b: Vec<u8>) -> String {
    String::from_utf8(b).unwrap().trim().to_string()
}

/// A small repository: two commits on main, a merge from a side branch,
/// an executable, a symlink, a subdirectory, and an annotated tag.
fn repo(r: &std::path::Path) {
    git(r, &["init", "-q", "-b", "main"], None);
    std::fs::create_dir_all(r.join("src")).unwrap();
    std::fs::write(r.join("README"), "hi\n").unwrap();
    std::fs::write(r.join("src/run.sh"), "#!/bin/sh\necho run\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(r.join("src/run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("README", r.join("link")).unwrap();
    git(r, &["add", "-A"], None);
    git(r, &["commit", "-q", "-m", "first"], None);
    git(r, &["tag", "-a", "v1", "-m", "release 1"], None);
    git(r, &["checkout", "-q", "-b", "side"], None);
    std::fs::write(r.join("src/side.txt"), "side\n").unwrap();
    git(r, &["add", "-A"], None);
    git(r, &["commit", "-q", "-m", "side"], None);
    git(r, &["checkout", "-q", "main"], None);
    std::fs::write(r.join("README"), "hi\nthere\n").unwrap();
    git(r, &["commit", "-q", "-am", "second"], None);
    git(r, &["merge", "-q", "--no-ff", "-m", "merge side", "side"], None);
}

#[test]
fn packs_keep_git_ids_both_ways() {
    let tmp = tempfile::tempdir().unwrap();
    let r = tmp.path().join("r");
    std::fs::create_dir(&r).unwrap();
    repo(&r);
    let s = store();
    let pack = git(&r, &["pack-objects", "--all", "--stdout", "--delta-base-offset"], Some(b""));
    let views = gitobj::unpack(&s, &pack).unwrap();
    let ids: BTreeSet<String> = views.iter().map(|v| gitobj::id_of(v).unwrap().to_string()).collect();
    let all: BTreeSet<String> =
        text(git(&r, &["rev-list", "--objects", "--all"], None)).lines().map(|l| l[..40].to_string()).collect();
    assert_eq!(ids, all, "every object came across under its own id");
    // Each is a known format whose children are what git says.
    let head = text(git(&r, &["rev-parse", "HEAD"], None));
    let tag = text(git(&r, &["rev-parse", "v1"], None));
    // What a clone of main and the tag needs is everything.
    let want: BTreeSet<String> = gitobj::closure(&s, &[head.clone(), tag.clone()], &[]).unwrap().into_iter().collect();
    let reach: BTreeSet<String> = text(git(&r, &["rev-list", "--objects", "main", "v1"], None))
        .lines()
        .map(|l| l[..40].to_string())
        .chain([tag.clone()])
        .collect();
    assert_eq!(want, reach);
    // A pack of them is one git accepts, with nothing missing.
    let out = tmp.path().join("out");
    std::fs::create_dir(&out).unwrap();
    git(&out, &["init", "-q", "--bare"], None);
    git(&out, &["index-pack", "--stdin"], Some(&gitobj::pack(&s, &want.iter().cloned().collect::<Vec<_>>()).unwrap()));
    git(&out, &["update-ref", "refs/heads/main", &head], None);
    git(&out, &["fsck", "--strict", "--no-dangling"], None);
    // Someone who has the first commit gets only what is new.
    let first = text(git(&r, &["rev-parse", "v1^{commit}"], None));
    let more = gitobj::closure(&s, std::slice::from_ref(&head), std::slice::from_ref(&first)).unwrap();
    let new: BTreeSet<String> =
        text(git(&r, &["rev-list", "--objects", "main", "^v1"], None)).lines().map(|l| l[..40].to_string()).collect();
    let more: BTreeSet<String> = more.into_iter().collect();
    assert!(more.is_superset(&new) && !more.contains(&first), "{more:?} vs {new:?}");
    // gc keeps what a variable reaches through the git formats, and only that.
    s.write("refs", gitobj::view("commit", &head).to_string().as_bytes(), &Expect::Absent).unwrap();
    let junk = gitobj::put(&s, "blob", b"nobody names me\n").unwrap();
    // A tag object is kept since a variable's git-tags names it.
    let tags: gitobj::Tags = [("v1".to_string(), tag.clone())].into_iter().collect();
    let tv = gitobj::write_tags(&s, b3(), &tags).unwrap();
    s.write("tags", tv.to_string().as_bytes(), &Expect::Absent).unwrap();
    assert_eq!(gitobj::read_tags(&s, &tv).unwrap(), tags);
    cas_version::gc::collect(&s, b3()).unwrap();
    cas_version::gc::collect(&s, b3()).unwrap();
    for id in &reach {
        assert!(gitobj::has(&s, id).unwrap(), "{id} kept");
    }
    assert!(!gitobj::has(&s, &junk).unwrap(), "an unreachable object is collected");
    assert!(gitobj::has(&s, &tag).unwrap(), "the tag object is kept");
    assert_eq!(
        gitobj::peel_tag(&s, &tag).as_deref(),
        Some(text(git(&r, &["rev-parse", "v1^{commit}"], None)).as_str())
    );
}
