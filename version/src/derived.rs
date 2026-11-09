//! Derived data (Stage 6, spec/derived.md): a cache of f(X) for
//! deterministic functions f. The result of function F over input X is a
//! blob, and the variable `derived/F/X` holds a view of it.
//!
//! A function is identified by the key of its function object, a `uni-bin`
//! map of its name, input format, output format and spec key. Changing a
//! function's definition gives a new id, so results never mix.

use crate::history::{History, NodeId};
use crate::uni::Value;
use crate::{Error, Path, Result, View, by_alias, malformed, object_at, read_blob, tree, write_blob};
use cas_core::algo::{self, Algorithm};
use cas_core::{Error as CoreError, Expect, Store};

type Compute = fn(&dyn Store, &'static Algorithm, &View) -> Result<Vec<u8>>;

pub struct Function {
    pub name: &'static str,
    /// Alias of the format inputs are in.
    pub input: &'static str,
    /// Alias of the format results are in: `uni-bin` unless the spec says
    /// otherwise.
    pub output: &'static str,
    pub spec: &'static str,
    pub(crate) compute: Compute,
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
            ("output", Value::str(by_alias(self.output).unwrap().id())),
            ("spec", Value::str(blake3().key(self.spec.as_bytes()))),
        ])
    }

    /// The key of the function object.
    pub fn id(&self) -> String {
        blake3().key(&self.object().serialize())
    }

    pub fn var(&self, input: &View) -> String {
        format!("derived/{}/{}", self.id(), input.key())
    }
}

/// Write the function object and its spec, so that its id can be looked up.
pub fn publish(store: &dyn Store, f: &Function) -> Result<()> {
    write_blob(store, blake3(), f.spec.as_bytes())?;
    write_blob(store, blake3(), &f.object().serialize())?;
    Ok(())
}

fn check_input(f: &Function, input: &View) -> Result<()> {
    if input.format() == by_alias(f.input).unwrap().id() {
        Ok(())
    } else {
        Err(Error::UnknownFormat(format!("{} takes {} input, not {}", f.name, f.input, input.format())))
    }
}

/// The cached result of `f` over `input`, if any.
pub fn cached(store: &dyn Store, f: &Function, input: &View) -> Result<Option<View>> {
    check_input(f, input)?;
    let var = f.var(input);
    match store.read(&var)? {
        None => Ok(None),
        Some(v) => {
            let s = String::from_utf8(v).map_err(|_| malformed(&var, "not UTF-8"))?;
            View::parse(&s).map(Some).map_err(|_| malformed(&var, "not a view"))
        }
    }
}

/// [`cached`], but a store that will not show its cache (a remote that
/// grants no reads of `derived/`) just has none.
pub fn readable_cache(store: &dyn Store, f: &Function, input: &View) -> Result<Option<View>> {
    match cached(store, f, input) {
        Err(Error::Store(CoreError::Denied)) => Ok(None),
        r => r,
    }
}

/// `f(input)`: the cached result, or else computed, stored, and recorded
/// under its variable. If another writer recorded a result first, that one is
/// returned; functions are deterministic, so it is the same value.
pub fn derive(store: &dyn Store, alg: &'static Algorithm, f: &Function, input: &View) -> Result<View> {
    if let Some(r) = cached(store, f, input)? {
        return Ok(r);
    }
    let bytes = (f.compute)(store, alg, input)?;
    let key = write_blob(store, alg, &bytes)?;
    let result = View { format: by_alias(f.output).unwrap().id().to_string(), key };
    match store.write(&f.var(input), result.to_string().as_bytes(), &Expect::Absent) {
        Ok(()) => Ok(result),
        Err(CoreError::Conflict) => Ok(cached(store, f, input)?.expect("a conflicting tag exists")),
        Err(e) => Err(e.into()),
    }
}

/// Read a derived result's value.
pub fn value(store: &dyn Store, r: &View) -> Result<Value> {
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
fn summary(store: &dyn Store, alg: &'static Algorithm, input: &View) -> Result<Vec<u8>> {
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
    Ok(Value::map([("dirs", count(dirs)), ("files", count(files))]).serialize())
}

pub static SUMMARY: Function = Function {
    name: "summary",
    input: "dir",
    output: "uni-bin",
    spec: include_str!("../../spec/functions/summary.md"),
    compute: summary,
};

/// The nodes of `h` whose step changed `path` (or anything under it),
/// newest first in log order. A node counts when what lies at `path`
/// differs from every predecessor's; a root counts when `path` exists in
/// it. Each test is one lookup per side, so the cost follows the depth of
/// `path`, not the size of the trees.
pub fn path_log(store: &dyn Store, h: &History, path: &Path) -> Result<Vec<NodeId>> {
    let at = |id: &NodeId| object_at(store, &id.version(), path);
    let mut out = Vec::new();
    for id in h.log() {
        let here = at(&id)?;
        let after = &h.nodes[&id].after;
        let hit = if after.is_empty() {
            here.is_some()
        } else {
            let mut changed = true;
            for p in after.keys() {
                if at(p)? == here {
                    changed = false;
                }
            }
            changed
        };
        if hit {
            out.push(id);
        }
    }
    Ok(out)
}

/// A history's tips, with the nodes each follows, and its tags: what most
/// commands need. The history's root holds the tips and tags, so this
/// reads the root, the parts above the leaves and one leaf per tip.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Heads {
    pub tips: std::collections::BTreeMap<NodeId, Vec<NodeId>>,
    pub tags: std::collections::BTreeMap<String, NodeId>,
}

impl Heads {
    pub fn of(h: &History) -> Heads {
        let tips = h.tips().into_iter().map(|t| {
            let after = h.nodes[&t].after.keys().cloned().collect();
            (t, after)
        });
        Heads { tips: tips.collect(), tags: h.tags.clone() }
    }

    /// Resolve a tag, `latest` (the single tip) or a tip's node id; `None`
    /// when only the whole history can answer.
    pub fn resolve(&self, what: &str) -> Option<NodeId> {
        if let Some(n) = self.tags.get(what) {
            return Some(n.clone());
        }
        if what == "latest" {
            return if self.tips.len() == 1 { self.tips.keys().next().cloned() } else { None };
        }
        self.tips.keys().find(|t| t.as_str() == what).cloned()
    }
}

/// The heads of the history at `input`.
pub fn heads(store: &dyn Store, input: &View) -> Result<Heads> {
    let st = crate::history::Stored::open(store, Some(input))?;
    let mut tips = std::collections::BTreeMap::new();
    for t in &st.root.tips {
        let n = st.get(t)?.ok_or_else(|| malformed(input.key(), format!("tip {t} is not a node")))?;
        tips.insert(t.clone(), n.after.into_keys().collect());
    }
    Ok(Heads { tips, tags: st.root.tags.clone() })
}

/// Record `bytes` as `f(input)`, computed by the caller. Best effort: a
/// refusal leaves the next reader to compute it.
pub fn record(store: &dyn Store, alg: &'static Algorithm, f: &Function, input: &View, bytes: &[u8]) {
    let Ok(key) = write_blob(store, alg, bytes) else { return };
    let result = View { format: by_alias(f.output).unwrap().id().to_string(), key };
    let _ = store.write(&f.var(input), result.to_string().as_bytes(), &Expect::Absent);
}
