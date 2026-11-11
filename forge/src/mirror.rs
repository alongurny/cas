//! Mirrors (spec/forge/mirror.md): following a git repository, and
//! exporting a history to git.

use crate::gate::as_actor;
use crate::names::ProjectId;
use crate::project::{DEV, RELEASE};
use crate::record::{self, Doc, Record};
use crate::{Error, Forge, Result, now};
use cas_version::gitcommits::{self, Links};
use cas_version::{commits, releases};
use std::collections::HashMap;
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
    /// only to fetch into) and bring it in: the default branch to `h/dev`,
    /// tags that are semantic versions to `h/release`, other branches to
    /// `u/<owner>/<branch>`.
    pub fn pull_mirror(&self, user: &str, p: &ProjectId, name: &str, cache: &FsPath) -> Result<Pulled> {
        let var = p.var(&format!("m/{name}"));
        let (_, r) = record::load(&*self.store, &var)?.ok_or_else(|| Error::missing(format!("no mirror {name}")))?;
        let url = r.doc.s("url").to_string();
        let dir = cache.join(format!("{}-{}-{name}.git", p.owner, p.name));
        if !dir.exists() {
            std::fs::create_dir_all(cache).map_err(|e| Error::new(500, e.to_string()))?;
            // Named from inside `cache`, which may itself be a relative path.
            let name = dir.file_name().and_then(|n| n.to_str()).expect("just joined");
            git(cache, &["init", "--bare", "-q", name])?;
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
        // The default branch first: the others share its commits.
        let mut order: Vec<&str> = branches.lines().filter(|b| !b.is_empty()).collect();
        order.sort_by_key(|b| *b != default);
        let mut dev_links = Links::new();
        for b in order {
            let target = if b == default { DEV.to_string() } else { format!("u/{}/{}", p.owner, b.replace('/', "-")) };
            if crate::names::check_history(&target).is_err() {
                continue;
            }
            let id =
                String::from_utf8_lossy(&git(&dir, &["rev-parse", &format!("refs/heads/{b}")])?).trim().to_string();
            // Each commit is the git commit it came from.
            let (lv, mut links) = self.git_links(p, &target)?;
            let before = links.len();
            let tip = gitcommits::from_git(&*self.store, self.alg, &mut links, &id, &[&dev_links])?;
            if links.len() != before {
                self.save_links(p, &target, lv, &links)?;
            }
            // Other branches follow the remote even when it rewrote them (a force-push): they are
            // the remote's working branches. The default branch never drops commits.
            if self.replace_history(user, p, &target, &tip, b != default)? {
                out.moved.push(target);
            }
            if b == default {
                out.commits = commits::log(&*self.store, &tip)?.len();
                // Tags become releases, with their messages as changelogs.
                self.mirror_releases(user, p, &dir, &tip, &links)?;
                dev_links = links;
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

    /// Move history `var` to the commit `tip` when it has what the history
    /// holds before it (or the history is unset); refuse to drop commits
    /// unless `rewrite`. Whether it moved.
    fn replace_history(&self, user: &str, p: &ProjectId, var: &str, tip: &str, rewrite: bool) -> Result<bool> {
        let name = p.var(var);
        let old = commits::tip(&*self.store, &name)?;
        if let Some(o) = &old {
            if o == tip {
                return Ok(false);
            }
            if !rewrite && !commits::reaches(&*self.store, tip, o)? {
                return Err(Error::conflict(format!("the mirror rewrote {var}; it would drop commits")));
            }
        }
        as_actor(Some(user), || commits::advance(&*self.store, &name, old.as_deref(), tip))?;
        Ok(true)
    }

    /// Publish each git tag of `dir` that is a semantic version (a leading
    /// `v` aside) and names a commit of the history ending at `tip`, oldest
    /// first, unless there is a release of that tag already.
    fn mirror_releases(&self, user: &str, p: &ProjectId, dir: &FsPath, tip: &str, links: &Links) -> Result<()> {
        let of_git: HashMap<&String, &String> = links.iter().map(|(k, g)| (g, k)).collect();
        let log = commits::log(&*self.store, tip)?;
        let pos: HashMap<&String, usize> = log.iter().rev().enumerate().map(|(i, (k, _))| (k, i)).collect();
        let names = String::from_utf8_lossy(&git(dir, &["tag", "-l"])?).into_owned();
        let mut tags: Vec<(usize, &str, String)> = Vec::new();
        for name in names.lines().filter(|n| !n.is_empty()) {
            let tag = name.strip_prefix('v').unwrap_or(name);
            if cas_version::typed::check_semver(tag.as_bytes()).is_err() {
                continue;
            }
            let Ok(id) = git(dir, &["rev-parse", &format!("refs/tags/{name}^{{commit}}")]) else { continue };
            let id = String::from_utf8_lossy(&id).trim().to_string();
            if let Some(i) = of_git.get(&id).and_then(|k| pos.get(k)) {
                tags.push((*i, name, tag.to_string()));
            }
        }
        tags.sort();
        let var = p.var(RELEASE);
        let mut have: Vec<String> = match releases::tip(&*self.store, &var)? {
            Some(t) => releases::log(&*self.store, &t)?.into_iter().map(|(_, _, tag)| tag).collect(),
            None => Vec::new(),
        };
        for (i, name, tag) in tags {
            if have.contains(&tag) {
                continue;
            }
            let version = log[log.len() - 1 - i].1.version();
            let msg =
                String::from_utf8_lossy(&git(dir, &["tag", "-l", "--format=%(contents)", name]).unwrap_or_default())
                    .trim()
                    .to_string();
            let changelog = if msg.is_empty() { format!("release {tag}") } else { msg };
            // What a tag changes is not said in git: it may be anything.
            let edge = (!have.is_empty()).then_some(("breaking", changelog.as_str()));
            as_actor(Some(user), || releases::publish(&*self.store, self.alg, &var, &version, &tag, edge))?;
            have.push(tag);
        }
        Ok(())
    }

    /// Export history `var` of `p` as branch `branch` of the git
    /// repository `repo` (made if absent): the git commits the forge's git
    /// endpoint serves for it, and the releases as tags. The same history
    /// always gives the same commits. How many commits the branch has.
    pub fn export(&self, p: &ProjectId, var: &str, repo: &FsPath, branch: &str) -> Result<usize> {
        let (_, h) = self.require_history(p, var)?;
        if !repo.exists() {
            std::fs::create_dir_all(repo).map_err(|e| Error::new(500, e.to_string()))?;
            git(repo, &["init", "-q"])?;
        }
        let (_, mut links) = self.git_links(p, var)?;
        gitcommits::to_git(&*self.store, &h.tip, &mut links, &[])?;
        let tip = links[&h.tip].clone();
        let ids = cas_version::gitobj::closure(&*self.store, std::slice::from_ref(&tip), &[])?;
        let pack = cas_version::gitobj::pack(&*self.store, &ids)?;
        let mut child = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["index-pack", "--stdin", "--fix-thin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::new(500, format!("git index-pack: {e}")))?;
        let mut w = child.stdin.take().unwrap();
        let feed = std::thread::spawn(move || w.write_all(&pack));
        let o = child.wait_with_output().map_err(|e| Error::new(500, format!("git: {e}")))?;
        let _ = feed.join();
        if !o.status.success() {
            return Err(Error::new(500, format!("git index-pack: {}", String::from_utf8_lossy(&o.stderr).trim())));
        }
        git(repo, &["update-ref", &format!("refs/heads/{branch}"), &tip])?;
        // A release is a tag on the newest commit with its version.
        for (version, tags) in self.release_tags(p)? {
            if let Some((k, _)) = h.log.iter().find(|(_, c)| c.version == version) {
                for tag in tags {
                    git(repo, &["update-ref", &format!("refs/tags/{tag}"), &links[k]])?;
                }
            }
        }
        Ok(h.log.len())
    }
}
