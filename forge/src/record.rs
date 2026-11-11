//! Records (spec/forge/store.md, "Records"): a `dir` version holding
//! `record.uni` and the views the record refers to.

use crate::{Error, Result};
use cas_core::algo::Algorithm;
use cas_core::{Expect, Store};
use cas_version::uni::Value;
use cas_version::{Node, Path, View, build, by_alias, put, read_blob, walk};
use std::collections::BTreeMap;

/// A record's fields: string keys to strings or nested documents.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Doc(pub BTreeMap<String, Value>);

impl Doc {
    pub fn new() -> Doc {
        Doc::default()
    }

    pub fn of(v: &Value) -> Doc {
        Doc(v
            .as_map()
            .map(|m| m.iter().filter_map(|(k, v)| Some((k.as_str()?.to_string(), v.clone()))).collect())
            .unwrap_or_default())
    }

    pub fn value(&self) -> Value {
        Value::Map(self.0.iter().map(|(k, v)| (Value::str(k), v.clone())).collect())
    }

    /// A string field, empty when absent.
    pub fn s(&self, k: &str) -> &str {
        self.opt(k).unwrap_or("")
    }

    pub fn opt(&self, k: &str) -> Option<&str> {
        self.0.get(k).and_then(Value::as_str)
    }

    pub fn num(&self, k: &str) -> u64 {
        self.s(k).parse().unwrap_or(0)
    }

    pub fn flag(&self, k: &str) -> bool {
        self.s(k) == "true"
    }

    /// A nested document (empty when absent).
    pub fn doc(&self, k: &str) -> Doc {
        self.0.get(k).map(Doc::of).unwrap_or_default()
    }

    /// The keys of a set or map field.
    pub fn keys(&self, k: &str) -> Vec<String> {
        self.doc(k).0.into_keys().collect()
    }

    pub fn set(&mut self, k: &str, v: impl Into<String>) -> &mut Doc {
        let v = v.into();
        self.0.insert(k.to_string(), Value::str(v));
        self
    }

    pub fn set_num(&mut self, k: &str, n: u64) -> &mut Doc {
        self.set(k, n.to_string())
    }

    /// Set `k` to `v`, or remove it when `v` is empty or `None`.
    pub fn set_opt(&mut self, k: &str, v: Option<&str>) -> &mut Doc {
        match v.filter(|v| !v.is_empty()) {
            Some(v) => self.set(k, v),
            None => self.remove(k),
        }
    }

    pub fn set_doc(&mut self, k: &str, d: Doc) -> &mut Doc {
        if d.0.is_empty() {
            self.0.remove(k);
        } else {
            self.0.insert(k.to_string(), d.value());
        }
        self
    }

    pub fn remove(&mut self, k: &str) -> &mut Doc {
        self.0.remove(k);
        self
    }

    /// Change nested document `k` with `f`.
    pub fn edit(&mut self, k: &str, f: impl FnOnce(&mut Doc)) -> &mut Doc {
        let mut d = self.doc(k);
        f(&mut d);
        self.set_doc(k, d)
    }

    /// The entries of a nested map, each as a document.
    pub fn entries(&self, k: &str) -> Vec<(String, Doc)> {
        self.doc(k).0.into_iter().map(|(k, v)| (k, Doc::of(&v))).collect()
    }

    /// The items of a list field, in order (spec/forge/store.md).
    pub fn list(&self, k: &str) -> Vec<Doc> {
        self.entries(k).into_iter().map(|(_, d)| d).collect()
    }

    /// Append to a list field; the item's number (from 1).
    pub fn push(&mut self, k: &str, item: Doc) -> u64 {
        let n = self.doc(k).0.len() as u64 + 1;
        self.edit(k, |l| {
            l.0.insert(list_key(n), item.value());
        });
        n
    }

    /// Change item `n` (from 1) of a list field.
    pub fn edit_item(&mut self, k: &str, n: u64, f: impl FnOnce(&mut Doc)) -> bool {
        let mut l = self.doc(k);
        let Some(v) = l.0.get(&list_key(n)) else { return false };
        let mut d = Doc::of(v);
        f(&mut d);
        l.0.insert(list_key(n), d.value());
        self.set_doc(k, l);
        true
    }

    /// The text form (`uni-txt`).
    pub fn text(&self) -> String {
        self.value().to_text()
    }

    /// As JSON: strings and objects; lists stay objects keyed by number.
    pub fn json(&self) -> serde_json::Value {
        to_json(&self.value())
    }
}

/// The key of item `n` of a list.
pub fn list_key(n: u64) -> String {
    format!("{n:012}")
}

pub fn to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Str(s) => serde_json::Value::String(s.clone()),
        Value::Map(m) => serde_json::Value::Object(
            m.iter().map(|(k, v)| (k.as_str().map_or_else(|| k.to_text(), String::from), to_json(v))).collect(),
        ),
    }
}

/// A record: its document and the views beside it, by path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    pub doc: Doc,
    pub views: BTreeMap<String, View>,
}

const DOC: &str = "record.uni";

impl Record {
    pub fn new(doc: Doc) -> Record {
        Record { doc, views: BTreeMap::new() }
    }

    /// Write the record's tree; its view.
    pub fn write(&self, store: &dyn Store, alg: &'static Algorithm) -> Result<View> {
        let mut entries = BTreeMap::new();
        let doc = put(store, alg, "uni-txt", self.doc.text().as_bytes())?;
        entries.insert(Path::new(DOC)?, Node::File(doc));
        for (p, v) in &self.views {
            if p == DOC {
                return Err(Error::bad("record.uni is the record's document"));
            }
            entries.insert(Path::new(p.as_str())?, Node::File(v.clone()));
        }
        Ok(build(store, alg, by_alias("dir").unwrap(), &entries)?)
    }

    /// The record at `root`.
    pub fn read(store: &dyn Store, root: &View) -> Result<Record> {
        let mut r = Record::default();
        for item in walk(store, root, None) {
            let (path, node) = item?;
            let Node::File(v) = node else { continue };
            if path.as_str() == DOC {
                let text = String::from_utf8(read_blob(store, v.key())?)
                    .map_err(|_| Error::new(500, "record.uni is not UTF-8"))?;
                r.doc = Doc::of(&Value::from_text(&text).map_err(|e| Error::new(500, format!("record.uni: {e}")))?);
            } else {
                r.views.insert(path.as_str().to_string(), v);
            }
        }
        Ok(r)
    }
}

/// The view variable `var` holds, if set.
pub fn head(store: &dyn Store, var: &str) -> Result<Option<View>> {
    match store.read(var)? {
        None => Ok(None),
        Some(b) => {
            let s = String::from_utf8(b).map_err(|_| Error::new(500, format!("{var} is not UTF-8")))?;
            View::parse(&s).map(Some).map_err(|e| Error::new(500, format!("{var}: {e}")))
        }
    }
}

/// The record at `var`, with the view it was read from.
pub fn load(store: &dyn Store, var: &str) -> Result<Option<(View, Record)>> {
    match head(store, var)? {
        None => Ok(None),
        Some(v) => Ok(Some((v.clone(), Record::read(store, &v)?))),
    }
}

/// Write `r` and move `var` to it, only if it still holds `old`.
pub fn save(store: &dyn Store, alg: &'static Algorithm, var: &str, old: Option<&View>, r: &Record) -> Result<View> {
    let new = r.write(store, alg)?;
    let expect = old.map_or(Expect::Absent, |o| Expect::Value(o.to_string().into_bytes()));
    if old != Some(&new) {
        store.write(var, new.to_string().as_bytes(), &expect)?;
    }
    Ok(new)
}

/// Load `var`, let `f` change the record (`None` when unset), save;
/// retried while other writers win. `f` returns the new record, or `None`
/// to leave the variable as it is.
pub fn update<T>(
    store: &dyn Store,
    alg: &'static Algorithm,
    var: &str,
    mut f: impl FnMut(Option<Record>) -> Result<(Option<Record>, T)>,
) -> Result<T> {
    loop {
        let cur = load(store, var)?;
        let old = cur.as_ref().map(|(v, _)| v.clone());
        let (new, out) = f(cur.map(|(_, r)| r))?;
        let Some(new) = new else { return Ok(out) };
        match save(store, alg, var, old.as_ref(), &new) {
            Err(Error { status: 409, .. }) => continue,
            r => return r.map(|_| out),
        }
    }
}

/// Write `bytes` as a `raw` file; its view.
pub fn raw(store: &dyn Store, alg: &'static Algorithm, bytes: &[u8]) -> Result<View> {
    Ok(put(store, alg, "raw", bytes)?)
}
