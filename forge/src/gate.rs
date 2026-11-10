//! The wire's rules (spec/forge/store.md, "Who may touch what on the
//! wire"), and the backend that tells the forge what moved.

use crate::Forge;
use crate::accounts::Caller;
use crate::forge::SELF;
use crate::names::{ProjectId, Role};
use crate::reach::{Index, readable_var};
use cas_core::backend::Backend;
use cas_core::key::{Parsed, classify};
use cas_core::store::{Stored, StoredPut};
use cas_core::{Expect, Gate, Op, Span, Who};
use std::cell::RefCell;
use std::io;
use std::sync::{Arc, OnceLock, Weak};

thread_local! {
    /// Who the current store operation is for: set by the gate for wire
    /// callers, and by the forge's API around its own writes.
    static ACTOR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Run `f` with `user` as the actor of the moves it makes.
pub fn as_actor<T>(user: Option<&str>, f: impl FnOnce() -> T) -> T {
    let prev = ACTOR.with(|a| a.replace(user.map(String::from)));
    let out = f();
    ACTOR.with(|a| *a.borrow_mut() = prev);
    out
}

fn actor() -> Option<String> {
    ACTOR.with(|a| a.borrow().clone())
}

/// Filled in once the forge exists (the store it wraps comes first).
pub type Slot = Arc<OnceLock<Weak<Forge>>>;

pub struct ForgeGate {
    pub forge: Slot,
}

impl ForgeGate {
    fn forge(&self) -> Option<Arc<Forge>> {
        self.forge.get().and_then(Weak::upgrade)
    }
}

fn caller(who: &Who) -> Option<Caller> {
    who.0.as_deref().map(Caller::from_who)
}

/// Whether `span` holds exactly one key, and which: a `has`.
fn one_key(span: &Span) -> Option<&str> {
    let (f, t) = (span.from.as_deref()?, span.to.as_deref()?);
    (t.len() == f.len() + 1 && t.starts_with(f) && t.last() == Some(&0)).then(|| std::str::from_utf8(f).ok()).flatten()
}

impl Forge {
    /// Whether caller `c` may read blob `key`: some project they can read
    /// reaches it.
    pub fn may_read_blob(&self, c: Option<&Caller>, key: &str) -> bool {
        if Index::always(key) {
            return true;
        }
        let Ok(projects) = self.readable_projects(c) else { return false };
        projects.iter().any(|(p, _)| self.reach.reaches(&*self.store, p, key))
    }

    fn may_read_var(&self, c: Option<&Caller>, key: &str) -> bool {
        let Some((p, rest)) = ProjectId::of_var(key) else { return false };
        readable_var(rest) && self.caller_role(c, &p).ok().flatten().is_some()
    }

    /// Whether `c` may move variable `key` on the wire.
    pub fn may_move(&self, c: Option<&Caller>, key: &str) -> bool {
        let Some((p, rest)) = ProjectId::of_var(key) else { return false };
        let Some(c) = c else { return false };
        let role = self.caller_role(Some(c), &p).ok().flatten();
        if let Some(h) = rest.strip_prefix("h/") {
            return !h.is_empty() && !self.is_protected(&p, rest) && role >= Some(Role::Maintain);
        }
        if let Some(u) = rest.strip_prefix("u/") {
            return u.split_once('/').is_some_and(|(user, name)| user == c.user && !name.is_empty())
                && role >= Some(Role::Write);
        }
        false
    }

    fn allows(&self, c: Option<&Caller>, op: &Op<'_>) -> bool {
        match op {
            Op::Read { key } => match classify(key) {
                Some(Parsed::Blob { .. }) => self.may_read_blob(c, key),
                _ => self.may_read_var(c, key),
            },
            Op::Write { key, .. } => match classify(key) {
                Some(Parsed::Blob { .. }) => c.is_some_and(|c| c.scope != crate::accounts::Scope::Read),
                _ => self.may_move(c, key),
            },
            Op::Remove { key, .. } => matches!(classify(key), Some(Parsed::Var)) && self.may_move(c, key),
            Op::List { span } => {
                if let Some(k) = one_key(span)
                    && matches!(classify(k), Some(Parsed::Blob { .. }))
                {
                    // A `has`: answered "absent" where hidden.
                    return true;
                }
                let from = span.from.as_deref().and_then(|f| std::str::from_utf8(f).ok()).unwrap_or("");
                let Some((p, _)) = ProjectId::of_var(from) else { return false };
                let prefix = p.prefix();
                let inside = span.to.as_deref().is_some_and(|t| {
                    let end = after(prefix.as_bytes());
                    end.is_some_and(|e| t <= &e[..])
                });
                from.starts_with(&prefix) && inside && self.caller_role(c, &p).ok().flatten().is_some()
            }
        }
    }
}

fn after(p: &[u8]) -> Option<Vec<u8>> {
    let mut v = p.to_vec();
    while let Some(last) = v.pop() {
        if last < 0xff {
            v.push(last + 1);
            return Some(v);
        }
    }
    None
}

impl Gate for ForgeGate {
    fn allow(&self, who: &Who, op: &Op<'_>) -> bool {
        if who.0.as_deref() == Some(SELF) {
            return true;
        }
        let c = caller(who);
        ACTOR.with(|a| *a.borrow_mut() = c.as_ref().map(|c| c.user.clone()));
        self.forge().is_some_and(|f| f.allows(c.as_ref(), op))
    }

    fn hides(&self, who: &Who, key: &str) -> bool {
        if who.0.as_deref() == Some(SELF) || !matches!(classify(key), Some(Parsed::Blob { .. })) {
            return false;
        }
        !self.forge().is_some_and(|f| f.may_read_blob(caller(who).as_ref(), key))
    }
}

/// A backend that reports each variable that moves to the forge.
pub struct Observed<B> {
    pub inner: B,
    pub forge: Slot,
}

impl<B: Backend> Backend for Observed<B> {
    fn get(&self, key: &str) -> io::Result<Option<Vec<u8>>> {
        self.inner.get(key)
    }
    fn put_blob(&self, key: &str, value: &[u8]) -> io::Result<()> {
        self.inner.put_blob(key, value)
    }
    fn put_blobs(&self, blobs: &[(&str, &[u8])]) -> io::Result<()> {
        self.inner.put_blobs(blobs)
    }
    fn get_stored(&self, key: &str) -> io::Result<Option<Stored>> {
        self.inner.get_stored(key)
    }
    fn put_stored(&self, blobs: &[StoredPut]) -> io::Result<()> {
        self.inner.put_stored(blobs)
    }
    fn remove_blob(&self, key: &str) -> io::Result<()> {
        self.inner.remove_blob(key)
    }
    fn cas_var(&self, name: &str, expect: &Expect, new: Option<&[u8]>) -> io::Result<bool> {
        let done = self.inner.cas_var(name, expect, new)?;
        if done && let Some(f) = self.forge.get().and_then(Weak::upgrade) {
            let old = match expect {
                Expect::Absent => None,
                Expect::Value(v) => Some(&v[..]),
            };
            f.moved(actor().as_deref(), name, old, new);
        }
        Ok(done)
    }
    fn list(&self, span: &Span, limit: usize) -> io::Result<Vec<String>> {
        self.inner.list(span, limit)
    }
    fn has(&self, key: &str) -> io::Result<bool> {
        self.inner.has(key)
    }
    fn has_many(&self, keys: &[&str]) -> io::Result<Vec<bool>> {
        self.inner.has_many(keys)
    }
    fn chunked(&self) -> bool {
        self.inner.chunked()
    }
    fn get_chunk(&self, id: &cas_core::chunk::Id) -> io::Result<Option<Vec<u8>>> {
        self.inner.get_chunk(id)
    }
    fn put_chunk(&self, id: &cas_core::chunk::Id, bytes: &[u8]) -> io::Result<()> {
        self.inner.put_chunk(id, bytes)
    }
}

/// A store whose gate and backend report to a forge made right after.
pub fn open(backend: impl Backend + 'static) -> Arc<Forge> {
    let slot: Slot = Arc::new(OnceLock::new());
    let core = cas_core::CoreStore::new(
        Observed { inner: backend, forge: slot.clone() },
        ForgeGate { forge: slot.clone() },
        cas_core::policy::Policy::permissive(),
    );
    let forge = Forge::new(core);
    let _ = slot.set(Arc::downgrade(&forge));
    forge
}
