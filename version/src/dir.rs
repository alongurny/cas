//! The `dir` format (spec/formats/dir.md): lines of `<format> <key> <name>`,
//! names strictly ascending. The empty blob is the empty directory.

use crate::{Result, View, is_blob_key, malformed, valid_name};

/// One entry of a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub child: View,
}

pub struct Dir;

pub static DIR: Dir = Dir;

impl Dir {
    pub fn encode(&self, items: &[Item]) -> Vec<u8> {
        let mut out = String::new();
        for it in items {
            out.push_str(&format!("{} {}\n", it.child, it.name));
        }
        out.into_bytes()
    }

    /// The entry `name` of a directory, without building the others: one
    /// pass that checks every line as [`Dir::decode`] does but allocates
    /// only the answer.
    pub fn find(&self, key: &str, bytes: &[u8], name: &str) -> Result<Option<View>> {
        let text = std::str::from_utf8(bytes).map_err(|_| malformed(key, "not UTF-8"))?;
        let mut prev: Option<&str> = None;
        let mut prev_format: Option<&str> = None;
        let mut found = None;
        for line in text.split_inclusive('\n') {
            let line = line.strip_suffix('\n').ok_or_else(|| malformed(key, "last line has no newline"))?;
            let mut parts = line.splitn(3, ' ');
            let (Some(f), Some(k), Some(n)) = (parts.next(), parts.next(), parts.next()) else {
                return Err(malformed(key, format!("bad line {line:?}")));
            };
            // Directories hold few formats: a format seen on the line before
            // is already checked.
            if !(prev_format == Some(f) || is_blob_key(f)) || !is_blob_key(k) {
                return Err(malformed(key, format!("bad view {f:?} {k:?}")));
            }
            if !valid_name(n) {
                return Err(malformed(key, format!("bad name {n:?}")));
            }
            if prev.is_some_and(|p| p >= n) {
                return Err(malformed(key, format!("{n:?} out of order")));
            }
            prev = Some(n);
            prev_format = Some(f);
            if n == name {
                found = Some(View { format: f.to_string(), key: k.to_string() });
            }
        }
        Ok(found)
    }

    /// Parse a directory, accepting only the canonical encoding.
    pub fn decode(&self, key: &str, bytes: &[u8]) -> Result<Vec<Item>> {
        let text = std::str::from_utf8(bytes).map_err(|_| malformed(key, "not UTF-8"))?;
        let mut items: Vec<Item> = Vec::new();
        for line in text.split_inclusive('\n') {
            let line = line.strip_suffix('\n').ok_or_else(|| malformed(key, "last line has no newline"))?;
            let mut parts = line.splitn(3, ' ');
            let (Some(f), Some(k), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
                return Err(malformed(key, format!("bad line {line:?}")));
            };
            // Directories hold few formats: a format seen on the line before
            // is already checked.
            let same_format = items.last().is_some_and(|p| p.child.format == f);
            if !(same_format || is_blob_key(f)) || !is_blob_key(k) {
                return Err(malformed(key, format!("bad view {f:?} {k:?}")));
            }
            if !valid_name(name) {
                return Err(malformed(key, format!("bad name {name:?}")));
            }
            if items.last().is_some_and(|prev| prev.name.as_str() >= name) {
                return Err(malformed(key, format!("{name:?} out of order")));
            }
            items.push(Item { name: name.to_string(), child: View { format: f.to_string(), key: k.to_string() } });
        }
        Ok(items)
    }
}
