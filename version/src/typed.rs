//! Formats given as types (spec/types.md). A format here is a [`Type`]
//! value; its parser, its printer, its check and the views it holds all
//! follow from that value, so no format has code of its own.
//!
//! A blob is a whole number of 64-byte blocks. A record is laid out as its
//! keys (32 bytes each), then one tag byte for each choice, then its
//! integers (8 bytes each, most significant first), then its name if it has
//! one (a length byte and the bytes), then zeros to the next block. A blob
//! is a head record, a list of records, or a head followed by a list.

use crate::formats::by_alias;
use crate::{View, valid_name};
use std::sync::LazyLock;

/// A key field. It names a blob of one of `targets`; with several targets a
/// tag byte among the record's tags says which.
pub struct KeyF {
    pub name: &'static str,
    pub targets: &'static [&'static str],
    /// Whether the key may be absent (32 zero bytes).
    pub optional: bool,
}

/// A choice among kinds that is not the target of a key: one tag byte.
pub struct ChoiceF {
    pub name: &'static str,
    pub kinds: &'static [&'static str],
}

pub struct IntF {
    pub name: &'static str,
    /// Whether the value is at least 1.
    pub positive: bool,
}

pub struct Rec {
    pub keys: Vec<KeyF>,
    pub choices: Vec<ChoiceF>,
    pub ints: Vec<IntF>,
    /// Whether the record ends in a name.
    pub name: bool,
}

/// How the records of a list relate to their neighbours.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Order {
    /// Strictly ascending: by name when the record has one, else by its
    /// first key.
    Sorted,
    /// Neighbours differ in their first key.
    Runs,
}

pub struct Type {
    pub alias: &'static str,
    pub head: Option<Rec>,
    pub list: Option<(Rec, Order)>,
    /// Whether the list has at least one record.
    pub nonempty: bool,
}

/// A record's value. `kinds` holds the tag of each key that has several
/// targets, in key order, then the tag of each choice; tags count from 1.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecV {
    pub keys: Vec<[u8; 32]>,
    pub kinds: Vec<u8>,
    pub ints: Vec<u64>,
    pub name: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Value {
    pub head: Option<RecV>,
    pub list: Vec<RecV>,
}

pub const NONE: [u8; 32] = [0; 32];

impl Rec {
    /// The number of tag bytes.
    fn tags(&self) -> impl Iterator<Item = usize> + '_ {
        self.keys
            .iter()
            .filter(|k| k.targets.len() > 1)
            .map(|k| k.targets.len())
            .chain(self.choices.iter().map(|c| c.kinds.len()))
    }

    fn parse(&self, b: &[u8], p: &mut usize) -> Result<RecV, String> {
        let mut take = |n: usize| -> Result<&[u8], String> {
            let s = b.get(*p..*p + n).ok_or("a record is cut short")?;
            *p += n;
            Ok(s)
        };
        let mut v = RecV::default();
        for k in &self.keys {
            let key: [u8; 32] = take(32)?.try_into().unwrap();
            if key == NONE && !k.optional {
                return Err(format!("{} is absent", k.name));
            }
            v.keys.push(key);
        }
        for n in self.tags() {
            let t = take(1)?[0];
            if t == 0 || t as usize > n {
                return Err(format!("tag {t} is not one of {n} kinds"));
            }
            v.kinds.push(t);
        }
        for i in &self.ints {
            let x = u64::from_be_bytes(take(8)?.try_into().unwrap());
            if i.positive && x == 0 {
                return Err(format!("{} is zero", i.name));
            }
            v.ints.push(x);
        }
        if self.name {
            let len = take(1)?[0] as usize;
            v.name = take(len)?.to_vec();
            if !std::str::from_utf8(&v.name).is_ok_and(valid_name) {
                return Err("a name is not valid".into());
            }
        }
        *p = p.div_ceil(64) * 64;
        Ok(v)
    }

    fn print(&self, v: &RecV, o: &mut Vec<u8>) {
        for k in &v.keys {
            o.extend_from_slice(k);
        }
        o.extend_from_slice(&v.kinds);
        for x in &v.ints {
            o.extend_from_slice(&x.to_be_bytes());
        }
        if self.name {
            o.push(v.name.len() as u8);
            o.extend_from_slice(&v.name);
        }
        o.resize(o.len().div_ceil(64) * 64, 0);
    }

    /// The views a record holds: each present key, read as the format its
    /// tag (or its only target) names.
    fn views(&self, v: &RecV, out: &mut Vec<View>) {
        let mut tags = v.kinds.iter();
        for (k, key) in self.keys.iter().zip(&v.keys) {
            let target = if k.targets.len() > 1 { k.targets[*tags.next().unwrap() as usize - 1] } else { k.targets[0] };
            if *key != NONE {
                let format = by_alias(target).expect("a key's target is a known format").id();
                out.push(View::new(format, key_text(key)).expect("valid keys"));
            }
        }
    }
}

/// The store key of 32 key bytes.
pub fn key_text(key: &[u8; 32]) -> String {
    {
        let mut s = String::from("blake3:");
        for b in key {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }
}

/// The 32 key bytes of a blake3 store key.
pub fn key_bytes(key: &str) -> Option<[u8; 32]> {
    {
        let h = key.strip_prefix("blake3:")?;
        if h.len() != 64 || !h.is_ascii() {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, o) in out.iter_mut().enumerate() {
            *o = u8::from_str_radix(&h[2 * i..2 * i + 2], 16).ok()?;
        }
        Some(out)
    }
}

impl Type {
    /// The value `b` holds. Reading is not the whole check: a blob is valid
    /// when it also prints back to the same bytes ([`Type::check`]).
    pub fn parse(&self, b: &[u8]) -> Result<Value, String> {
        if !b.len().is_multiple_of(64) {
            return Err("not a whole number of 64-byte blocks".into());
        }
        let mut p = 0;
        let head = self.head.as_ref().map(|h| h.parse(b, &mut p)).transpose()?;
        let mut list: Vec<RecV> = Vec::new();
        if let Some((r, order)) = &self.list {
            while p < b.len() {
                let v = r.parse(b, &mut p)?;
                if let Some(l) = list.last() {
                    let ok = match (order, r.name) {
                        (Order::Sorted, true) => l.name < v.name,
                        (Order::Sorted, false) => l.keys[0] < v.keys[0],
                        (Order::Runs, _) => l.keys[0] != v.keys[0],
                    };
                    if !ok {
                        return Err(if *order == Order::Sorted {
                            "records are not in order"
                        } else {
                            "two neighbours are equal"
                        }
                        .into());
                    }
                }
                list.push(v);
            }
            if self.nonempty && list.is_empty() {
                return Err("the list is empty".into());
            }
        }
        if p != b.len() {
            return Err("bytes follow the value".into());
        }
        Ok(Value { head, list })
    }

    /// The one encoding of `v`. `v` must fit the type.
    pub fn print(&self, v: &Value) -> Vec<u8> {
        let mut o = Vec::new();
        if let (Some(h), Some(hv)) = (&self.head, &v.head) {
            h.print(hv, &mut o);
        }
        if let Some((r, _)) = &self.list {
            for e in &v.list {
                r.print(e, &mut o);
            }
        }
        o
    }

    /// Whether `b` is valid: it parses, and the value prints back to `b`.
    pub fn check(&self, b: &[u8]) -> Result<(), String> {
        if self.print(&self.parse(b)?) == b { Ok(()) } else { Err("not the one encoding of its value".into()) }
    }

    /// The views a valid blob holds.
    pub fn children(&self, b: &[u8]) -> Result<Vec<View>, String> {
        let v = self.parse(b)?;
        let mut out = Vec::new();
        if let (Some(h), Some(hv)) = (&self.head, &v.head) {
            h.views(hv, &mut out);
        }
        if let Some((r, _)) = &self.list {
            for e in &v.list {
                r.views(e, &mut out);
            }
        }
        Ok(out)
    }
}

/// `text`: UTF-8 with no control characters.
pub fn check_text(b: &[u8]) -> Result<(), String> {
    match std::str::from_utf8(b) {
        Ok(s) if !s.chars().any(char::is_control) => Ok(()),
        Ok(_) => Err("text has a control character".into()),
        Err(_) => Err("text is not UTF-8".into()),
    }
}

/// `semver`: a version as Semantic Versioning 2.0.0 writes it.
pub fn check_semver(b: &[u8]) -> Result<(), String> {
    let s = std::str::from_utf8(b).map_err(|_| "a tag is not UTF-8")?;
    let numeric = |x: &str| !x.is_empty() && x.bytes().all(|c| c.is_ascii_digit()) && (x == "0" || !x.starts_with('0'));
    let ident = |x: &str| !x.is_empty() && x.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-');
    let (rest, build) = match s.split_once('+') {
        Some((r, b)) => (r, Some(b)),
        None => (s, None),
    };
    let (core, pre) = match rest.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (rest, None),
    };
    let nums: Vec<&str> = core.split('.').collect();
    let ok = nums.len() == 3
        && nums.iter().all(|n| numeric(n))
        && pre.is_none_or(|p| p.split('.').all(|i| ident(i) && (!i.bytes().all(|c| c.is_ascii_digit()) || numeric(i))))
        && build.is_none_or(|b| b.split('.').all(ident));
    if ok { Ok(()) } else { Err("a tag is not a semantic version".into()) }
}

fn key(name: &'static str, targets: &'static [&'static str]) -> KeyF {
    KeyF { name, targets, optional: false }
}
fn maybe(name: &'static str, targets: &'static [&'static str]) -> KeyF {
    KeyF { name, targets, optional: true }
}

/// The kinds of change between two releases, in tag order.
pub const CHANGES: &[&str] = &["breaking", "feature", "patch", "security"];

static TYPES: LazyLock<Vec<Type>> = LazyLock::new(|| {
    vec![
        // directory = sorted<{ child: key<raw | symlink | directory>, name: name }> by name
        Type {
            alias: "directory",
            head: None,
            list: Some((
                Rec {
                    keys: vec![key("child", &["raw", "symlink", "directory"])],
                    choices: vec![],
                    ints: vec![],
                    name: true,
                },
                Order::Sorted,
            )),
            nonempty: false,
        },
        // commit = { version: key<directory>, author: key<author>, notes: key<annotation>?, time: time,
        //            parents: sorted<{ commit: key<commit>, step: key<annotation>? }> by commit }
        Type {
            alias: "commit",
            head: Some(Rec {
                keys: vec![key("version", &["directory"]), key("author", &["author"]), maybe("notes", &["annotation"])],
                choices: vec![],
                ints: vec![IntF { name: "time", positive: false }],
                name: false,
            }),
            list: Some((
                Rec {
                    keys: vec![key("commit", &["commit"]), maybe("step", &["annotation"])],
                    choices: vec![],
                    ints: vec![],
                    name: false,
                },
                Order::Sorted,
            )),
            nonempty: false,
        },
        // annotation = { note: key<note>?, entries: sorted<{ child: key<annotation | lines>, name: name }> by name }
        Type {
            alias: "annotation",
            head: Some(Rec { keys: vec![maybe("note", &["note"])], choices: vec![], ints: vec![], name: false }),
            list: Some((
                Rec { keys: vec![key("child", &["annotation", "lines"])], choices: vec![], ints: vec![], name: true },
                Order::Sorted,
            )),
            nonempty: false,
        },
        // lines = { note: key<note>?, old: key<runs>?, new: key<runs>? }
        Type {
            alias: "lines",
            head: Some(Rec {
                keys: vec![maybe("note", &["note"]), maybe("old", &["runs"]), maybe("new", &["runs"])],
                choices: vec![],
                ints: vec![],
                name: false,
            }),
            list: None,
            nonempty: false,
        },
        // runs = runs<{ note: key<note>?, count: integer }> by note
        Type {
            alias: "runs",
            head: None,
            list: Some((
                Rec {
                    keys: vec![maybe("note", &["note"])],
                    choices: vec![],
                    ints: vec![IntF { name: "count", positive: true }],
                    name: false,
                },
                Order::Runs,
            )),
            nonempty: true,
        },
        // release = { version: key<directory>, tag: key<tag>,
        //             from: sorted<{ release: key<release>, changelog: key<note>, change }> by release }
        Type {
            alias: "release",
            head: Some(Rec {
                keys: vec![key("version", &["directory"]), key("tag", &["tag"])],
                choices: vec![],
                ints: vec![],
                name: false,
            }),
            list: Some((
                Rec {
                    keys: vec![key("release", &["release"]), key("changelog", &["note"])],
                    choices: vec![ChoiceF { name: "change", kinds: CHANGES }],
                    ints: vec![],
                    name: false,
                },
                Order::Sorted,
            )),
            nonempty: false,
        },
    ]
});

/// The type of the format with this alias, if it is given as a type.
pub fn by_type(alias: &str) -> Option<&'static Type> {
    TYPES.iter().find(|t| t.alias == alias)
}

fn of(alias: &str) -> &'static Type {
    by_type(alias).expect("a typed format")
}

pub fn check_directory(b: &[u8]) -> Result<(), String> {
    of("directory").check(b)
}
pub fn check_commit(b: &[u8]) -> Result<(), String> {
    of("commit").check(b)
}
pub fn check_annotation(b: &[u8]) -> Result<(), String> {
    of("annotation").check(b)
}
pub fn check_lines(b: &[u8]) -> Result<(), String> {
    of("lines").check(b)
}
pub fn check_runs(b: &[u8]) -> Result<(), String> {
    of("runs").check(b)
}
pub fn check_release(b: &[u8]) -> Result<(), String> {
    of("release").check(b)
}
