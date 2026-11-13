//! Releases (spec/formats/release.md): the `h/release` history, published
//! from `h/dev` through the forge, and archives of a release.

use crate::accounts::Caller;
use crate::gate::as_actor;
use crate::names::{ProjectId, Role};
use crate::project::{DEV, RELEASE};
use crate::{Error, Forge, Result};
use cas_core::Expect;
use cas_version::uni::Value;
use cas_version::{Node, View, read_blob, releases, walk};
use serde_json::{Value as J, json};

impl Forge {
    /// The releases, newest first: tag, version, changelog, kind of change.
    pub fn releases(&self, p: &ProjectId) -> Result<Vec<J>> {
        let Some(tip) = releases::tip(&*self.store, &p.var(RELEASE))? else { return Ok(Vec::new()) };
        let mut out = Vec::new();
        for (key, r, tag) in releases::log(&*self.store, &tip)? {
            // What it says of the release before it.
            let (changelog, change) = match r.from.values().next() {
                Some((note, change)) => (Some(releases::changelog(&*self.store, note)?), Some(*change)),
                None => (None, None),
            };
            out.push(json!({
                "id": key,
                "version": r.version().to_string(),
                "tags": [tag],
                "changelog": changelog,
                "change": change,
                "compatible": change.is_some_and(|c| c != "breaking"),
            }));
        }
        Ok(out)
    }

    /// Publish the version of commit `what` of `h/dev` (empty: its tip) as
    /// the release `tag`, a semantic version, once the release gates hold.
    /// After an earlier release it says its `change` from that one
    /// (breaking, feature, patch or security) and why in `changelog`.
    pub fn publish_release(
        &self,
        c: &Caller,
        p: &ProjectId,
        what: &str,
        tag: &str,
        changelog: &str,
        change: &str,
    ) -> Result<String> {
        self.require(Some(c), p, Role::Maintain)?;
        if let Err(why) = cas_version::typed::check_semver(tag.as_bytes()) {
            return Err(Error::bad(format!("a release's tag is a semantic version, such as 1.4.0: {why}")));
        }
        if changelog.trim().is_empty() {
            return Err(Error::bad("a release has a changelog: what is different from the previous one"));
        }
        let (_, dev) = self.require_history(p, DEV)?;
        let version = dev.version(&self.node(p, &dev, what)?);
        // The gates of the release history, on the version released.
        let s = self.settings(p)?.ok_or_else(|| Error::missing("no project"))?;
        for name in s.doc("gates").doc(RELEASE).keys("checks") {
            let st = self.check_status(&version, &name)?;
            if st != "pass" {
                return Err(Error::conflict(format!("check {name} is {st}")));
            }
        }
        let var = p.var(RELEASE);
        let old = releases::tip(&*self.store, &var)?;
        if let Some(o) = &old
            && releases::read(&*self.store, o)?.version == version.key()
        {
            return Err(Error::conflict("that version is the latest release"));
        }
        // A first release follows nothing, so it says no change.
        let edge = old.is_some().then_some((change, changelog.trim()));
        let made = as_actor(Some(&c.user), || releases::publish(&*self.store, self.alg, &var, &version, tag, edge));
        let key = match made {
            Ok(k) => k,
            Err(cas_version::Error::NotRelease(m)) => return Err(Error::bad(m)),
            Err(e) => return Err(e.into()),
        };
        self.event("release", Some(&c.user), |e| {
            e.set("project", p.to_string()).set("text", format!("{tag}: {}", changelog.trim()));
            e.set("new", key.clone());
        });
        Ok(key)
    }

    /// The `.tar.gz` of a release's version, as derived data: made once,
    /// then read from `derived/<archive>/<version>`.
    pub fn archive(&self, p: &ProjectId, tag: &str) -> Result<(String, Vec<u8>)> {
        let none = || Error::missing(format!("no release {tag}"));
        let tip = releases::tip(&*self.store, &p.var(RELEASE))?.ok_or_else(none)?;
        let hit = releases::log(&*self.store, &tip)?.into_iter().find(|(_, _, t)| t == tag).ok_or_else(none)?;
        let version = hit.1.version();
        let obj = Value::map([("name", Value::str("archive")), ("format", Value::str("tar.gz"))]);
        let var = format!("derived/{}/{}", self.alg.key(&obj.serialize()), version.key());
        let name = format!("{}-{tag}", p.name);
        if let Some(v) = crate::record::head(&*self.store, &var)? {
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
            let full = format!("{top}/{path}");
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
