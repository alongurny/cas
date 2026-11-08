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
}

/// Every key in `span`, following continuations.
pub fn list_all(store: &dyn Store, span: &Span) -> Result<Vec<String>> {
    let mut keys = Vec::new();
    let mut span = span.clone();
    loop {
        let page = store.list(&span)?;
        keys.extend(page.keys);
        match page.rest {
            Some(rest) => span = rest,
            None => return Ok(keys),
        }
    }
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
}

/// A [`CoreStore`] bound to one caller.
#[derive(Clone)]
pub struct Session {
    store: Arc<CoreStore>,
    who: Who,
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
}

/// Whether `value` is a view: `<format> <key>`, two blob keys separated by
/// one space. Only the syntax is checked; the blobs need not exist.
pub fn is_view(value: &[u8]) -> bool {
    let Ok(s) = std::str::from_utf8(value) else { return false };
    let Some((f, k)) = s.split_once(' ') else { return false };
    [f, k].iter().all(|x| matches!(crate::key::classify(x), Some(crate::key::Parsed::Blob { .. })))
}
