//! Projects (spec/forge/project.md): settings, roles, numbers.

use crate::accounts::Caller;
use crate::names::{ProjectId, Role, check_history};
use crate::record::{self, Doc, Record};
use crate::{Error, Forge, Result, now};
use std::sync::Arc;

pub const DEV: &str = "h/dev";
pub const RELEASE: &str = "h/release";

/// Settings fields an admin may set through [`Forge::edit_settings`].
pub const EDITABLE: &[&str] = &["description", "visibility", "readme"];

impl Forge {
    /// The project's settings, or `None` when there is no such project.
    pub fn settings(&self, p: &ProjectId) -> Result<Option<Arc<Doc>>> {
        if let Some(s) = self.caches.lock().unwrap().settings.get(p) {
            return Ok(Some(s.clone()));
        }
        let Some((_, r)) = record::load(&*self.store, &p.meta())? else { return Ok(None) };
        let s = Arc::new(r.doc);
        self.caches.lock().unwrap().settings.insert(p.clone(), s.clone());
        Ok(Some(s))
    }

    /// Make project `p`, owned by `p.owner`, by `user`.
    pub fn create_project(&self, user: &str, p: &ProjectId, description: &str, public: bool) -> Result<()> {
        let owner = self.account(&p.owner)?.ok_or_else(|| Error::missing(format!("no account {}", p.owner)))?;
        let may = p.owner == user
            || self.is_admin(user)
            || (owner.s("kind") == "org" && owner.doc("members").s(user) == "owner");
        if !may {
            return Err(Error::denied(format!("{user} cannot make projects for {}", p.owner)));
        }
        record::update(&*self.store, self.alg, &p.meta(), |cur| {
            if cur.is_some() {
                return Err(Error::conflict(format!("{p} exists")));
            }
            let mut d = Doc::new();
            d.set_opt("description", Some(description))
                .set("visibility", if public { "public" } else { "private" })
                .set_num("created", now())
                .set_num("next", 1)
                .set("readme", "README.md");
            d.edit("protected", |s| {
                s.set(DEV, "").set(RELEASE, "");
            });
            d.edit("sets", |s| {
                s.set("priority", "one").set("difficulty", "one");
            });
            d.edit("labels", |l| {
                for (name, color, set) in [
                    ("P1", "#d73a4a", "priority"),
                    ("P2", "#e99695", "priority"),
                    ("P3", "#f9d0c4", "priority"),
                    ("P4", "#c5def5", "priority"),
                    ("difficulty:easy", "#0e8a16", "difficulty"),
                    ("difficulty:moderate", "#fbca04", "difficulty"),
                    ("difficulty:hard", "#b60205", "difficulty"),
                ] {
                    l.edit(name, |x| {
                        x.set("color", color).set("set", set);
                    });
                }
            });
            Ok((Some(Record::new(d)), ()))
        })?;
        self.event("project", Some(user), |e| {
            e.set("project", p.to_string()).set("text", "created");
        });
        Ok(())
    }

    /// The role of `user` (`None`: not signed in) in `p`, or `None` when
    /// they have none (spec/forge/project.md, "Roles").
    pub fn role(&self, user: Option<&str>, p: &ProjectId) -> Result<Option<Role>> {
        let Some(s) = self.settings(p)? else { return Ok(None) };
        let mut best = (s.s("visibility") == "public").then_some(Role::Read);
        let mut up = |r: Role| best = Some(best.map_or(r, |b| b.max(r)));
        if let Some(u) = user {
            if p.owner == u || self.is_admin(u) {
                up(Role::Admin);
            }
            let members = s.doc("members");
            if let Some(r) = members.opt(u) {
                up(Role::parse(r)?);
            }
            for (org, org_role) in self.orgs_of(u) {
                if org == p.owner && org_role == "owner" {
                    up(Role::Admin);
                }
                if let Some(r) = members.opt(&org) {
                    up(Role::parse(r)?);
                }
            }
        }
        Ok(best)
    }

    /// The role a caller acts with: their role, capped by their token.
    pub fn caller_role(&self, c: Option<&Caller>, p: &ProjectId) -> Result<Option<Role>> {
        let Some(c) = c else { return self.role(None, p) };
        if !c.may_read(p) {
            return self.role(None, p);
        }
        let r = self.role(Some(&c.user), p)?;
        Ok(if c.may_write(p) { r } else { r.map(|r| r.min(Role::Read)) })
    }

    /// Fail unless the caller has at least `need` in `p`. A project the
    /// caller cannot read is "not found", as it is to everyone else.
    pub fn require(&self, c: Option<&Caller>, p: &ProjectId, need: Role) -> Result<Role> {
        match self.caller_role(c, p)? {
            None => Err(Error::missing(format!("no project {p}"))),
            Some(r) if r >= need => Ok(r),
            Some(_) if c.is_none() => Err(Error::unauthorized()),
            Some(_) => Err(Error::denied(format!("this needs the {} role in {p}", need.as_str()))),
        }
    }

    /// Change the settings with `f`, as `user` (admin).
    pub fn edit_settings(
        &self,
        user: &str,
        p: &ProjectId,
        what: &str,
        mut f: impl FnMut(&mut Doc) -> Result<()>,
    ) -> Result<()> {
        record::update(&*self.store, self.alg, &p.meta(), |cur| {
            let mut r = cur.ok_or_else(|| Error::missing(format!("no project {p}")))?;
            f(&mut r.doc)?;
            Ok((Some(r), ()))
        })?;
        self.event("project", Some(user), |e| {
            e.set("project", p.to_string()).set("text", what);
        });
        Ok(())
    }

    pub fn set_member(&self, user: &str, p: &ProjectId, member: &str, role: Option<Role>) -> Result<()> {
        if self.account(member)?.is_none() {
            return Err(Error::missing(format!("no account {member}")));
        }
        self.edit_settings(user, p, &format!("member {member}"), |d| {
            d.edit("members", |m| match role {
                Some(r) => {
                    m.set(member, r.as_str());
                }
                None => {
                    m.remove(member);
                }
            });
            Ok(())
        })?;
        self.event("member", Some(user), |e| {
            e.set("project", p.to_string()).set("text", format!("{member} {}", role.map_or("removed", Role::as_str)));
        });
        Ok(())
    }

    pub fn set_protected(&self, user: &str, p: &ProjectId, history: &str, on: bool) -> Result<()> {
        check_history(history)?;
        self.edit_settings(user, p, &format!("protect {history} {on}"), |d| {
            d.edit("protected", |s| {
                if on {
                    s.set(history, "");
                } else {
                    s.remove(history);
                }
            });
            Ok(())
        })
    }

    pub fn is_protected(&self, p: &ProjectId, history: &str) -> bool {
        self.settings(p).ok().flatten().is_some_and(|s| s.doc("protected").0.contains_key(history))
    }

    /// Take the next issue or proposal number.
    pub fn take_number(&self, p: &ProjectId) -> Result<u64> {
        record::update(&*self.store, self.alg, &p.meta(), |cur| {
            let mut r = cur.ok_or_else(|| Error::missing(format!("no project {p}")))?;
            let n = r.doc.num("next").max(1);
            r.doc.set_num("next", n + 1);
            Ok((Some(r), n))
        })
    }

    /// Delete project `p`: every variable under its prefix.
    pub fn delete_project(&self, user: &str, p: &ProjectId) -> Result<()> {
        for var in self.store.list_all(&cas_core::Span::prefix(p.prefix().as_bytes()))? {
            if let Some(v) = self.store.read(&var)? {
                match self.store.remove(&var, &cas_core::Expect::Value(v)) {
                    Ok(()) | Err(cas_core::Error::Conflict) => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        self.reach.forget(p);
        self.event("project", Some(user), |e| {
            e.set("project", p.to_string()).set("text", "deleted");
        });
        Ok(())
    }

    /// The projects `c` may read.
    pub fn readable_projects(&self, c: Option<&Caller>) -> Result<Vec<(ProjectId, Role)>> {
        let mut out = Vec::new();
        for p in self.projects()?.iter() {
            if let Some(r) = self.caller_role(c, p)? {
                out.push((p.clone(), r));
            }
        }
        Ok(out)
    }

    /// Fork `from` into `to`: a new project, owned by `to.owner`, whose
    /// shared histories are those of `from` (a few writes; no blob copied).
    pub fn fork(&self, user: &str, from: &ProjectId, to: &ProjectId) -> Result<()> {
        let s = self.settings(from)?.ok_or_else(|| Error::missing(format!("no project {from}")))?;
        let desc = format!("fork of {from}{}", s.opt("description").map(|d| format!(": {d}")).unwrap_or_default());
        self.create_project(user, to, &desc, s.s("visibility") == "public")?;
        let prefix = from.var("h/");
        for var in self.store.list_all(&cas_core::Span::prefix(prefix.as_bytes()))? {
            if let Some(v) = self.store.read(&var)? {
                let rest = &var[from.prefix().len()..];
                crate::gate::as_actor(Some(user), || self.store.write(&to.var(rest), &v, &cas_core::Expect::Absent))?;
            }
        }
        self.edit_settings(user, to, &format!("forked from {from}"), |d| {
            d.set("fork", from.to_string());
            Ok(())
        })
    }
}
