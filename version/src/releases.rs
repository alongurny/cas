//! Releases (spec/formats/release.md). A release names a version and a
//! tag (a semantic version), and each release it follows with the kind of
//! change from it and a changelog. A release history is a variable holding
//! a view of its newest release.

use crate::commits::{one_line, put_text, text, view};
use crate::typed::{CHANGES, RecV, Value, by_type, key_bytes, key_text};
use crate::{Error, Result, View, malformed, read_blob};
use cas_core::algo::Algorithm;
use cas_core::{Expect, Store};
use std::collections::{BTreeMap, HashSet};

/// One release. `version`, `tag` and the keys of `from` are blake3 keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// The version: a `directory`.
    pub version: String,
    /// The key of the `tag`.
    pub tag: String,
    /// Each release this one follows: the changelog's key (a `note`) and
    /// the kind of change, one of [`CHANGES`].
    pub from: BTreeMap<String, (String, &'static str)>,
}

fn bytes_of(key: &str) -> Result<[u8; 32]> {
    key_bytes(key).ok_or_else(|| Error::BadKey(key.to_string()))
}

impl Release {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let head = RecV { keys: vec![bytes_of(&self.version)?, bytes_of(&self.tag)?], ..Default::default() };
        let mut list = Vec::new();
        for (r, (changelog, change)) in &self.from {
            let kind = CHANGES.iter().position(|c| c == change).expect("a kind of change") as u8 + 1;
            list.push(RecV { keys: vec![bytes_of(r)?, bytes_of(changelog)?], kinds: vec![kind], ..Default::default() });
        }
        Ok(by_type("release").unwrap().print(&Value { head: Some(head), list }))
    }

    pub fn decode(b: &[u8]) -> std::result::Result<Release, String> {
        let t = by_type("release").unwrap();
        let v = t.parse(b)?;
        if t.print(&v) != b {
            return Err("not the one encoding of its value".into());
        }
        let h = v.head.ok_or("a release has a head")?;
        Ok(Release {
            version: key_text(&h.keys[0]),
            tag: key_text(&h.keys[1]),
            from: v
                .list
                .iter()
                .map(|r| (key_text(&r.keys[0]), (key_text(&r.keys[1]), CHANGES[r.kinds[0] as usize - 1])))
                .collect(),
        })
    }

    pub fn version(&self) -> View {
        view("directory", &self.version)
    }
}

pub fn read(store: &dyn Store, key: &str) -> Result<Release> {
    Release::decode(&read_blob(store, key)?).map_err(|e| malformed(key, e))
}

pub fn is_release(v: &View) -> bool {
    v.known_format().is_some_and(|f| f.alias == "release")
}

/// The newest release of the release history `name`: `None` when unset,
/// an error when the variable holds something else.
pub fn tip(store: &dyn Store, name: &str) -> Result<Option<String>> {
    match crate::history::head(store, name)? {
        None => Ok(None),
        Some(v) if is_release(&v) => Ok(Some(v.key)),
        Some(v) => Err(Error::NotRelease(format!("{name} holds {v}"))),
    }
}

/// Every release `tip` reaches, newest first, each with its tag's text.
pub fn log(store: &dyn Store, tip: &str) -> Result<Vec<(String, Release, String)>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut wave = vec![tip.to_string()];
    while !wave.is_empty() {
        let mut next = Vec::new();
        for k in wave {
            if !seen.insert(k.clone()) {
                continue;
            }
            let r = read(store, &k)?;
            next.extend(r.from.keys().cloned());
            let tag = text(store, &r.tag)?;
            out.push((k, r, tag));
        }
        wave = next;
    }
    Ok(out)
}

/// The release `what` names in the history ending at `tip`: `latest`, a
/// tag, a key or the start of one.
pub fn resolve(store: &dyn Store, tip: &str, what: &str) -> Result<Option<String>> {
    if what == "latest" {
        return Ok(Some(tip.to_string()));
    }
    let all = log(store, tip)?;
    if let Some((k, _, _)) = all.iter().find(|(_, _, tag)| tag == what) {
        return Ok(Some(k.clone()));
    }
    let digits = what.strip_prefix("blake3:").unwrap_or(what);
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Ok(None);
    }
    let want = format!("blake3:{}", digits.to_ascii_lowercase());
    let hits: Vec<&String> = all.iter().map(|(k, _, _)| k).filter(|k| k.starts_with(&want)).collect();
    Ok(if hits.len() == 1 { Some(hits[0].clone()) } else { None })
}

/// Add `version` to the release history `name` as the release `tag`. After
/// an earlier release it says what kind of change it is from that one and
/// carries a changelog; a first release has neither. A tag names one
/// release of a history. Retried while other writers win the race.
pub fn publish(
    store: &dyn Store,
    alg: &'static Algorithm,
    name: &str,
    version: &View,
    tag: &str,
    edge: Option<(&str, &str)>,
) -> Result<String> {
    if version.known_format().is_none_or(|f| f.alias != "directory") {
        return Err(Error::NotRelease(format!("{version} is not a directory")));
    }
    let tag_key = put_text(store, alg, "tag", tag)?;
    loop {
        let old = tip(store, name)?;
        let mut from = BTreeMap::new();
        match (&old, edge) {
            (None, None) => {}
            (None, Some(_)) => {
                return Err(Error::NotRelease("a first release follows nothing: it has no change or changelog".into()));
            }
            (Some(_), None) => {
                return Err(Error::NotRelease(
                    "a release after another says its change (breaking, feature, patch or security) and has a changelog"
                        .into(),
                ));
            }
            (Some(o), Some((change, changelog))) => {
                if log(store, o)?.iter().any(|(_, _, t)| t == tag) {
                    return Err(Error::NotRelease(format!("{name} already has a release {tag}")));
                }
                let change = CHANGES
                    .iter()
                    .find(|c| **c == change)
                    .ok_or_else(|| Error::NotRelease(format!("{change:?} is not one of {}", CHANGES.join(", "))))?;
                let changelog = one_line(changelog);
                if changelog.is_empty() {
                    return Err(Error::NotRelease("the changelog is empty".into()));
                }
                from.insert(o.clone(), (put_text(store, alg, "note", &changelog)?, *change));
            }
        }
        let r = Release { version: version.key.clone(), tag: tag_key.clone(), from };
        let key = crate::write_blob(store, alg, &r.encode()?)?;
        let expect = match &old {
            None => Expect::Absent,
            Some(o) => Expect::Value(view("release", o).to_string().into_bytes()),
        };
        match store.write(name, view("release", &key).to_string().as_bytes(), &expect) {
            Err(cas_core::Error::Conflict) => continue,
            r => return r.map(|_| key).map_err(Error::from),
        }
    }
}

/// Whether upgrading from the release `old` to the release `new` is safe:
/// some path from `old` to `new` has no `breaking` step.
pub fn safe(store: &dyn Store, old: &str, new: &str) -> Result<bool> {
    let mut seen = HashSet::new();
    let mut todo = vec![new.to_string()];
    while let Some(k) = todo.pop() {
        if k == old {
            return Ok(true);
        }
        if !seen.insert(k.clone()) {
            continue;
        }
        todo.extend(read(store, &k)?.from.into_iter().filter(|(_, (_, c))| *c != "breaking").map(|(r, _)| r));
    }
    Ok(false)
}
