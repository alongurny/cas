//! The outer protocol (spec/access.md): who is calling, and what they may do.
//!
//! The core carries no credentials and its gate sees only `who`, the op and
//! its arguments. This module supplies both ends: a bearer token becomes a
//! `Who`, and an allow-list of grants decides each op. Users and grants are
//! one `uni-bin` document kept in a separate store, the meta-store, under
//! variable `policy`; only the token's hash is stored.

use crate::uni::Value;
use crate::{Error, Result, View, put, read_blob};
use cas_core::algo::{self, Algorithm};
use cas_core::key::{Parsed, classify};
use cas_core::server::{Authenticate, bearer};
use cas_core::{Expect, Gate, Op, Span, Store, Who};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

/// The meta-store variable holding the policy document.
pub const VAR: &str = "policy";
/// Grants to `ANYONE` apply to every caller, signed in or not.
pub const ANYONE: &str = "*";
/// Grants to `USERS` apply to every signed-in caller.
pub const USERS: &str = "+";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Action {
    Read,
    Write,
    Remove,
    List,
}

/// Which keys a grant covers.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// Every key.
    All,
    /// Every blob key.
    Blobs,
    /// Variables whose names start with the prefix (empty: every variable).
    Vars(String),
}

/// One permission: `<action> <scope>`, e.g. `read blobs`, `write var:main`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Grant {
    pub action: Action,
    pub scope: Scope,
}

impl Grant {
    pub fn parse(s: &str) -> Result<Grant> {
        let bad = || Error::Policy(format!("grant {s:?}: expected `read|write|remove|list *|blobs|var:PREFIX`"));
        let (a, sc) = s.split_once(' ').ok_or_else(bad)?;
        let action = match a {
            "read" => Action::Read,
            "write" => Action::Write,
            "remove" => Action::Remove,
            "list" => Action::List,
            _ => return Err(bad()),
        };
        let scope = match sc {
            "*" => Scope::All,
            "blobs" => Scope::Blobs,
            _ => Scope::Vars(sc.strip_prefix("var:").ok_or_else(bad)?.to_string()),
        };
        Ok(Grant { action, scope })
    }
}

impl std::fmt::Display for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let a = match self.action {
            Action::Read => "read",
            Action::Write => "write",
            Action::Remove => "remove",
            Action::List => "list",
        };
        match &self.scope {
            Scope::All => write!(f, "{a} *"),
            Scope::Blobs => write!(f, "{a} blobs"),
            Scope::Vars(p) => write!(f, "{a} var:{p}"),
        }
    }
}

/// Users and their grants.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rules {
    /// User name to the blake3 key of their token.
    pub users: BTreeMap<String, String>,
    /// User name, `*` or `+` to what they may do.
    pub grants: BTreeMap<String, BTreeSet<Grant>>,
}

fn blake3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

/// Whether `name` may be a user name: lowercase letters, digits, `-`, `_`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// A fresh random token: 32 bytes from the OS, as hex.
pub fn new_token() -> Result<String> {
    use std::io::Read;
    let mut b = [0u8; 32];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// The smallest byte string greater than every string starting with `p`,
/// or `None` if there is none (`p` is empty or all `ff`).
fn after_prefix(p: &[u8]) -> Option<Vec<u8>> {
    let mut v = p.to_vec();
    while let Some(last) = v.pop() {
        if last < 0xff {
            v.push(last + 1);
            return Some(v);
        }
    }
    None
}

/// Whether every key in `span` starts with `p`.
fn span_within(span: &Span, p: &[u8]) -> bool {
    let from_ok = span.from.as_deref().is_some_and(|f| f.starts_with(p)) || (p.is_empty());
    let to_ok = match (after_prefix(p), span.to.as_deref()) {
        (None, _) => true,
        (Some(end), Some(t)) => t <= &end[..],
        (Some(_), None) => false,
    };
    from_ok && to_ok
}

impl Scope {
    fn covers_key(&self, key: &str) -> bool {
        match (self, classify(key)) {
            (Scope::All, _) => true,
            (Scope::Blobs, Some(Parsed::Blob { .. })) => true,
            (Scope::Vars(p), Some(Parsed::Var)) => key.starts_with(p.as_str()),
            _ => false,
        }
    }

    /// A list grant covers a span only when no key outside the scope can
    /// fall in it.
    fn covers_span(&self, span: &Span) -> bool {
        match self {
            Scope::All => true,
            Scope::Blobs => algo::REGISTRY.iter().any(|a| span_within(span, format!("{}:", a.name).as_bytes())),
            Scope::Vars(p) => {
                // Blob keys start with `<alg>:`; a variable prefix that could share
                // a key with them does not authorize listing.
                let clash = algo::REGISTRY.iter().any(|a| {
                    let b = format!("{}:", a.name);
                    b.starts_with(p.as_str()) || p.starts_with(&b)
                });
                !clash && span_within(span, p.as_bytes())
            }
        }
    }
}

impl Rules {
    fn grants_for<'a>(&'a self, who: &'a Who) -> impl Iterator<Item = &'a Grant> + 'a {
        let mut names = vec![ANYONE];
        if let Some(n) = &who.0 {
            names.extend([USERS, n.as_str()]);
        }
        names.into_iter().filter_map(|n| self.grants.get(n)).flatten()
    }

    /// The gate's decision: some grant to the caller covers the op.
    pub fn allows(&self, who: &Who, op: &Op<'_>) -> bool {
        let (action, key, span) = match op {
            Op::Read { key } => (Action::Read, Some(*key), None),
            Op::Write { key, .. } => (Action::Write, Some(*key), None),
            Op::Remove { key, .. } => (Action::Remove, Some(*key), None),
            Op::List { span } => (Action::List, None, Some(*span)),
        };
        self.grants_for(who).any(|g| {
            g.action == action
                && match (key, span) {
                    (Some(k), _) => g.scope.covers_key(k),
                    (_, Some(s)) => g.scope.covers_span(s),
                    _ => false,
                }
        })
    }

    /// The user whose token this is.
    pub fn user_for_token(&self, token: &str) -> Option<&str> {
        let h = blake3().key(token.as_bytes());
        self.users.iter().find(|(_, t)| **t == h).map(|(n, _)| n.as_str())
    }

    /// Add or replace user `name`; returns the new token, shown once.
    pub fn add_user(&mut self, name: &str) -> Result<String> {
        if !valid_name(name) {
            return Err(Error::Policy(format!("user name {name:?}")));
        }
        let token = new_token()?;
        self.users.insert(name.to_string(), blake3().key(token.as_bytes()));
        Ok(token)
    }

    pub fn to_value(&self) -> Value {
        let users =
            self.users.iter().map(|(n, t)| (Value::str(n), Value::Map([(Value::str("token"), Value::str(t))].into())));
        let grants = self.grants.iter().map(|(w, gs)| {
            (Value::str(w), Value::Map(gs.iter().map(|g| (Value::str(g.to_string()), Value::empty())).collect()))
        });
        Value::Map(
            [(Value::str("grants"), Value::Map(grants.collect())), (Value::str("users"), Value::Map(users.collect()))]
                .into(),
        )
    }

    pub fn from_value(v: &Value) -> Result<Rules> {
        let bad = |what: &str| Error::Policy(format!("policy: {what}"));
        let m = v.as_map().ok_or_else(|| bad("not a map"))?;
        let field = |k: &str| m.get(&Value::str(k)).map_or(Ok(BTreeMap::new()), |x| x.as_map().cloned().ok_or(bad(k)));
        let mut rules = Rules::default();
        for (n, u) in field("users")? {
            let n = n.as_str().filter(|n| valid_name(n)).ok_or_else(|| bad("user name"))?;
            let t = u.as_map().and_then(|u| u.get(&Value::str("token"))).and_then(Value::as_str);
            rules.users.insert(n.to_string(), t.ok_or_else(|| bad("user token"))?.to_string());
        }
        for (w, gs) in field("grants")? {
            let w =
                w.as_str().filter(|w| *w == ANYONE || *w == USERS || valid_name(w)).ok_or_else(|| bad("grantee"))?;
            let gs = gs.as_map().ok_or_else(|| bad("grants"))?;
            let set = gs.keys().map(|g| Grant::parse(g.as_str().unwrap_or(""))).collect::<Result<_>>()?;
            rules.grants.insert(w.to_string(), set);
        }
        Ok(rules)
    }
}

/// The policy in `meta` and the variable value it was read from.
pub fn load(meta: &dyn Store) -> Result<(Option<Vec<u8>>, Rules)> {
    let Some(var) = meta.read(VAR)? else { return Ok((None, Rules::default())) };
    let r = View::parse(std::str::from_utf8(&var).map_err(|_| Error::Policy("policy variable".into()))?)?;
    let v = Value::deserialize(&read_blob(meta, r.key())?).map_err(Error::Policy)?;
    Ok((Some(var), Rules::from_value(&v)?))
}

/// Replace the policy in `meta`, only if it is still `old` (from `load`).
pub fn save(meta: &dyn Store, alg: &'static Algorithm, old: Option<Vec<u8>>, rules: &Rules) -> Result<()> {
    let r = put(meta, alg, "uni-bin", &rules.to_value().serialize())?;

    let expect = old.map_or(Expect::Absent, Expect::Value);
    meta.write(VAR, r.to_string().as_bytes(), &expect)?;
    Ok(())
}

/// The variable value last read and the rules parsed from it.
type Cached = Option<(Vec<u8>, Arc<Rules>)>;

/// The policy as a gate and an authenticator, re-read when its variable moves.
/// Any failure to read it denies everything.
/// Clones share the cache, so one can be the gate and another authenticate.
#[derive(Clone)]
pub struct Access {
    meta: Arc<dyn Store>,
    cache: Arc<Mutex<Cached>>,
}

impl Access {
    pub fn new(meta: Arc<dyn Store>) -> Access {
        Access { meta, cache: Arc::new(Mutex::new(None)) }
    }

    fn rules(&self) -> Option<Arc<Rules>> {
        let var = self.meta.read(VAR).ok()?;
        let mut cache = self.cache.lock().unwrap();
        match (&var, &*cache) {
            (None, _) => return Some(Arc::new(Rules::default())),
            (Some(t), Some((c, r))) if t == c => return Some(Arc::clone(r)),
            _ => {}
        }
        let (var, rules) = load(&*self.meta).ok()?;
        let rules = Arc::new(rules);
        *cache = var.map(|t| (t, Arc::clone(&rules)));
        Some(rules)
    }

    /// The caller a token names; no token or an unknown one is anonymous.
    pub fn who(&self, token: Option<&str>) -> Who {
        match (token, self.rules()) {
            (Some(t), Some(r)) => r.user_for_token(t).map_or(Who::anonymous(), Who::named),
            _ => Who::anonymous(),
        }
    }

    /// For [`cas_core::server::Server::start`].
    pub fn authenticate(&self) -> Arc<Authenticate> {
        let me = self.clone();
        Arc::new(move |req| me.who(bearer(req)))
    }
}

impl Gate for Access {
    fn allow(&self, who: &Who, op: &Op<'_>) -> bool {
        self.rules().is_some_and(|r| r.allows(who, op))
    }
}
