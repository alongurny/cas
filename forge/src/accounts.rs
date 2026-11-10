//! Accounts (spec/forge/account.md): users and organizations, passwords and
//! tokens, as records under `forge/a/`.

use crate::names::{account_var, check_owner};
use crate::record::{self, Doc, Record};
use crate::{Error, Forge, Result, now};
use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use cas_core::Span;
use cas_core::algo;

/// How long a password sign-in lasts.
pub const SESSION: u64 = 30 * 24 * 3600;

/// The `blake3` key of a token: what an account keeps of it.
pub fn token_key(token: &str) -> String {
    algo::lookup("blake3").unwrap().key(token.as_bytes())
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    use std::io::Read;
    let mut b = [0u8; N];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .map_err(|e| Error::new(500, format!("random: {e}")))?;
    Ok(b)
}

/// A fresh token: 32 random bytes as hex.
pub fn new_token() -> Result<String> {
    Ok(hex::encode(random_bytes::<32>()?))
}

pub fn hash_password(password: &str) -> Result<String> {
    if password.len() < 8 {
        return Err(Error::bad("a password has at least 8 characters"));
    }
    let salt = SaltString::encode_b64(&random_bytes::<16>()?).map_err(|e| Error::new(500, e.to_string()))?;
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| Error::new(500, e.to_string()))?
        .to_string())
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
}

/// What a token may do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    All,
    Read,
    Project(String),
}

impl Scope {
    pub fn parse(s: &str) -> Result<Scope> {
        Ok(match s {
            "" | "all" => Scope::All,
            "read" => Scope::Read,
            _ => match s.strip_prefix("project ") {
                Some(p) => Scope::Project(crate::ProjectId::parse(p)?.to_string()),
                None => return Err(Error::bad(format!("scope {s:?}: all, read or project <owner>/<name>"))),
            },
        })
    }

    pub fn text(&self) -> String {
        match self {
            Scope::All => "all".into(),
            Scope::Read => "read".into(),
            Scope::Project(p) => format!("project {p}"),
        }
    }
}

/// A signed-in caller: the user and what their token allows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    pub user: String,
    pub scope: Scope,
}

impl Caller {
    /// As the core's caller name: `user`, or `user|<scope>` for a limited token.
    pub fn who_name(&self) -> String {
        match self.scope {
            Scope::All => self.user.clone(),
            _ => format!("{}|{}", self.user, self.scope.text()),
        }
    }

    pub fn from_who(name: &str) -> Caller {
        match name.split_once('|') {
            Some((u, s)) => Caller { user: u.into(), scope: Scope::parse(s).unwrap_or(Scope::Read) },
            None => Caller { user: name.into(), scope: Scope::All },
        }
    }

    /// Whether the token allows changes in `project`.
    pub fn may_write(&self, project: &crate::ProjectId) -> bool {
        match &self.scope {
            Scope::All => true,
            Scope::Read => false,
            Scope::Project(p) => *p == project.to_string(),
        }
    }

    pub fn may_read(&self, project: &crate::ProjectId) -> bool {
        match &self.scope {
            Scope::Project(p) => *p == project.to_string(),
            _ => true,
        }
    }
}

impl Forge {
    /// Every account, by name (cached until one changes).
    pub fn accounts(&self) -> Result<std::sync::Arc<std::collections::BTreeMap<String, Doc>>> {
        if let Some(a) = self.caches.lock().unwrap().accounts.clone() {
            return Ok(a);
        }
        let span = Span::prefix(b"forge/a/");
        let mut out = std::collections::BTreeMap::new();
        for var in self.store.list_all(&span)? {
            if let Some((_, r)) = record::load(&*self.store, &var)? {
                out.insert(var["forge/a/".len()..].to_string(), r.doc);
            }
        }
        let a = std::sync::Arc::new(out);
        self.caches.lock().unwrap().accounts = Some(a.clone());
        Ok(a)
    }

    pub fn account(&self, name: &str) -> Result<Option<Doc>> {
        Ok(self.accounts()?.get(name).cloned())
    }

    pub fn is_admin(&self, user: &str) -> bool {
        self.account(user).ok().flatten().is_some_and(|a| a.s("kind") == "user" && a.flag("admin"))
    }

    /// The caller a token stands for, if it is valid and unexpired.
    pub fn caller_for_token(&self, token: &str) -> Option<Caller> {
        let key = token_key(token);
        let accounts = self.accounts().ok()?;
        for (name, a) in accounts.iter() {
            if a.s("kind") != "user" {
                continue;
            }
            let t = a.doc("tokens").doc(&key);
            if t.0.is_empty() {
                continue;
            }
            let exp = t.num("expires");
            if exp != 0 && exp < now() {
                return None;
            }
            return Some(Caller { user: name.clone(), scope: Scope::parse(t.s("scope")).ok()? });
        }
        None
    }

    /// Make a user. The first user becomes a site administrator.
    pub fn create_user(&self, name: &str, password: Option<&str>, email: Option<&str>) -> Result<()> {
        check_owner(name)?;
        let first = self.accounts()?.values().all(|a| a.s("kind") != "user");
        let hash = password.map(hash_password).transpose()?;
        record::update(&*self.store, self.alg, &account_var(name), |cur| {
            if cur.is_some() {
                return Err(Error::conflict(format!("{name} exists")));
            }
            let mut d = Doc::new();
            d.set("kind", "user").set("name", name).set_num("created", now());
            d.set_opt("email", email).set_opt("password", hash.as_deref());
            if first {
                d.set("admin", "true");
            }
            Ok((Some(Record::new(d)), ()))
        })?;
        self.event("account", Some(name), |e| {
            e.set("text", format!("user {name}"));
        });
        Ok(())
    }

    /// Make an organization owned by `owner`.
    pub fn create_org(&self, name: &str, owner: &str) -> Result<()> {
        check_owner(name)?;
        record::update(&*self.store, self.alg, &account_var(name), |cur| {
            if cur.is_some() {
                return Err(Error::conflict(format!("{name} exists")));
            }
            let mut d = Doc::new();
            d.set("kind", "org").set("name", name).set_num("created", now());
            d.edit("members", |m| {
                m.set(owner, "owner");
            });
            Ok((Some(Record::new(d)), ()))
        })?;
        self.event("account", Some(owner), |e| {
            e.set("text", format!("organization {name}"));
        });
        Ok(())
    }

    /// Change account `name`'s document with `f`.
    pub fn edit_account(&self, name: &str, mut f: impl FnMut(&mut Doc) -> Result<()>) -> Result<()> {
        record::update(&*self.store, self.alg, &account_var(name), |cur| {
            let mut r = cur.ok_or_else(|| Error::missing(format!("no account {name}")))?;
            f(&mut r.doc)?;
            Ok((Some(r), ()))
        })
    }

    pub fn set_password(&self, user: &str, password: &str) -> Result<()> {
        let hash = hash_password(password)?;
        self.edit_account(user, |d| {
            d.set("password", hash.clone());
            Ok(())
        })
    }

    /// Make a token for `user`; returned once.
    pub fn make_token(&self, user: &str, label: &str, scope: &Scope, expires: Option<u64>) -> Result<String> {
        let token = new_token()?;
        let key = token_key(&token);
        self.edit_account(user, |d| {
            if d.s("kind") != "user" {
                return Err(Error::bad("only users have tokens"));
            }
            d.edit("tokens", |ts| {
                ts.edit(&key, |t| {
                    t.set("label", label).set_num("created", now()).set("scope", scope.text());
                    if let Some(x) = expires {
                        t.set_num("expires", x);
                    }
                });
                // Expired tokens go when a new one comes.
                let n = now();
                ts.0.retain(|_, v| {
                    let e = Doc::of(v).num("expires");
                    e == 0 || e > n
                });
            });
            Ok(())
        })?;
        Ok(token)
    }

    pub fn revoke_token(&self, user: &str, token_key: &str) -> Result<()> {
        self.edit_account(user, |d| {
            d.edit("tokens", |ts| {
                ts.remove(token_key);
            });
            Ok(())
        })
    }

    /// Sign in with a password: a session token.
    pub fn sign_in(&self, user: &str, password: &str) -> Result<String> {
        let a = self.account(user)?.filter(|a| a.s("kind") == "user");
        let ok = a.as_ref().and_then(|a| a.opt("password")).is_some_and(|h| verify_password(h, password));
        if !ok {
            return Err(Error::new(401, "wrong name or password"));
        }
        self.make_token(user, "session", &Scope::All, Some(now() + SESSION))
    }

    /// The organizations `user` belongs to, with their role there.
    pub fn orgs_of(&self, user: &str) -> Vec<(String, String)> {
        let Ok(accounts) = self.accounts() else { return Vec::new() };
        accounts
            .iter()
            .filter(|(_, a)| a.s("kind") == "org")
            .filter_map(|(n, a)| a.doc("members").opt(user).map(|r| (n.clone(), r.to_string())))
            .collect()
    }
}
