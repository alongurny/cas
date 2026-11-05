//! Derived data (Stage 6, spec/derived.md): a cache of f(X) for
//! deterministic functions f. The result of function F over input X is a
//! blob, and the tag `derived/F/X` holds a reference to it.
//!
//! A function is identified by the key of its function object, a `uni-bin`
//! map of its name, input format, output format and spec key. Changing a
//! function's definition gives a new id, so results never mix.

use crate::history;
use crate::uni::Value;
use crate::{Change, Error, Path, Ref, Result, by_alias, diff, malformed, read_blob, tree, write_blob};
use cas_core::algo::{self, Algorithm};
use cas_core::{Error as CoreError, Expect, Store};
use std::collections::BTreeSet;

type Compute = fn(&dyn Store, &'static Algorithm, &Ref) -> Result<Value>;

pub struct Function {
    pub name: &'static str,
    /// Alias of the format inputs are in.
    pub input: &'static str,
    /// Results are `uni-bin` values.
    pub spec: &'static str,
    compute: Compute,
}

fn blake3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

impl Function {
    /// The function object.
    pub fn object(&self) -> Value {
        Value::map([
            ("input", Value::str(by_alias(self.input).unwrap().id())),
            ("name", Value::str(self.name)),
            ("output", Value::str(by_alias("uni-bin").unwrap().id())),
            ("spec", Value::str(blake3().key(self.spec.as_bytes()))),
        ])
    }

    /// The key of the function object.
    pub fn id(&self) -> String {
        blake3().key(&self.object().serialize())
    }

    pub fn tag(&self, input: &Ref) -> String {
        format!("derived/{}/{}", self.id(), input.key())
    }
}

/// Write the function object and its spec, so that its id can be looked up.
pub fn publish(store: &dyn Store, f: &Function) -> Result<()> {
    write_blob(store, blake3(), f.spec.as_bytes())?;
    write_blob(store, blake3(), &f.object().serialize())?;
    Ok(())
}

fn check_input(f: &Function, input: &Ref) -> Result<()> {
    if input.format() == by_alias(f.input).unwrap().id() {
        Ok(())
    } else {
        Err(Error::UnknownFormat(format!("{} takes {} input, not {}", f.name, f.input, input.format())))
    }
}

/// The cached result of `f` over `input`, if any.
pub fn cached(store: &dyn Store, f: &Function, input: &Ref) -> Result<Option<Ref>> {
    check_input(f, input)?;
    let tag = f.tag(input);
    match store.read(&tag)? {
        None => Ok(None),
        Some(v) => {
            let s = String::from_utf8(v).map_err(|_| malformed(&tag, "not UTF-8"))?;
            Ref::parse(&s).map(Some).map_err(|_| malformed(&tag, "not a reference"))
        }
    }
}

/// `f(input)`: the cached result, or else computed, stored, and recorded
/// under its tag. If another writer recorded a result first, that one is
/// returned; functions are deterministic, so it is the same value.
pub fn derive(store: &dyn Store, alg: &'static Algorithm, f: &Function, input: &Ref) -> Result<Ref> {
    if let Some(r) = cached(store, f, input)? {
        return Ok(r);
    }
    let value = (f.compute)(store, alg, input)?;
    let key = write_blob(store, alg, &value.serialize())?;
    let result = Ref { format: by_alias("uni-bin").unwrap().id().to_string(), key };
    match store.write(&f.tag(input), result.to_string().as_bytes(), &Expect::Absent) {
        Ok(()) => Ok(result),
        Err(CoreError::Conflict) => Ok(cached(store, f, input)?.expect("a conflicting tag exists")),
        Err(e) => Err(e.into()),
    }
}

/// Read a derived result's value.
pub fn value(store: &dyn Store, r: &Ref) -> Result<Value> {
    Value::deserialize(&read_blob(store, r.key())?).map_err(|e| malformed(r.key(), e))
}

fn count(n: u64) -> Value {
    Value::str(n.to_string())
}

fn number(v: &Value, field: &str) -> u64 {
    v.as_map().and_then(|m| m.get(&Value::str(field))).and_then(Value::as_str).and_then(|s| s.parse().ok()).unwrap()
}

/// `summary`: how many directories and files lie under a directory. Each
/// subdirectory's summary is itself derived, so after an edit only the
/// directories on the edited paths are recomputed.
fn summary(store: &dyn Store, alg: &'static Algorithm, input: &Ref) -> Result<Value> {
    let (mut dirs, mut files) = (0, 0);
    for item in tree::load(store, input)? {
        if item.child.is_dir() {
            let sub = value(store, &derive(store, alg, &SUMMARY, &item.child)?)?;
            dirs += 1 + number(&sub, "dirs");
            files += number(&sub, "files");
        } else {
            files += 1;
        }
    }
    Ok(Value::map([("dirs", count(dirs)), ("files", count(files))]))
}

pub static SUMMARY: Function =
    Function { name: "summary", input: "dir", spec: include_str!("../../spec/functions/summary.md"), compute: summary };

/// `changes`: the paths a record changed, compared with each of its
/// parents. A path counts when it differs from every parent's version.
fn changes(store: &dyn Store, _: &'static Algorithm, input: &Ref) -> Result<Value> {
    let rec = history::read(store, input)?;
    let paths = |d: Vec<Change>| d.into_iter().map(|c| c.path).collect::<BTreeSet<Path>>();
    let changed = if rec.parents.is_empty() {
        // A first record adds everything.
        tree::walk(store, &rec.version, None).map(|r| r.map(|(p, _)| p)).collect::<Result<BTreeSet<_>>>()?
    } else {
        let mut sets = Vec::new();
        for p in &rec.parents {
            sets.push(paths(diff(store, &history::read(store, p)?.version, &rec.version, None)?));
        }
        let first = sets.pop().unwrap();
        first.into_iter().filter(|p| sets.iter().all(|s| s.contains(p))).collect()
    };
    Ok(Value::Map(changed.into_iter().map(|p| (Value::str(p.as_str()), Value::empty())).collect()))
}

pub static CHANGES: Function = Function {
    name: "changes",
    input: "record",
    spec: include_str!("../../spec/functions/changes.md"),
    compute: changes,
};

/// The records from `head` back that changed `path` (or anything under
/// it), newest first in log order, using cached `changes`.
pub fn path_log(store: &dyn Store, alg: &'static Algorithm, head: &Ref, path: &Path) -> Result<Vec<Ref>> {
    let mut out = Vec::new();
    for (r, _) in history::log(store, head)? {
        let changed = value(store, &derive(store, alg, &CHANGES, &r)?)?;
        let hit = changed.as_map().is_some_and(|m| {
            m.keys().filter_map(Value::as_str).any(|p| Path::new(p).is_ok_and(|p| p.starts_with(path)))
        });
        if hit {
            out.push(r);
        }
    }
    Ok(out)
}
