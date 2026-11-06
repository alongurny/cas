//! A store whose blob reads fall through to an upstream store (spec/wire.md,
//! "Caching store"). It speaks the same [`Store`] interface, so client, cache
//! and server are interchangeable.

use crate::algo;
use crate::gate::{Gate, Op, Who};
use crate::key::{Parsed, classify};
use crate::store::Store;
use crate::types::{Error, Expect, ListPage, Result, Span};
use std::sync::Arc;

pub struct CachingStore {
    local: Arc<dyn Store>,
    upstream: Arc<dyn Store>,
    /// The same gate upstream runs, so a cached copy is never served to a
    /// caller upstream would deny.
    gate: Arc<dyn Gate>,
    who: Who,
}

impl CachingStore {
    pub fn new(local: Arc<dyn Store>, upstream: Arc<dyn Store>, gate: Arc<dyn Gate>, who: Who) -> CachingStore {
        CachingStore { local, upstream, gate, who }
    }

    fn gate(&self, op: Op<'_>) -> Result<()> {
        if self.gate.allow(&self.who, &op) { Ok(()) } else { Err(Error::Denied) }
    }
}

/// The registry algorithm of a well-formed blob key, if any.
fn blob_algorithm(key: &str) -> Option<&'static algo::Algorithm> {
    match classify(key)? {
        Parsed::Blob { alg, .. } => algo::lookup(alg),
        Parsed::Var => None,
    }
}

impl Store for CachingStore {
    fn read(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.gate(Op::Read { key })?;
        let Some(alg) = blob_algorithm(key) else { return self.upstream.read(key) };
        if let Ok(Some(v)) = self.local.read(key) {
            return Ok(Some(v));
        }
        let value = self.upstream.read(key)?;
        if let Some(v) = &value {
            if alg.key(v) != key {
                return Err(Error::Unavailable(format!("upstream sent bytes that do not hash to {key}")));
            }
            // Filling the cache is best effort; the read already succeeded.
            let _ = self.local.write(key, v, &Expect::Absent);
        }
        Ok(value)
    }

    fn write(&self, key: &str, value: &[u8], expect: &Expect) -> Result<()> {
        self.gate(Op::Write { key, value, expect })?;
        self.upstream.write(key, value, expect)?;
        if blob_algorithm(key).is_some() {
            let _ = self.local.write(key, value, &Expect::Absent);
        }
        Ok(())
    }

    fn remove(&self, key: &str, expect: &Expect) -> Result<()> {
        self.gate(Op::Remove { key, expect })?;
        self.upstream.remove(key, expect)?;
        if blob_algorithm(key).is_some() {
            let _ = self.local.remove(key, &Expect::Absent);
        }
        Ok(())
    }

    fn list(&self, span: &Span) -> Result<ListPage> {
        self.gate(Op::List { span })?;
        self.upstream.list(span)
    }
}
