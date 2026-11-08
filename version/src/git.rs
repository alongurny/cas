//! Import a git repository's history (spec/git-import.md): git trees become
//! `dir` objects, files become `raw`, `exec` or `symlink` objects, commits
//! become history nodes, commit messages become step changelogs, and git
//! tags become history tags. Runs the `git` command; reads only.

use crate::dir::{DIR, Item};
use crate::history::{History, NodeId, Step};
use crate::{Error, Result, View, put};
use cas_core::Store;
use cas_core::algo::Algorithm;
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path as FsPath;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

fn git(repo: &FsPath, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output()?;
    if !out.status.success() {
        return Err(Error::Io(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())));
    }
    Ok(out.stdout)
}

/// A running `git cat-file --batch`.
struct CatFile {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl CatFile {
    fn start(repo: &FsPath) -> Result<CatFile> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Ok(CatFile { child, input, output })
    }

    fn get(&mut self, id: &str) -> Result<Vec<u8>> {
        writeln!(self.input, "{id}")?;
        self.input.flush()?;
        let mut header = String::new();
        self.output.read_line(&mut header)?;
        let size: usize = header
            .split_whitespace()
            .nth(2)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| Error::Io(format!("git cat-file: {}", header.trim())))?;
        let mut body = vec![0; size + 1];
        self.output.read_exact(&mut body)?;
        body.pop();
        Ok(body)
    }
}

impl Drop for CatFile {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What an import did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Imported {
    pub commits: usize,
    pub trees: usize,
    pub files: usize,
    /// Submodule entries, which have no content here and are left out.
    pub skipped: usize,
}

struct Importer<'a> {
    store: &'a dyn Store,
    alg: &'static Algorithm,
    cat: CatFile,
    trees: HashMap<String, View>,
    files: HashMap<(String, &'static str), View>,
    n: Imported,
}

impl Importer<'_> {
    fn file(&mut self, id: &str, alias: &'static str) -> Result<View> {
        if let Some(v) = self.files.get(&(id.to_string(), alias)) {
            return Ok(v.clone());
        }
        let bytes = self.cat.get(id)?;
        let v = put(self.store, self.alg, alias, &bytes)?;
        self.files.insert((id.to_string(), alias), v.clone());
        self.n.files += 1;
        Ok(v)
    }

    /// The `dir` view of git tree `id`; each tree is converted once.
    fn tree(&mut self, id: &str) -> Result<View> {
        if let Some(v) = self.trees.get(id) {
            return Ok(v.clone());
        }
        let raw = self.cat.get(id)?;
        // Binary tree entries: "<mode> <name>\0<20-byte id>".
        let mut items = Vec::new();
        let mut rest = &raw[..];
        while !rest.is_empty() {
            let nul = rest.iter().position(|&b| b == 0).ok_or_else(|| Error::Io(format!("bad git tree {id}")))?;
            let head = std::str::from_utf8(&rest[..nul]).map_err(|_| Error::Io(format!("non-UTF-8 name in {id}")))?;
            let (mode, name) = head.split_once(' ').ok_or_else(|| Error::Io(format!("bad git tree {id}")))?;
            let child: String = rest[nul + 1..nul + 21].iter().map(|b| format!("{b:02x}")).collect();
            rest = &rest[nul + 21..];
            let view = match mode {
                "40000" => self.tree(&child)?,
                "100644" | "100664" => self.file(&child, "raw")?,
                "100755" => self.file(&child, "exec")?,
                "120000" => self.file(&child, "symlink")?,
                _ => {
                    self.n.skipped += 1;
                    continue;
                }
            };
            items.push(Item { name: name.to_string(), child: view });
        }
        // Git sorts directories as if their names ended in '/'; dir sorts by name.
        items.sort_by(|a, b| a.name.cmp(&b.name));
        // `put` checks the encoding, so names the dir format refuses fail here.
        let v = put(self.store, self.alg, "dir", &DIR.encode(&items))?;
        self.trees.insert(id.to_string(), v.clone());
        self.n.trees += 1;
        Ok(v)
    }
}

/// Import every commit reachable from `rev` in `repo` as a new history.
pub fn import(store: &dyn Store, alg: &'static Algorithm, repo: &FsPath, rev: &str) -> Result<(History, Imported)> {
    // git re-encodes messages that declare an encoding; a message that is
    // not UTF-8 and says nothing (old latin-1 commits) is kept lossily.
    let log =
        git(repo, &["log", "--encoding=UTF-8", "--topo-order", "--reverse", "--format=%H%x1f%P%x1f%T%x1f%B%x1e", rev])?;
    let log = String::from_utf8_lossy(&log);
    let mut im = Importer {
        store,
        alg,
        cat: CatFile::start(repo)?,
        trees: HashMap::new(),
        files: HashMap::new(),
        n: Imported::default(),
    };
    let mut h = History::default();
    let mut nodes: BTreeMap<String, NodeId> = BTreeMap::new();
    for rec in log.split('\x1e').map(|r| r.trim_start_matches('\n')).filter(|r| !r.is_empty()) {
        let f: Vec<&str> = rec.splitn(4, '\x1f').collect();
        let [commit, parents, tree, message] = f[..] else {
            return Err(Error::Io(format!("bad git log record {rec:?}")));
        };
        let version = im.tree(tree)?;
        let message = message.trim_end();
        let step = Step { changelog: (!message.is_empty()).then(|| message.to_string()), compatible: false };
        let mut after = BTreeMap::new();
        for p in parents.split_whitespace() {
            let pred = nodes.get(p).ok_or_else(|| Error::Io(format!("parent {p} of {commit} not imported")))?;
            after.insert(pred.clone(), step.clone());
        }
        let doc = if parents.is_empty() { step.changelog.clone() } else { None };
        nodes.insert(commit.to_string(), h.add(&version, after, doc)?);
        im.n.commits += 1;
    }
    // Tags that point (through annotated tags) at imported commits.
    let refs = String::from_utf8(git(repo, &["show-ref", "--tags", "-d"]).unwrap_or_default()).unwrap_or_default();
    for line in refs.lines() {
        let Some((id, name)) = line.split_once(' ') else { continue };
        let name = name.trim_start_matches("refs/tags/");
        let (name, peeled) = match name.strip_suffix("^{}") {
            Some(n) => (n, true),
            None => (name, false),
        };
        if let Some(node) = nodes.get(id)
            && (peeled || !h.tags.contains_key(name))
        {
            h.tags.insert(name.to_string(), node.clone());
        }
    }
    Ok((h, im.n))
}
