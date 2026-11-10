//! Releases (spec/release-history.md): the `h/release` history, published
//! from `h/dev` through the forge, and archives of a release.

use crate::accounts::Caller;
use crate::gate::as_actor;
use crate::names::{ProjectId, Role};
use crate::project::{DEV, RELEASE};
use crate::{Error, Forge, Result};
use cas_core::Expect;
use cas_version::history::{History, Step};
use cas_version::merge::{self, Outcome};
use cas_version::uni::Value;
use cas_version::{Node, View, read_blob, walk};
use serde_json::{Value as J, json};

impl Forge {
    /// The releases, newest first: tag, node, changelog, compatibility.
    pub fn releases(&self, p: &ProjectId) -> Result<Vec<J>> {
        let Some((_, h)) = self.history(p, RELEASE)? else { return Ok(Vec::new()) };
        let mut out = Vec::new();
        for id in h.log() {
            let n = &h.nodes[&id];
            let tags: Vec<&String> = h.tags.iter().filter(|(_, t)| **t == id).map(|(k, _)| k).collect();
            let step = n.after.values().next();
            out.push(json!({
                "id": id.to_string(),
                "version": id.version().to_string(),
                "tags": tags,
                "changelog": step.and_then(|s| s.changelog.clone()).or_else(|| n.doc.clone()),
                "compatible": step.is_some_and(|s| s.compatible),
            }));
        }
        Ok(out)
    }

    /// Publish node `what` of `h/dev` (empty: its tip) as the next release,
    /// tagged `tag`, once the release gates hold.
    pub fn publish_release(
        &self,
        c: &Caller,
        p: &ProjectId,
        what: &str,
        tag: &str,
        changelog: &str,
        compatible: bool,
    ) -> Result<String> {
        self.require(Some(c), p, Role::Maintain)?;
        if tag.is_empty() || !cas_version::valid_name(tag) {
            return Err(Error::bad("a release needs a tag: a name without / or control characters"));
        }
        if changelog.trim().is_empty() {
            return Err(Error::bad("a release has a changelog: what is different from the previous one"));
        }
        let (_, dev) = self.require_history(p, DEV)?;
        let node = self.node(&dev, what)?;
        // The gates of the release history, on the version released.
        let s = self.settings(p)?.ok_or_else(|| Error::missing("no project"))?;
        for name in s.doc("gates").doc(RELEASE).keys("checks") {
            let st = self.check_status(&node.version(), &name)?;
            if st != "pass" {
                return Err(Error::conflict(format!("check {name} is {st}")));
            }
        }
        // `h/dev` as of the node.
        let keep = dev.ancestors(&node);
        let mut mine = History { doc: dev.doc.clone(), nodes: Default::default(), tags: Default::default() };
        for (id, n) in &dev.nodes {
            if keep.contains(id) || *id == node {
                mine.nodes.insert(id.clone(), n.clone());
            }
        }
        loop {
            let (old, mut public) = match self.history(p, RELEASE)? {
                Some((v, h)) => (Some(v), (*h).clone()),
                None => (None, History::default()),
            };
            if public.tags.contains_key(tag) {
                return Err(Error::conflict(format!("tag {tag} exists")));
            }
            let step = Step { changelog: Some(changelog.trim().to_string()), compatible, notes: None };
            let tip = match merge::publish(&mine, &mut public, step, None)? {
                Outcome::Done(t) => t,
                Outcome::UpToDate => return Err(Error::conflict("that version is the latest release")),
                Outcome::Conflicts(_) => return Err(Error::conflict("conflicts")),
            };
            public.tags.insert(tag.to_string(), tip.clone());
            let saved = as_actor(Some(&c.user), || {
                cas_version::history::save(&*self.store, self.alg, &p.var(RELEASE), old.as_ref(), &public)
            });
            match saved {
                Ok(_) => {
                    self.event("release", Some(&c.user), |e| {
                        e.set("project", p.to_string()).set("text", format!("{tag}: {}", changelog.trim()));
                        e.set("new", tip.to_string());
                    });
                    return Ok(tip.to_string());
                }
                Err(cas_version::Error::Store(cas_core::Error::Conflict)) => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// The `.tar.gz` of a release's version, as derived data: made once,
    /// then read from `derived/<archive>/<version>`.
    pub fn archive(&self, p: &ProjectId, tag: &str) -> Result<(String, Vec<u8>)> {
        let (_, h) = self.require_history(p, RELEASE)?;
        let id = h.tags.get(tag).ok_or_else(|| Error::missing(format!("no release {tag}")))?;
        let version = id.version();
        let obj = Value::map([("name", Value::str("archive")), ("format", Value::str("tar.gz"))]);
        let var = format!("derived/{}/{}", self.alg.key(&obj.serialize()), version.key());
        let name = format!("{}-{tag}", p.name);
        if let Some(v) = cas_version::history::head(&*self.store, &var)? {
            return Ok((name, read_blob(&*self.store, v.key())?));
        }
        let bytes = self.tar_gz(&version, &name)?;
        let v = crate::record::raw(&*self.store, self.alg, &bytes)?;
        match self.store.write(&var, v.to_string().as_bytes(), &Expect::Absent) {
            Ok(()) | Err(cas_core::Error::Conflict) => {}
            Err(e) => return Err(e.into()),
        }
        Ok((name, bytes))
    }

    fn tar_gz(&self, version: &View, top: &str) -> Result<Vec<u8>> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut tar = tar::Builder::new(gz);
        let io = |e: std::io::Error| Error::new(500, e.to_string());
        for item in walk(&*self.store, version, None) {
            let (path, node) = item?;
            let full = format!("{top}/{}", path.as_str());
            let mut header = tar::Header::new_gnu();
            header.set_mtime(0);
            match node {
                Node::Dir => {
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_mode(0o755);
                    header.set_size(0);
                    tar.append_data(&mut header, &full, std::io::empty()).map_err(io)?;
                }
                Node::File(v) => {
                    let alias = v.known_format().map(|f| f.alias).unwrap_or("");
                    let raw = read_blob(&*self.store, v.key())?;
                    let bytes = match alias {
                        "git-blob" => cas_version::formats::git_blob_contents(&raw).unwrap_or(&raw).to_vec(),
                        _ => raw,
                    };
                    if alias == "symlink" {
                        header.set_entry_type(tar::EntryType::Symlink);
                        header.set_size(0);
                        header.set_mode(0o777);
                        let target = String::from_utf8_lossy(&bytes).into_owned();
                        tar.append_link(&mut header, &full, target).map_err(io)?;
                        continue;
                    }
                    header.set_mode(if alias == "exec" { 0o755 } else { 0o644 });
                    header.set_size(bytes.len() as u64);
                    tar.append_data(&mut header, &full, &bytes[..]).map_err(io)?;
                }
            }
        }
        tar.into_inner().map_err(io)?.finish().map_err(io)
    }
}
