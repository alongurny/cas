//! Mirrors (spec/forge/mirror.md): following a git repository, and
//! exporting a history to git.

use crate::gate::as_actor;
use crate::names::ProjectId;
use crate::project::{DEV, RELEASE};
use crate::record::{self, Doc, Record};
use crate::{Error, Forge, Result, now};
use cas_version::history::{History, NodeId, Step, predecessors_first};
use cas_version::{Node, View, diff, read_blob};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::Path as FsPath;
use std::process::{Command, Stdio};

fn git(dir: &FsPath, args: &[&str]) -> Result<Vec<u8>> {
    let out =
        Command::new("git").arg("-C").arg(dir).args(args).output().map_err(|e| Error::new(500, format!("git: {e}")))?;
    if !out.status.success() {
        return Err(Error::new(
            502,
            format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()),
        ));
    }
    Ok(out.stdout)
}

/// What a pull did.
#[derive(Debug, Default)]
pub struct Pulled {
    pub commits: usize,
    pub moved: Vec<String>,
}

impl Forge {
    /// Set up mirror `name` of `p` following `url`.
    pub fn add_mirror(&self, user: &str, p: &ProjectId, name: &str, url: &str) -> Result<()> {
        crate::names::check(name, "mirror")?;
        record::update(&*self.store, self.alg, &p.var(&format!("m/{name}")), |cur| {
            if cur.is_some() {
                return Err(Error::conflict(format!("mirror {name} exists")));
            }
            let mut d = Doc::new();
            d.set("url", url).set_num("created", now());
            Ok((Some(Record::new(d)), ()))
        })?;
        self.event("project", Some(user), |e| {
            e.set("project", p.to_string()).set("text", format!("mirror {name} of {url}"));
        });
        Ok(())
    }

    /// Fetch mirror `name` into `cache` (a git repository the forge keeps
    /// only to fetch into) and import it: the default branch to `h/dev`,
    /// tags to `h/release`, other branches to `u/<owner>/<branch>`.
    pub fn pull_mirror(&self, user: &str, p: &ProjectId, name: &str, cache: &FsPath) -> Result<Pulled> {
        let var = p.var(&format!("m/{name}"));
        let (_, r) = record::load(&*self.store, &var)?.ok_or_else(|| Error::missing(format!("no mirror {name}")))?;
        let url = r.doc.s("url").to_string();
        let dir = cache.join(format!("{}-{}-{name}.git", p.owner, p.name));
        if !dir.exists() {
            std::fs::create_dir_all(cache).map_err(|e| Error::new(500, e.to_string()))?;
            git(cache, &["init", "--bare", "-q", dir.to_str().unwrap()])?;
        }
        git(&dir, &["fetch", "-q", "--prune", "--tags", &url, "+refs/heads/*:refs/heads/*"])?;
        let head = String::from_utf8_lossy(&git(&dir, &["ls-remote", "--symref", &url, "HEAD"])?).into_owned();
        let default = head
            .lines()
            .find_map(|l| l.strip_prefix("ref: refs/heads/").and_then(|r| r.split_whitespace().next()))
            .unwrap_or("main")
            .to_string();
        let branches =
            String::from_utf8_lossy(&git(&dir, &["for-each-ref", "--format=%(refname:short)", "refs/heads"])?)
                .into_owned();
        // Git's own objects come too, so the commits keep their ids.
        let packed = self.mirror_objects(&dir, r.doc.s("packed"))?;
        // Annotated tags keep their tag objects.
        let mut objects = cas_version::gitobj::Tags::new();
        let refs = String::from_utf8_lossy(&git(
            &dir,
            &["for-each-ref", "--format=%(objecttype) %(objectname) %(refname:short)", "refs/tags"],
        )?)
        .into_owned();
        for l in refs.lines() {
            if let Some(("tag", rest)) = l.split_once(' ')
                && let Some((id, name)) = rest.split_once(' ')
            {
                objects.insert(name.to_string(), id.to_string());
            }
        }
        if !objects.is_empty() {
            self.save_git_tags(p, &objects)?;
        }
        let mut out = Pulled::default();
        for b in branches.lines().filter(|b| !b.is_empty()) {
            let (h, im) = cas_version::git::import(&*self.store, self.alg, &dir, b)?;
            out.commits = out.commits.max(im.commits);
            let target = if b == default { DEV.to_string() } else { format!("u/{}/{}", p.owner, b.replace('/', "-")) };
            if crate::names::check_history(&target).is_err() {
                continue;
            }
            if b == default {
                // Tags become releases, with their messages as changelogs.
                self.mirror_releases(user, p, &dir, &h)?;
            }
            let mut h = h;
            h.tags.clear();
            // Each node is the commit it came from.
            let (lv, mut links) = self.git_links(p, &target)?;
            let before = links.len();
            for (c, n) in &im.nodes {
                if h.nodes.contains_key(n) {
                    links.entry(n.clone()).or_insert_with(|| c.clone());
                }
            }
            if links.len() != before {
                self.save_links(p, &target, lv, &links)?;
            }
            // Other branches follow the remote even when it rewrote them (a force-push): they are
            // the remote's working branches. The default branch never drops nodes.
            if self.replace_history(user, p, &target, &h, b != default)? {
                out.moved.push(target);
            }
        }
        let tip = String::from_utf8_lossy(&git(&dir, &["rev-parse", &default])?).trim().to_string();
        record::update(&*self.store, self.alg, &var, |cur| {
            let mut r = cur.ok_or_else(|| Error::missing("mirror gone"))?;
            r.doc
                .set("packed", packed.clone())
                .set_num("last", now())
                .set("tip", tip.clone())
                .set_num("commits", out.commits as u64);
            Ok((Some(r), ()))
        })?;
        Ok(out)
    }

    /// Store the objects of the cache repository `dir` that are new since
    /// the tips in `packed` (space-separated commit ids); the tips now.
    fn mirror_objects(&self, dir: &FsPath, packed: &str) -> Result<String> {
        let tips = String::from_utf8_lossy(&git(dir, &["for-each-ref", "--format=%(objectname)"])?).into_owned();
        let tips: Vec<&str> = tips.lines().filter(|l| !l.is_empty()).collect();
        let mut revs: String = tips.iter().map(|t| format!("{t}\n")).collect();
        for old in packed.split_whitespace() {
            if git(dir, &["cat-file", "-e", &format!("{old}^{{commit}}")]).is_ok() {
                revs.push_str(&format!("^{old}\n"));
            }
        }
        let mut c = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["pack-objects", "--revs", "--stdout", "--delta-base-offset", "-q"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::new(500, format!("git: {e}")))?;
        let mut w = c.stdin.take().unwrap();
        let feed = std::thread::spawn(move || w.write_all(revs.as_bytes()));
        let o = c.wait_with_output().map_err(|e| Error::new(500, format!("git: {e}")))?;
        let _ = feed.join();
        if !o.status.success() {
            return Err(Error::new(502, format!("git pack-objects: {}", String::from_utf8_lossy(&o.stderr).trim())));
        }
        cas_version::gitobj::unpack(&*self.store, &o.stdout)?;
        let mut tips: Vec<&str> = tips;
        tips.sort();
        tips.dedup();
        Ok(tips.join(" "))
    }

    /// Move history `var` to `h` when `h` extends what it holds (or it is
    /// unset); refuse to drop nodes unless `rewrite`. Whether it moved.
    fn replace_history(&self, user: &str, p: &ProjectId, var: &str, h: &History, rewrite: bool) -> Result<bool> {
        let cur = self.history(p, var)?;
        if let Some((_, old)) = &cur {
            if **old == *h {
                return Ok(false);
            }
            if !rewrite && !h.extends(old) {
                return Err(Error::conflict(format!("the mirror rewrote {var}; it would drop nodes")));
            }
        }
        let old = cur.map(|(v, _)| v);
        as_actor(Some(user), || cas_version::history::save(&*self.store, self.alg, &p.var(var), old.as_ref(), h))?;
        Ok(true)
    }

    fn mirror_releases(&self, user: &str, p: &ProjectId, dir: &FsPath, dev: &History) -> Result<()> {
        // Tags in the order of the commits they name, oldest first.
        let log = dev.log();
        let pos: HashMap<&NodeId, usize> = log.iter().rev().enumerate().map(|(i, n)| (n, i)).collect();
        let mut tags: Vec<(&String, &NodeId)> = dev.tags.iter().collect();
        tags.sort_by_key(|(_, n)| pos.get(n).copied().unwrap_or(0));
        let mut public = self.history(p, RELEASE)?.map(|(_, h)| (*h).clone()).unwrap_or_default();
        let before = public.clone();
        for (tag, node) in tags {
            if public.tags.contains_key(tag) {
                continue;
            }
            let msg =
                String::from_utf8_lossy(&git(dir, &["tag", "-l", "--format=%(contents)", tag]).unwrap_or_default())
                    .trim()
                    .to_string();
            let changelog = if msg.is_empty() { format!("release {tag}") } else { msg };
            let tip = match public.tip() {
                Some(t) if t.version() == node.version() => t,
                _ => public.append(
                    &node.version(),
                    Step { changelog: Some(changelog), compatible: false, notes: None },
                    None,
                )?,
            };
            public.tags.insert(tag.clone(), tip);
        }
        if public != before {
            self.replace_history(user, p, RELEASE, &public, false)?;
        }
        Ok(())
    }

    /// Export history `var` of `p` as branch `branch` of the git
    /// repository `repo` (made if absent), one commit per node, through
    /// `git fast-import`. The same history always gives the same commits.
    pub fn export(&self, p: &ProjectId, var: &str, repo: &FsPath, branch: &str) -> Result<usize> {
        let (_, h) = self.require_history(p, var)?;
        if !repo.exists() {
            std::fs::create_dir_all(repo).map_err(|e| Error::new(500, e.to_string()))?;
            git(repo, &["init", "-q"])?;
        }
        let marks_file = std::env::temp_dir().join(format!("cas-export-{}-{}.marks", std::process::id(), now()));
        let mut child = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["fast-import", "--quiet", "--force", "--done"])
            .arg(format!("--export-marks={}", marks_file.display()))
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| Error::new(500, format!("git fast-import: {e}")))?;
        let mut w = std::io::BufWriter::new(child.stdin.take().unwrap());
        let io = |e: std::io::Error| Error::new(500, e.to_string());
        let mut blob_marks: HashMap<String, usize> = HashMap::new();
        let mut node_marks: HashMap<NodeId, usize> = HashMap::new();
        let mut next = 1usize;
        let order = predecessors_first(h.nodes.clone());
        let empty =
            cas_version::build(&*self.store, self.alg, cas_version::by_alias("dir").unwrap(), &BTreeMap::new())?;
        for (id, n) in &order {
            let preds: Vec<&NodeId> = n.after.keys().collect();
            let base = preds.first().map(|p| p.version()).unwrap_or_else(|| empty.clone());
            let changes = diff(&*self.store, &base, &id.version(), None)?;
            let mut lines = Vec::new();
            for c in changes {
                match c.new {
                    Some(Node::File(v)) => {
                        let (mode, bytes) = git_file(&*self.store, &v)?;
                        let mark = match blob_marks.get(v.key()) {
                            Some(m) => *m,
                            None => {
                                let m = next;
                                next += 1;
                                write!(w, "blob\nmark :{m}\ndata {}\n", bytes.len()).map_err(io)?;
                                w.write_all(&bytes).map_err(io)?;
                                w.write_all(b"\n").map_err(io)?;
                                blob_marks.insert(v.key().to_string(), m);
                                m
                            }
                        };
                        lines.push(format!("M {mode} :{mark} {}\n", quote(c.path.as_str())));
                    }
                    Some(Node::Dir) => {
                        if matches!(c.old, Some(Node::File(_))) {
                            lines.push(format!("D {}\n", quote(c.path.as_str())));
                        }
                    }
                    None => lines.push(format!("D {}\n", quote(c.path.as_str()))),
                }
            }
            let msg = {
                let m = self.line(&h, id);
                if m.is_empty() { "(no notes)".to_string() } else { m }
            };
            let mark = next;
            next += 1;
            node_marks.insert(id.clone(), mark);
            write!(
                w,
                "commit refs/heads/{branch}\nmark :{mark}\ncommitter cas <cas@localhost> 0 +0000\ndata {}\n{msg}\n",
                msg.len() + 1
            )
            .map_err(io)?;
            for (i, pr) in preds.iter().enumerate() {
                writeln!(w, "{} :{}", if i == 0 { "from" } else { "merge" }, node_marks[*pr]).map_err(io)?;
            }
            if preds.is_empty() {
                // A root starts empty, even when the branch exists.
                w.write_all(b"deleteall\n").map_err(io)?;
            }
            for l in lines {
                w.write_all(l.as_bytes()).map_err(io)?;
            }
            w.write_all(b"\n").map_err(io)?;
        }
        w.write_all(b"done\n").map_err(io)?;
        drop(w);
        let st = child.wait().map_err(io)?;
        if !st.success() {
            return Err(Error::new(500, "git fast-import failed"));
        }
        // Tags of the history become lightweight git tags.
        let marks = std::fs::read_to_string(&marks_file).unwrap_or_default();
        let _ = std::fs::remove_file(&marks_file);
        let sha: HashMap<usize, &str> = marks
            .lines()
            .filter_map(|l| l.strip_prefix(':')?.split_once(' ').and_then(|(m, s)| Some((m.parse().ok()?, s))))
            .collect();
        for (tag, node) in &h.tags {
            if let Some(s) = node_marks.get(node).and_then(|m| sha.get(m)) {
                git(repo, &["update-ref", &format!("refs/tags/{tag}"), s])?;
            }
        }
        Ok(order.len())
    }
}

/// A file's git mode and contents.
fn git_file(store: &dyn cas_core::Store, v: &View) -> Result<(&'static str, Vec<u8>)> {
    let alias = v.known_format().map(|f| f.alias).unwrap_or("");
    let raw = read_blob(store, v.key())?;
    Ok(match alias {
        "exec" => ("100755", raw),
        "symlink" => ("120000", raw),
        "git-blob" => ("100644", cas_version::formats::git_blob_contents(&raw).unwrap_or(&raw).to_vec()),
        _ => ("100644", raw),
    })
}

/// A path as fast-import takes it: quoted when it needs to be.
fn quote(p: &str) -> String {
    if p.bytes().any(|b| b == b'"' || b == b'\\' || b == b'\n' || b < 0x20) || p.starts_with('"') {
        let mut s = String::from("\"");
        for c in p.chars() {
            match c {
                '"' => s.push_str("\\\""),
                '\\' => s.push_str("\\\\"),
                '\n' => s.push_str("\\n"),
                c => s.push(c),
            }
        }
        s.push('"');
        s
    } else {
        p.to_string()
    }
}
