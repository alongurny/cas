//! A store whose blob reads fall through to an upstream store (spec/wire.md,
//! "Caching store"). It speaks the same [`Store`] interface, so client, cache
//! and server are interchangeable.

use crate::algo;
use crate::gate::{Gate, Op, Who};
use crate::key::{Parsed, classify};
use crate::store::{Put, Store};
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

    /// Cached blobs answer locally; everything else goes upstream as one batch.
    fn read_many(&self, keys: &[String]) -> Vec<Result<Option<Vec<u8>>>> {
        let mut out: Vec<Option<Result<Option<Vec<u8>>>>> = Vec::with_capacity(keys.len());
        let mut ask = Vec::new();
        for k in keys {
            let hit = match self.gate(Op::Read { key: k }) {
                Err(e) => Some(Err(e)),
                Ok(()) if blob_algorithm(k).is_some() => self.local.read(k).ok().flatten().map(|v| Ok(Some(v))),
                Ok(()) => None,
            };
            if hit.is_none() {
                ask.push(k.clone());
            }
            out.push(hit);
        }
        let mut answers = self.upstream.read_many(&ask).into_iter();
        keys.iter()
            .zip(out)
            .map(|(k, hit)| {
                hit.unwrap_or_else(|| {
                    let a = answers.next().unwrap_or_else(|| Err(Error::Unavailable("short batch answer".into())));
                    if let (Ok(Some(v)), Some(alg)) = (&a, blob_algorithm(k)) {
                        if alg.key(v) != *k {
                            return Err(Error::Unavailable(format!("upstream sent bytes that do not hash to {k}")));
                        }
                        let _ = self.local.write(k, v, &Expect::Absent);
                    }
                    a
                })
            })
            .collect()
    }

    fn has_many(&self, keys: &[String]) -> Result<Vec<bool>> {
        for k in keys {
            let mut end = k.as_bytes().to_vec();
            end.push(0);
            self.gate(Op::List { span: &Span { from: Some(k.as_bytes().to_vec()), to: Some(end) } })?;
        }
        self.upstream.has_many(keys)
    }

    fn write_many(&self, puts: &[Put]) -> std::result::Result<(), (usize, Error)> {
        // Up to the first write the gate denies, as `write` one by one would.
        let denied =
            puts.iter().position(|p| self.gate(Op::Write { key: &p.key, value: &p.value, expect: &p.expect }).is_err());
        let allowed = &puts[..denied.unwrap_or(puts.len())];
        self.upstream.write_many(allowed)?;
        let blobs: Vec<Put> = allowed.iter().filter(|p| blob_algorithm(&p.key).is_some()).cloned().collect();
        let _ = self.local.write_many(&blobs);
        match denied {
            Some(i) => Err((i, Error::Denied)),
            None => Ok(()),
        }
    }
}
