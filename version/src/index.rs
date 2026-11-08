//! The stat cache for a working directory (spec/performance.md, step 4):
//! git's index without staging. For each file it remembers the stat data
//! seen when the file was last hashed, and the view it hashed to. A commit
//! rehashes only files whose stat data changed; the rest reuse their view.
//!
//! The cache lives in the directory as [`FILE`], which import skips. It is
//! only a cache: losing or damaging it costs a full rehash, nothing else.
//! Racy-git rule: a file whose modification time is not older than the
//! moment the cache was written may have changed within the same clock
//! tick, so it is rehashed.

use crate::View;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path as FsPath;
use std::time::SystemTime;

/// The cache's name inside the working directory.
pub const FILE: &str = ".cas-index";
const MAGIC: &[u8; 10] = b"CASINDEX1\n";

/// The stat data that must match for a file to count as unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    pub mtime: (i64, i64),
    pub ctime: (i64, i64),
    pub size: u64,
    pub ino: u64,
    pub mode: u32,
}

impl Stat {
    pub fn of(m: &fs::Metadata) -> Stat {
        Stat {
            mtime: (m.mtime(), m.mtime_nsec()),
            ctime: (m.ctime(), m.ctime_nsec()),
            size: m.size(),
            ino: m.ino(),
            mode: m.mode(),
        }
    }
}

/// The cache: path (as in the version) to stat data and view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Index {
    /// The store the views are in; a cache for another store is ignored.
    pub scope: String,
    /// When the cache was written, as (seconds, nanoseconds).
    pub written: (i64, i64),
    pub files: BTreeMap<String, (Stat, View)>,
}

fn now() -> (i64, i64) {
    let d = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    (d.as_secs() as i64, d.subsec_nanos() as i64)
}

impl Index {
    pub fn new(scope: &str) -> Index {
        Index { scope: scope.to_string(), ..Index::default() }
    }

    /// The cached view of `path` if its stat data still matches and it is
    /// not racily clean.
    pub fn fresh(&self, path: &str, stat: &Stat) -> Option<&View> {
        let (s, v) = self.files.get(path)?;
        (s == stat && stat.mtime < self.written).then_some(v)
    }

    /// The cache in `dir` for `scope`, or an empty one when there is none,
    /// it is damaged, or it is for another store.
    pub fn load(dir: &FsPath, scope: &str) -> Index {
        fs::read(dir.join(FILE))
            .ok()
            .and_then(|b| Index::decode(&b))
            .filter(|i| i.scope == scope)
            .unwrap_or_else(|| Index::new(scope))
    }

    /// Write the cache into `dir`, stamped with the current time. Not
    /// synced: it is a cache. Failures are ignored for the same reason.
    pub fn save(&mut self, dir: &FsPath) {
        self.written = now();
        let tmp = dir.join(format!("{FILE}.{}", std::process::id()));
        if fs::write(&tmp, self.encode()).is_ok() && fs::rename(&tmp, dir.join(FILE)).is_err() {
            let _ = fs::remove_file(&tmp);
        }
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        let s = |out: &mut Vec<u8>, s: &str| {
            out.extend_from_slice(&(s.len() as u32).to_be_bytes());
            out.extend_from_slice(s.as_bytes());
        };
        s(&mut out, &self.scope);
        out.extend_from_slice(&self.written.0.to_be_bytes());
        out.extend_from_slice(&self.written.1.to_be_bytes());
        out.extend_from_slice(&(self.files.len() as u64).to_be_bytes());
        for (path, (st, v)) in &self.files {
            s(&mut out, path);
            for n in [st.mtime.0, st.mtime.1, st.ctime.0, st.ctime.1] {
                out.extend_from_slice(&n.to_be_bytes());
            }
            out.extend_from_slice(&st.size.to_be_bytes());
            out.extend_from_slice(&st.ino.to_be_bytes());
            out.extend_from_slice(&st.mode.to_be_bytes());
            s(&mut out, v.format());
            s(&mut out, v.key());
        }
        out
    }

    fn decode(b: &[u8]) -> Option<Index> {
        let mut c = Cursor { b, at: 0 };
        if c.take(MAGIC.len())? != MAGIC {
            return None;
        }
        let scope = c.str()?;
        let written = (c.i64()?, c.i64()?);
        let n = c.u64()?;
        let mut files = BTreeMap::new();
        for _ in 0..n {
            let path = c.str()?;
            let st = Stat {
                mtime: (c.i64()?, c.i64()?),
                ctime: (c.i64()?, c.i64()?),
                size: c.u64()?,
                ino: c.u64()?,
                mode: u32::from_be_bytes(c.take(4)?.try_into().ok()?),
            };
            let v = View::new(c.str()?, c.str()?).ok()?;
            files.insert(path, (st, v));
        }
        (c.at == b.len()).then_some(Index { scope, written, files })
    }
}

struct Cursor<'a> {
    b: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let s = self.b.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }
    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }
    fn str(&mut self) -> Option<String> {
        let n = u32::from_be_bytes(self.take(4)?.try_into().ok()?) as usize;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
}
