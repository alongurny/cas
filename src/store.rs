//! The four core ops with spec semantics and outcome order (spec sections 5
//! and 6): gate, key syntax, store policy, content checks, then state.

use crate::algo::{self, Algorithm};
use crate::backend::Backend;
use crate::gate::{Gate, Op, Who};
use crate::key::{Parsed, classify};
use crate::policy::Policy;
use crate::types::{Error, Expect, Invalid, ListPage, Result, Span};
use std::sync::Arc;

/// The core ops as seen by one caller. Store, server and cache all speak this.
pub trait Store: Send + Sync {
    fn read(&self, key: &str) -> Result<Option<Vec<u8>>>;
    fn write(&self, key: &str, value: &[u8], expect: &Expect) -> Result<()>;
    fn remove(&self, key: &str, expect: &Expect) -> Result<()>;
    /// The first part of `span`; `ListPage::rest` holds what is left.
    fn list(&self, span: &Span) -> Result<ListPage>;

    // Batches (spec/wire.md, "Batches"): the same ops on many keys at once,
    // each decided as if made alone. Stores that can do better (one round
    // trip, one fsync) override these.

    /// Whether the store holds each of `keys`: a `list` of a span holding
    /// only that key, so it is gated as that list.
    fn has_many(&self, keys: &[String]) -> Result<Vec<bool>> {
        keys.iter().map(|k| has_by_list(self, k)).collect()
    }

    /// `read` of each key, in order.
    fn read_many(&self, keys: &[String]) -> Vec<Result<Option<Vec<u8>>>> {
        keys.iter().map(|k| self.read(k)).collect()
    }

    /// `write` of each, in order, stopping at the first that fails: the
    /// writes before it are done, it and the rest are not.
    fn write_many(&self, puts: &[Put]) -> std::result::Result<(), (usize, Error)> {
        for (i, p) in puts.iter().enumerate() {
            self.write(&p.key, &p.value, &p.expect).map_err(|e| (i, e))?;
        }
        Ok(())
    }

    /// Every key in `span`: `list` pages, following continuations. A local
    /// store answers in one pass instead (the same gate, no page limit).
    fn list_all(&self, span: &Span) -> Result<Vec<String>> {
        let mut keys = Vec::new();
        let mut span = span.clone();
        loop {
            let page = self.list(&span)?;
            keys.extend(page.keys);
            match page.rest {
                Some(rest) => span = rest,
                None => return Ok(keys),
            }
        }
    }
}

/// One write of a batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Put {
    pub key: String,
    pub value: Vec<u8>,
    pub expect: Expect,
}

impl Put {
    /// A blob write (expect absent).
    pub fn blob(key: impl Into<String>, value: Vec<u8>) -> Put {
        Put { key: key.into(), value, expect: Expect::Absent }
    }
}

/// Whether `store` holds `key`, by listing the span `[key, key + "\0")`.
pub fn has_by_list<S: Store + ?Sized>(store: &S, key: &str) -> Result<bool> {
    let mut end = key.as_bytes().to_vec();
    end.push(0);
    let page = store.list(&Span { from: Some(key.as_bytes().to_vec()), to: Some(end) })?;
    Ok(page.keys.iter().any(|k| k == key))
}

/// Every key in `span` ([`Store::list_all`]).
pub fn list_all(store: &dyn Store, span: &Span) -> Result<Vec<String>> {
    store.list_all(span)
}

enum Kind {
    Blob(&'static Algorithm),
    Var,
}

pub struct CoreStore {
    backend: Box<dyn Backend>,
    gate: Arc<dyn Gate>,
    policy: Policy,
}

impl CoreStore {
    pub fn new(backend: impl Backend + 'static, gate: impl Gate + 'static, policy: Policy) -> Arc<CoreStore> {
        CoreStore::with_gate(backend, Arc::new(gate), policy)
    }

    pub fn with_gate(backend: impl Backend + 'static, gate: Arc<dyn Gate>, policy: Policy) -> Arc<CoreStore> {
        Arc::new(CoreStore { backend: Box::new(backend), gate, policy })
    }

    pub fn session(self: &Arc<Self>, who: Who) -> Session {
        Session { store: Arc::clone(self), who }
    }

    fn gate(&self, who: &Who, op: Op<'_>) -> Result<()> {
        if self.gate.allow(who, &op) { Ok(()) } else { Err(Error::Denied) }
    }

    /// Syntax, then policy, then digest length. Shared by every keyed op.
    fn check_key(&self, key: &str) -> Result<Kind> {
        let parsed = classify(key).ok_or(Error::Invalid(Invalid::Key))?;
        if let Some(max) = self.policy.max_key_len
            && key.len() > max
        {
            return Err(Error::refused("key-too-long", format!("keys are limited to {max} bytes")));
        }
        match parsed {
            Parsed::Var => Ok(Kind::Var),
            Parsed::Blob { alg, digest_hex } => {
                let algorithm =
                    algo::lookup(alg).filter(|a| self.policy.algorithms.contains(a.name)).ok_or_else(|| {
                        Error::refused("unsupported-algorithm", format!("this store does not accept {alg}"))
                    })?;
                if digest_hex.len() != algorithm.digest_len * 2 {
                    return Err(Error::Invalid(Invalid::Key));
                }
                Ok(Kind::Blob(algorithm))
            }
        }
    }

    fn check_size(&self, kind: &Kind, value: &[u8]) -> Result<()> {
        let max = match kind {
            Kind::Blob(_) => self.policy.max_blob_len,
            Kind::Var => self.policy.max_var_len,
        };
        match max {
            Some(max) if value.len() > max => {
                Err(Error::refused("too-large", format!("values are limited to {max} bytes")))
            }
            _ => Ok(()),
        }
    }

    fn read(&self, who: &Who, key: &str) -> Result<Option<Vec<u8>>> {
        self.gate(who, Op::Read { key })?;
        let kind = self.check_key(key)?;
        let value = self.backend.get(key)?;
        if let (Kind::Blob(a), Some(v)) = (&kind, &value)
            && a.key(v) != key
        {
            return Err(Error::Unavailable(format!("stored copy of {key} is corrupt")));
        }
        Ok(value)
    }

    fn write(&self, who: &Who, key: &str, value: &[u8], expect: &Expect) -> Result<()> {
        self.gate(who, Op::Write { key, value, expect })?;
        let kind = self.check_key(key)?;
        self.check_size(&kind, value)?;
        match kind {
            Kind::Blob(a) => {
                if a.key(value) != key {
                    return Err(Error::Invalid(Invalid::HashMismatch));
                }
                if let Expect::Value(e) = expect
                    && e.as_slice() != value
                {
                    return Err(Error::Invalid(Invalid::Expect));
                }
                Ok(self.backend.put_blob(key, value)?)
            }
            Kind::Var => {
                if !is_view(value) {
                    return Err(Error::Invalid(Invalid::View));
                }
                if self.backend.cas_var(key, expect, Some(value))? { Ok(()) } else { Err(Error::Conflict) }
            }
        }
    }

    /// Check a blob write as `write` would, without storing it.
    fn check_blob_write(&self, who: &Who, key: &str, value: &[u8], expect: &Expect) -> Result<bool> {
        self.gate(who, Op::Write { key, value, expect })?;
        let kind = self.check_key(key)?;
        self.check_size(&kind, value)?;
        match kind {
            Kind::Blob(a) => {
                if a.key(value) != key {
                    return Err(Error::Invalid(Invalid::HashMismatch));
                }
                if let Expect::Value(e) = expect
                    && e.as_slice() != value
                {
                    return Err(Error::Invalid(Invalid::Expect));
                }
                Ok(true)
            }
            Kind::Var => Ok(false),
        }
    }

    /// `write` of each, in order. Runs of blob writes go to the backend as
    /// one durable batch; a variable write first flushes the blobs before it.
    fn write_many(&self, who: &Who, puts: &[Put]) -> std::result::Result<(), (usize, Error)> {
        // The pending run of blobs and the index of its first; a failed
        // flush is reported there, as nothing in the run is known written.
        let mut run: Vec<(&str, &[u8])> = Vec::new();
        let mut start = 0;
        let flush = |run: &mut Vec<(&str, &[u8])>, start: usize| -> std::result::Result<(), (usize, Error)> {
            let r = self.backend.put_blobs(run).map_err(|e| (start, Error::from(e)));
            run.clear();
            r
        };
        for (i, p) in puts.iter().enumerate() {
            match self.check_blob_write(who, &p.key, &p.value, &p.expect) {
                Ok(true) => {
                    if run.is_empty() {
                        start = i;
                    }
                    run.push((&p.key, &p.value));
                }
                Ok(false) => {
                    flush(&mut run, start)?;
                    self.write(who, &p.key, &p.value, &p.expect).map_err(|e| (i, e))?;
                }
                Err(e) => {
                    flush(&mut run, start)?;
                    return Err((i, e));
                }
            }
        }
        flush(&mut run, start)
    }

    fn remove(&self, who: &Who, key: &str, expect: &Expect) -> Result<()> {
        self.gate(who, Op::Remove { key, expect })?;
        match self.check_key(key)? {
            Kind::Blob(a) => {
                if let Expect::Value(e) = expect
                    && a.key(e) != key
                {
                    return Err(Error::Invalid(Invalid::Expect));
                }
                Ok(self.backend.remove_blob(key)?)
            }
            Kind::Var => {
                if self.backend.cas_var(key, expect, None)? {
                    Ok(())
                } else {
                    Err(Error::Conflict)
                }
            }
        }
    }

    fn list(&self, who: &Who, span: &Span) -> Result<ListPage> {
        self.gate(who, Op::List { span })?;
        let limit = self.policy.list_page.max(1);
        let keys = self.backend.list(span, limit)?;
        let rest = match keys.last() {
            Some(last) if keys.len() == limit => Some(span.after(last.as_bytes())),
            _ => None,
        };
        Ok(ListPage { keys, rest })
    }

    /// All of `span` in one backend pass, gated as one `list` of it.
    fn list_all(&self, who: &Who, span: &Span) -> Result<Vec<String>> {
        self.gate(who, Op::List { span })?;
        Ok(self.backend.list(span, usize::MAX)?)
    }
}

/// A [`CoreStore`] bound to one caller.
#[derive(Clone)]
pub struct Session {
    store: Arc<CoreStore>,
    who: Who,
}

/// Chunked transfer (spec/wire.md). Chunks are not keys: they are pieces a
/// large blob is sent in. Uploading one is gated as a write of the blob key
/// `blake3:<id>` its bytes would have, so only callers who may write blobs
/// can add chunks.
impl Session {
    /// Whether the store keeps chunks at all.
    pub fn chunked(&self) -> bool {
        self.store.backend.chunked()
    }

    /// Which of `ids` the store lacks.
    /// Asking is gated like uploading, so it reveals nothing to a caller
    /// who could not add the chunks.
    pub fn missing_chunks(&self, ids: &[crate::chunk::Id]) -> Result<Vec<crate::chunk::Id>> {
        let mut out = Vec::new();
        for id in ids {
            let key = format!("blake3:{}", hex::encode(id));
            self.store.gate(&self.who, Op::Write { key: &key, value: &[], expect: &Expect::Absent })?;
            if self.store.backend.get_chunk(id)?.is_none() {
                out.push(*id);
            }
        }
        Ok(out)
    }

    pub fn put_chunk(&self, id: &crate::chunk::Id, bytes: &[u8]) -> Result<()> {
        let key = format!("blake3:{}", hex::encode(id));
        self.store.gate(&self.who, Op::Write { key: &key, value: bytes, expect: &Expect::Absent })?;
        if bytes.len() > crate::chunk::CHUNK_OVER {
            return Err(Error::refused("too-large", "a chunk is at most 64 KiB"));
        }
        if crate::chunk::id_of(bytes) != *id {
            return Err(Error::Invalid(Invalid::HashMismatch));
        }
        Ok(self.store.backend.put_chunk(id, bytes)?)
    }

    /// Write blob `key` from chunks the store holds, as an ordinary write
    /// of their concatenation (same gate, checks and outcome). `None` when
    /// a chunk is missing.
    pub fn write_chunks(&self, key: &str, ids: &[crate::chunk::Id], expect: &Expect) -> Result<Option<()>> {
        let mut value = Vec::new();
        for id in ids {
            match self.store.backend.get_chunk(id)? {
                Some(b) => value.extend(b),
                None => return Ok(None),
            }
        }
        self.write(key, &value, expect).map(Some)
    }
}

impl Store for Session {
    fn read(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.store.read(&self.who, key)
    }

    fn write(&self, key: &str, value: &[u8], expect: &Expect) -> Result<()> {
        self.store.write(&self.who, key, value, expect)
    }

    fn remove(&self, key: &str, expect: &Expect) -> Result<()> {
        self.store.remove(&self.who, key, expect)
    }

    fn list(&self, span: &Span) -> Result<ListPage> {
        self.store.list(&self.who, span)
    }

    fn list_all(&self, span: &Span) -> Result<Vec<String>> {
        self.store.list_all(&self.who, span)
    }

    fn write_many(&self, puts: &[Put]) -> std::result::Result<(), (usize, Error)> {
        self.store.write_many(&self.who, puts)
    }
}

/// Whether `value` is a view: `<format> <key>`, two blob keys separated by
/// one space. Only the syntax is checked; the blobs need not exist.
pub fn is_view(value: &[u8]) -> bool {
    let Ok(s) = std::str::from_utf8(value) else { return false };
    let Some((f, k)) = s.split_once(' ') else { return false };
    [f, k].iter().all(|x| matches!(crate::key::classify(x), Some(crate::key::Parsed::Blob { .. })))
}
