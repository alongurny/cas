//! History records (Stage 5, spec/formats/record.md): each names a version
//! and the records it follows. Versions never refer to records. A branch is
//! a tag whose value is a reference to a record, moved by compare-and-swap.

use crate::uni::Value;
use crate::{Error, Ref, Result, by_alias, malformed, read_blob, write_blob};
use cas_core::algo::Algorithm;
use cas_core::{Expect, Store};
use std::collections::{BTreeMap, BTreeSet, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub version: Ref,
    pub parents: BTreeSet<Ref>,
    /// Free metadata, by convention `author`, `message`, `time` strings.
    pub meta: BTreeMap<Value, Value>,
}

fn record_format() -> &'static str {
    by_alias("record").unwrap().id()
}

impl Record {
    pub fn to_value(&self) -> Value {
        let parents = Value::Map(self.parents.iter().map(|p| (Value::str(p.to_string()), Value::empty())).collect());
        Value::map([
            ("meta", Value::Map(self.meta.clone())),
            ("parents", parents),
            ("version", Value::str(self.version.to_string())),
        ])
    }

    pub fn from_value(v: &Value) -> std::result::Result<Record, String> {
        let m = v.as_map().ok_or("a record is a map")?;
        let keys: Vec<Option<&str>> = m.keys().map(Value::as_str).collect();
        if keys != [Some("meta"), Some("parents"), Some("version")] {
            return Err("a record has exactly the keys meta, parents, version".into());
        }
        let field = |k: &str| &m[&Value::str(k)];
        let reference =
            |v: &Value| v.as_str().and_then(|s| Ref::parse(s).ok()).ok_or_else(|| format!("not a reference: {v}"));
        let meta = field("meta").as_map().ok_or("meta is a map")?.clone();
        let mut parents = BTreeSet::new();
        for (p, unit) in field("parents").as_map().ok_or("parents is a map")? {
            if *unit != Value::empty() {
                return Err("parents maps each reference to {}".into());
            }
            parents.insert(reference(p)?);
        }
        Ok(Record { version: reference(field("version"))?, parents, meta })
    }
}

/// Check the bytes of a `record` blob.
pub(crate) fn check(bytes: &[u8]) -> std::result::Result<(), String> {
    Record::from_value(&Value::deserialize(bytes)?).map(|_| ())
}

pub fn write(store: &dyn Store, alg: &'static Algorithm, record: &Record) -> Result<Ref> {
    let key = write_blob(store, alg, &record.to_value().serialize())?;
    Ok(Ref { format: record_format().to_string(), key })
}

pub fn read(store: &dyn Store, r: &Ref) -> Result<Record> {
    if r.format() != record_format() {
        return Err(Error::UnknownFormat(format!("{} is not the record format", r.format())));
    }
    let bytes = read_blob(store, r.key())?;
    let value = Value::deserialize(&bytes).map_err(|e| malformed(r.key(), e))?;
    Record::from_value(&value).map_err(|e| malformed(r.key(), e))
}

/// The record a branch tag points at, if the tag is set.
pub fn head(store: &dyn Store, tag: &str) -> Result<Option<Ref>> {
    match store.read(tag)? {
        None => Ok(None),
        Some(v) => {
            let s = String::from_utf8(v).map_err(|_| malformed(tag, "tag value is not UTF-8"))?;
            Ref::parse(&s).map(Some).map_err(|_| malformed(tag, "tag value is not a reference"))
        }
    }
}

/// Record `version` on the branch `tag`, following its current head, and
/// move the tag. Fails with a store conflict if the tag moved meanwhile.
pub fn commit(
    store: &dyn Store,
    alg: &'static Algorithm,
    tag: &str,
    version: &Ref,
    meta: BTreeMap<Value, Value>,
) -> Result<Ref> {
    let current = head(store, tag)?;
    let record = Record { version: version.clone(), parents: current.iter().cloned().collect(), meta };
    let new = write(store, alg, &record)?;
    let expect = match &current {
        Some(r) => Expect::Value(r.to_string().into_bytes()),
        None => Expect::Absent,
    };
    store.write(tag, new.to_string().as_bytes(), &expect)?;
    Ok(new)
}

/// `head` and every record before it, each once. A record always comes
/// before its parents; among records that are free to go next, the one
/// with the smallest reference goes first, so the order is deterministic.
pub fn log(store: &dyn Store, head: &Ref) -> Result<Vec<(Ref, Record)>> {
    let mut records = BTreeMap::new();
    let mut todo = vec![head.clone()];
    while let Some(r) = todo.pop() {
        if records.contains_key(&r) {
            continue;
        }
        let rec = read(store, &r)?;
        todo.extend(rec.parents.iter().cloned());
        records.insert(r, rec);
    }
    // Kahn's algorithm, counting children per record.
    let mut children: BTreeMap<&Ref, usize> = records.keys().map(|r| (r, 0)).collect();
    for rec in records.values() {
        for p in &rec.parents {
            *children.get_mut(p).unwrap() += 1;
        }
    }
    let mut ready: BTreeSet<&Ref> = children.iter().filter(|(_, n)| **n == 0).map(|(r, _)| *r).collect();
    let mut out = Vec::new();
    while let Some(r) = ready.pop_first() {
        let rec = &records[r];
        for p in &rec.parents {
            let n = children.get_mut(p).unwrap();
            *n -= 1;
            if *n == 0 {
                ready.insert(p);
            }
        }
        out.push((r.clone(), rec.clone()));
    }
    Ok(out)
}

/// The best common ancestors of `a` and `b`: records that both reach (each
/// reaches itself) and that no other common ancestor follows.
pub fn merge_bases(store: &dyn Store, a: &Ref, b: &Ref) -> Result<BTreeSet<Ref>> {
    let la = log(store, a)?;
    let in_b: HashSet<Ref> = log(store, b)?.into_iter().map(|(r, _)| r).collect();
    let common: BTreeMap<Ref, Record> = la.into_iter().filter(|(r, _)| in_b.contains(r)).collect();
    // Drop every common ancestor that another one reaches.
    let mut beaten = HashSet::new();
    for rec in common.values() {
        let mut todo: Vec<Ref> = rec.parents.iter().cloned().collect();
        while let Some(r) = todo.pop() {
            if beaten.insert(r.clone())
                && let Some(c) = common.get(&r)
            {
                todo.extend(c.parents.iter().cloned());
            }
        }
    }
    Ok(common.into_keys().filter(|r| !beaten.contains(r)).collect())
}
