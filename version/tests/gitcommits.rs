//! Git commits to and from commits (spec/formats/git-commits.md): what a
//! git push brings becomes commits, and commits are served as git commits,
//! with git's own ids kept where a commit came from git.

use cas_core::algo::{self, Algorithm};
use cas_core::mem::MemBackend;
use cas_core::{AllowAll, CoreStore, Policy, Session, Who};
use cas_version::commits::{self, Commit};
use cas_version::gitcommits::{self, Links};
use cas_version::{Edit, Node, Path, gitobj, put, update};
use std::collections::BTreeMap;
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
        .args(["-c", "user.name=Dana", "-c", "user.email=dana@example.invalid", "-c", "commit.gpgsign=false"])
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

/// Two commits on main and a merge of a side branch, with an executable,
/// a symbolic link and a subdirectory.
fn repo(r: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    git(r, &["init", "-q", "-b", "main"], None);
    std::fs::create_dir_all(r.join("src")).unwrap();
    std::fs::write(r.join("README"), "hi\n").unwrap();
    std::fs::write(r.join("src/run.sh"), "#!/bin/sh\necho run\n").unwrap();
    std::fs::set_permissions(r.join("src/run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("README", r.join("link")).unwrap();
    git(r, &["add", "-A"], None);
    git(r, &["commit", "-q", "-m", "first\n\nwith a body"], None);
    git(r, &["checkout", "-q", "-b", "side"], None);
    std::fs::write(r.join("src/side.txt"), "side\n").unwrap();
    git(r, &["add", "-A"], None);
    git(r, &["commit", "-q", "-m", "on the side"], None);
    git(r, &["checkout", "-q", "main"], None);
    std::fs::write(r.join("README"), "hi again\n").unwrap();
    git(r, &["commit", "-q", "-am", "second"], None);
    git(r, &["merge", "-q", "--no-ff", "-m", "merge side", "side"], None);
}

#[test]
fn git_commits_become_commits_and_keep_their_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let r = tmp.path().join("r");
    std::fs::create_dir(&r).unwrap();
    repo(&r);
    let s = store();
    gitobj::unpack(&s, &git(&r, &["pack-objects", "--all", "--stdout"], Some(b""))).unwrap();
    let head = text(git(&r, &["rev-parse", "HEAD"], None));

    let mut links = Links::new();
    let tip = gitcommits::from_git(&s, b3(), &mut links, &head, &[]).unwrap();
    assert_eq!(links.len(), 4);
    assert_eq!(links[&tip], head);
    let log = commits::log(&s, &tip).unwrap();
    assert_eq!(log.len(), 4);
    // The merge has two parents; its message notes the step from the first.
    let merge = &log[0].1;
    assert_eq!(merge.parents.len(), 2);
    assert_eq!(commits::line(&s, merge).unwrap(), "merge side");
    assert_eq!(merge.parents.values().filter(|n| n.is_some()).count(), 1);
    // Author, time and a root commit's message are kept.
    let root = &log.last().unwrap().1;
    assert_eq!(commits::text(&s, &root.author).unwrap(), "Dana <dana@example.invalid>");
    assert_eq!(commits::line(&s, root).unwrap(), "first / with a body");
    assert_eq!(root.time.to_string(), text(git(&r, &["log", "--format=%ct", "--max-parents=0"], None)));
    // Asking again makes nothing new, in either direction.
    assert_eq!(gitcommits::from_git(&s, b3(), &mut links.clone(), &head, &[]).unwrap(), tip);
    assert_eq!(gitcommits::to_git(&s, &tip, &mut links, &[]).unwrap(), 0);

    // A commit made in cas gets a git commit on top of git's own.
    let files = BTreeMap::from([(
        Path::new("README").unwrap(),
        Edit::Set(Node::File(put(&s, b3(), "raw", b"changed in cas\n").unwrap())),
    )]);
    let v = update(&s, b3(), &commits::view("directory", &merge.version), &files).unwrap();
    let author = commits::put_text(&s, b3(), "author", "Noa").unwrap();
    let c = Commit {
        version: v.key().to_string(),
        author,
        notes: None,
        time: 1_800_000_000,
        parents: BTreeMap::from([(tip.clone(), None)]),
    };
    let key = commits::write(&s, b3(), &c).unwrap();
    assert_eq!(gitcommits::to_git(&s, &key, &mut links, &[]).unwrap(), 1);
    let g = links[&key].clone();
    let ids = gitobj::closure(&s, std::slice::from_ref(&g), &[]).unwrap();
    let out = tmp.path().join("out");
    std::fs::create_dir(&out).unwrap();
    git(&out, &["init", "-q", "--bare"], None);
    git(&out, &["index-pack", "--stdin"], Some(&gitobj::pack(&s, &ids).unwrap()));
    git(&out, &["update-ref", "refs/heads/main", &g], None);
    git(&out, &["fsck", "--strict", "--no-dangling"], None);
    assert_eq!(text(git(&out, &["rev-parse", "main~1"], None)), head);
    assert_eq!(text(git(&out, &["show", "main:README"], None)), "changed in cas");
    assert_eq!(
        text(git(&out, &["log", "-1", "--format=%an <%ae>|%ct|%s", "main"], None)),
        "Noa <>|1800000000|(no notes)"
    );
    // The same commit always gives the same git commit.
    let mut again = Links::new();
    gitcommits::to_git(&s, &key, &mut again, &[&links]).unwrap();
    assert_eq!(again, links);

    // The links are a blob that names git's commits.
    let lv = gitcommits::write_links(&s, b3(), &links).unwrap();
    assert_eq!(gitcommits::read_links(&s, &lv).unwrap(), links);
}

#[test]
fn histories_that_share_git_commits_share_commits() {
    let tmp = tempfile::tempdir().unwrap();
    let r = tmp.path().join("r");
    std::fs::create_dir(&r).unwrap();
    repo(&r);
    let s = store();
    gitobj::unpack(&s, &git(&r, &["pack-objects", "--all", "--stdout"], Some(b""))).unwrap();
    let (main, side) = (text(git(&r, &["rev-parse", "main"], None)), text(git(&r, &["rev-parse", "side"], None)));
    let mut of_main = Links::new();
    let tip = gitcommits::from_git(&s, b3(), &mut of_main, &main, &[]).unwrap();
    // The side branch is part of main: its history takes main's commits.
    let mut of_side = Links::new();
    let side_tip = gitcommits::from_git(&s, b3(), &mut of_side, &side, &[&of_main]).unwrap();
    assert_eq!(of_side.len(), 2);
    assert!(of_side.iter().all(|(k, g)| of_main.get(k) == Some(g)));
    assert!(commits::reaches(&s, &tip, &side_tip).unwrap());
}
