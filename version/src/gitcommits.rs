//! Git commits to and from commits (spec/formats/git-commits.md): what the
//! forge's git endpoint serves and takes. A history keeps its links, which
//! say which git commit each of its commits is, so a commit that came from
//! git is served as git's own object and keeps its id.

use crate::commits::{self, Commit, Converter, Notes};
use crate::gitobj::{self, TreeWriter, VersionWriter};
use crate::uni::Value;
use crate::{Error, Result, View, malformed, read_blob};
use cas_core::Store;
use cas_core::algo::Algorithm;
use std::collections::{BTreeMap, HashMap};

/// The git commit id of each commit, by the commit's key.
pub type Links = BTreeMap<String, String>;

fn is_hex40(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn write_links(store: &dyn Store, alg: &'static Algorithm, links: &Links) -> Result<View> {
    let v = Value::map(links.iter().map(|(k, c)| (Value::str(k.as_str()), Value::str(c.as_str()))));
    crate::put(store, alg, "git-commits", &v.serialize())
}

pub fn read_links(store: &dyn Store, v: &View) -> Result<Links> {
    let bytes = read_blob(store, v.key())?;
    check_links(&bytes).map_err(|e| malformed(v.key(), e))?;
    let val = Value::deserialize(&bytes).map_err(|e| malformed(v.key(), e))?;
    let map = val.as_map().expect("checked");
    Ok(map.iter().map(|(k, c)| (k.as_str().expect("checked").into(), c.as_str().expect("checked").into())).collect())
}

pub(crate) fn check_links(b: &[u8]) -> std::result::Result<(), String> {
    let v = Value::deserialize(b)?;
    for (k, c) in v.as_map().ok_or("not a map")? {
        let ok = k.as_str().is_some_and(|k| crate::typed::key_bytes(k).is_some()) && c.as_str().is_some_and(is_hex40);
        if !ok {
            return Err("a link is a commit's key and a git commit id".into());
        }
    }
    Ok(())
}

pub(crate) fn links_children(b: &[u8]) -> Vec<View> {
    let Ok(v) = Value::deserialize(b) else { return Vec::new() };
    v.as_map()
        .map(|m| {
            m.values().filter_map(|c| c.as_str().filter(|c| is_hex40(c)).map(|c| gitobj::view("commit", c))).collect()
        })
        .unwrap_or_default()
}

/// The commits `tip` reaches that `links` lacks, parents before children.
fn unlinked(store: &dyn Store, tip: &str, links: &Links) -> Result<Vec<(String, Commit)>> {
    let mut out = Vec::new();
    let mut seen: HashMap<String, bool> = HashMap::new();
    let mut stack = vec![(tip.to_string(), false)];
    let mut read: HashMap<String, Commit> = HashMap::new();
    while let Some((k, done)) = stack.pop() {
        if done {
            let c = read.remove(&k).expect("read when first met");
            out.push((k, c));
            continue;
        }
        if links.contains_key(&k) || seen.contains_key(&k) {
            continue;
        }
        seen.insert(k.clone(), true);
        let c = commits::read(store, &k)?;
        stack.push((k.clone(), true));
        for p in c.parents.keys().rev() {
            stack.push((p.clone(), false));
        }
        read.insert(k, c);
    }
    Ok(out)
}

/// Take from `other` the git commit of `key` and of every commit before
/// it that `links` lacks. A commit `links` has must be the same git commit.
fn graft(store: &dyn Store, links: &mut Links, other: &Links, key: &str) -> Result<()> {
    for (k, _) in unlinked(store, key, links)? {
        let c = other.get(&k).ok_or_else(|| Error::Missing(format!("git commit of commit {k}")))?;
        links.insert(k, c.clone());
    }
    Ok(())
}

/// Give `tip` and every commit before it that `links` lacks a git commit:
/// its version's tree, its parents' git commits, its author and time, and
/// what it says in one line as the message. The same commit always gives
/// the same git commit, and a commit that one of `others` links already,
/// with the same git parents, takes that git commit: a commit two
/// histories share is one git commit. How many were made.
pub fn to_git(store: &dyn Store, tip: &str, links: &mut Links, others: &[&Links]) -> Result<usize> {
    let mut tw = TreeWriter::new(store);
    let mut made = 0;
    for (key, c) in unlinked(store, tip, links)? {
        let shared = others.iter().find_map(|o| {
            let g = o.get(&key)?;
            c.parents.keys().all(|p| links.get(p).is_some() && links.get(p) == o.get(p)).then(|| g.clone())
        });
        if let Some(g) = shared {
            links.insert(key, g);
            continue;
        }
        let tree = tw.tree(&commits::view("directory", &c.version))?;
        let mut body = format!("tree {tree}\n");
        for p in c.parents.keys() {
            let pc = links.get(p).ok_or_else(|| Error::Missing(format!("git commit of commit {p}")))?;
            body.push_str(&format!("parent {pc}\n"));
        }
        let who = signature(&commits::text(store, &c.author)?);
        body.push_str(&format!("author {who} {} +0000\ncommitter {who} {} +0000\n\n", c.time, c.time));
        let msg = commits::line(store, &c)?;
        body.push_str(if msg.is_empty() { "(no notes)" } else { msg.trim_end() });
        body.push('\n');
        links.insert(key, tw.put_commit(body.as_bytes())?);
        made += 1;
    }
    tw.flush()?;
    Ok(made)
}

/// An author as git writes one: `name <email>`. An author with no email
/// gets an empty one.
fn signature(author: &str) -> String {
    let clean: String = author.chars().filter(|c| !matches!(c, '\n' | '\0')).collect();
    match clean.rsplit_once(" <") {
        Some((name, mail))
            if mail.ends_with('>') && !name.contains(['<', '>']) && !mail[..mail.len() - 1].contains(['<', '>']) =>
        {
            clean
        }
        _ => format!("{} <>", clean.replace(['<', '>'], "").trim()),
    }
}

/// What a git commit says: its tree, its parents, its author as
/// `name <email>`, its committer's time and its message.
struct GitCommit {
    tree: String,
    parents: Vec<String>,
    author: String,
    time: u64,
    message: String,
}

fn git_commit(store: &dyn Store, id: &str) -> Result<GitCommit> {
    let (kind, lo) = gitobj::get(store, id)?;
    if kind != "commit" {
        return Err(malformed(&format!("sha1:{id}"), "not a commit"));
    }
    let body = gitobj::split(&lo).expect("a stored git object").1;
    let hs = gitobj::headers(body);
    let field = |name: &str| hs.iter().find(|x| x.0 == name).map(|x| x.1).unwrap_or_default();
    // `name <email> seconds zone`: the last two words are the time.
    let who = |line: &str| -> (String, u64) {
        let mut words = line.rsplitn(3, ' ');
        let (_zone, secs, name) = (words.next(), words.next(), words.next());
        (name.unwrap_or_default().to_string(), secs.and_then(|s| s.parse().ok()).unwrap_or(0))
    };
    let message = body.windows(2).position(|w| w == b"\n\n").map(|i| &body[i + 2..]).unwrap_or(b"");
    Ok(GitCommit {
        tree: field("tree").to_string(),
        parents: hs.iter().filter(|x| x.0 == "parent").map(|x| x.1.to_string()).collect(),
        author: who(field("author")).0,
        time: who(field("committer")).1,
        message: String::from_utf8_lossy(message).trim_end().to_string(),
    })
}

/// A text of one line with no control characters, as `note` and `author`
/// take: the lines of `s` that say something, joined by ` / `.
fn one_text(s: &str) -> String {
    let lines: Vec<String> = s
        .lines()
        .map(|l| l.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    lines.join(" / ")
}

/// Make commits of the git commits `tip` (a git commit id) reaches that
/// `links` does not name yet, parents first, and link them; the key of
/// `tip`'s commit. A git commit becomes a commit of its tree, by its
/// author at its committer's time, and its message is the note of the step
/// from its first parent (a root commit's is the note of its version). A
/// git commit one of `others` links already brings that commit, and what
/// is before it.
pub fn from_git(
    store: &dyn Store,
    alg: &'static Algorithm,
    links: &mut Links,
    tip: &str,
    others: &[&Links],
) -> Result<String> {
    // Each known git commit: its commit, and whose links name it (`None`: ours).
    let mut known: HashMap<String, (String, Option<usize>)> =
        links.iter().map(|(k, g)| (g.clone(), (k.clone(), None))).collect();
    for (i, o) in others.iter().enumerate() {
        for (k, g) in o.iter() {
            known.entry(g.clone()).or_insert((k.clone(), Some(i)));
        }
    }
    let bring = |links: &mut Links, (key, from): &(String, Option<usize>)| -> Result<String> {
        if let Some(i) = from {
            graft(store, links, others[*i], key)?;
        }
        Ok(key.clone())
    };
    if let Some(k) = known.get(tip) {
        return bring(links, k);
    }
    // The new git commits, parents before children.
    let mut order: Vec<(String, GitCommit)> = Vec::new();
    let mut met: HashMap<String, GitCommit> = HashMap::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut stack = vec![(tip.to_string(), false)];
    while let Some((g, done)) = stack.pop() {
        if done {
            let c = met.remove(&g).expect("read when first met");
            order.push((g, c));
            continue;
        }
        if known.contains_key(&g) || !seen.insert(g.clone()) {
            continue;
        }
        let c = git_commit(store, &g)?;
        stack.push((g.clone(), true));
        for p in c.parents.iter().rev() {
            stack.push((p.clone(), false));
        }
        met.insert(g, c);
    }
    let mut vw = VersionWriter::new(store, alg);
    let mut conv = Converter::new(store, alg);
    let mut authors: HashMap<String, String> = HashMap::new();
    let mut last = None;
    for (g, gc) in order {
        let version = conv.directory(&vw.version(&gc.tree)?)?;
        let note = Notes { note: one_text(&gc.message), paths: BTreeMap::new() };
        let note = commits::write_notes(store, alg, &note, &|_| Ok(false))?;
        let mut parents = BTreeMap::new();
        for (i, p) in gc.parents.iter().enumerate() {
            let k = known.get(p).ok_or_else(|| Error::Missing(format!("commit of git commit {p}")))?.clone();
            let pk = bring(links, &k)?;
            let step = if i == 0 { note.clone() } else { None };
            if parents.insert(pk, step).is_some() {
                return Err(Error::Diverged(format!("git commit {g} has two parents that are one commit here")));
            }
        }
        let who = one_text(&gc.author);
        let author = match authors.get(&who) {
            Some(k) => k.clone(),
            None => {
                let k = commits::put_text(store, alg, "author", &who)?;
                authors.insert(who, k.clone());
                k
            }
        };
        let notes = if gc.parents.is_empty() { note } else { None };
        let key = commits::write(store, alg, &Commit { version, author, notes, time: gc.time, parents })?;
        if links.get(&key).is_some_and(|other| *other != g) {
            return Err(Error::Diverged(format!(
                "git commits {} and {g} say the same and are one commit here",
                links[&key]
            )));
        }
        links.insert(key.clone(), g.clone());
        known.insert(g, (key.clone(), None));
        last = Some(key);
    }
    vw.flush()?;
    last.ok_or_else(|| Error::Missing(format!("git commit {tip}")))
}

/// Import every commit reachable from `rev` in the git repository `repo`:
/// its objects are stored as git's, and each commit becomes a commit as
/// [`from_git`] makes them. The key of the newest, and the links.
pub fn import(
    store: &dyn Store,
    alg: &'static Algorithm,
    repo: &std::path::Path,
    rev: &str,
) -> Result<(String, Links)> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let fail = |what: &str, err: &[u8]| Error::Io(format!("git {what}: {}", String::from_utf8_lossy(err).trim()));
    let out = Command::new("git").arg("-C").arg(repo).args(["rev-parse", &format!("{rev}^{{commit}}")]).output()?;
    if !out.status.success() {
        return Err(fail("rev-parse", &out.stderr));
    }
    let tip = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let mut c = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["pack-objects", "--revs", "--stdout", "-q"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    c.stdin.take().expect("piped").write_all(format!("{tip}\n").as_bytes())?;
    let out = c.wait_with_output()?;
    if !out.status.success() {
        return Err(fail("pack-objects", &out.stderr));
    }
    gitobj::unpack(store, &out.stdout)?;
    let mut links = Links::new();
    let key = from_git(store, alg, &mut links, &tip, &[])?;
    Ok((key, links))
}

/// A git tag to publish as a release: its name in git, the semantic version
/// (the name with a leading `v` dropped), the version of the commit it
/// tags, and its changelog (the tag's message, else `release <tag>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagRelease {
    pub name: String,
    pub tag: String,
    pub version: View,
    pub changelog: String,
}

/// A git tag that is not made a release, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagLeftOut {
    pub name: String,
    pub why: String,
}

fn git_out(repo: &std::path::Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = std::process::Command::new("git").arg("-C").arg(repo).args(args).output()?;
    if !out.status.success() {
        return Err(Error::Io(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())));
    }
    Ok(out.stdout)
}

/// The git tags of `repo` that become releases of the history ending at
/// `tip` (whose commits `links` names in git): each tag whose name is a
/// semantic version, a leading `v` aside, and that tags a commit of the
/// history. Oldest commit first; tags in `have` are skipped silently. The
/// rest are left out with a reason.
pub fn tag_releases(
    store: &dyn Store,
    repo: &std::path::Path,
    tip: &str,
    links: &Links,
    have: &[String],
) -> Result<(Vec<TagRelease>, Vec<TagLeftOut>)> {
    let of_git: HashMap<&String, &String> = links.iter().map(|(k, g)| (g, k)).collect();
    let log = commits::log(store, tip)?;
    let pos: HashMap<&String, usize> = log.iter().rev().enumerate().map(|(i, (k, _))| (k, i)).collect();
    let names = String::from_utf8_lossy(&git_out(repo, &["tag", "-l"])?).into_owned();
    let mut found: Vec<(usize, TagRelease)> = Vec::new();
    let mut left = Vec::new();
    for name in names.lines().filter(|n| !n.is_empty()) {
        let tag = name.strip_prefix('v').unwrap_or(name);
        if crate::typed::check_semver(tag.as_bytes()).is_err() {
            left.push(TagLeftOut { name: name.into(), why: "not a semantic version".into() });
            continue;
        }
        let Ok(id) = git_out(repo, &["rev-parse", &format!("refs/tags/{name}^{{commit}}")]) else {
            left.push(TagLeftOut { name: name.into(), why: "tags no commit".into() });
            continue;
        };
        let id = String::from_utf8_lossy(&id).trim().to_string();
        let Some(i) = of_git.get(&id).and_then(|k| pos.get(k)) else {
            left.push(TagLeftOut { name: name.into(), why: format!("tags sha1:{id}, not in the history") });
            continue;
        };
        if have.iter().any(|h| h == tag) {
            continue;
        }
        if let Some((_, other)) = found.iter().find(|(_, r)| r.tag == tag) {
            left.push(TagLeftOut { name: name.into(), why: format!("the same version as {}", other.name) });
            continue;
        }
        let msg = git_out(repo, &["tag", "-l", "--format=%(contents)", name]).unwrap_or_default();
        let msg = String::from_utf8_lossy(&msg).trim().to_string();
        let changelog = if msg.is_empty() { format!("release {tag}") } else { msg };
        let version = log[log.len() - 1 - i].1.version();
        found.push((*i, TagRelease { name: name.into(), tag: tag.into(), version, changelog }));
    }
    found.sort_by(|a, b| (a.0, &a.1.name).cmp(&(b.0, &b.1.name)));
    Ok((found.into_iter().map(|(_, r)| r).collect(), left))
}
