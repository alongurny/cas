//! Names: owners, projects, users, and the variables of a project
//! (spec/forge/store.md).

use crate::{Error, Result};
use std::fmt;

/// Names that cannot be owners: they are paths of the forge itself.
const RESERVED: &[&str] = &["forge", "api", "p", "static", "admin", "new", "login", "user"];

/// A lowercase name: letters, digits and `-`, starting with a letter or
/// digit, at most 64 bytes.
pub fn valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub fn check(name: &str, what: &str) -> Result<()> {
    if valid(name) { Ok(()) } else { Err(Error::bad(format!("{what} {name:?}: lowercase letters, digits and -"))) }
}

pub fn check_owner(name: &str) -> Result<()> {
    check(name, "name")?;
    if RESERVED.contains(&name) {
        return Err(Error::bad(format!("{name:?} is reserved")));
    }
    Ok(())
}

/// The variable of account `name`.
pub fn account_var(name: &str) -> String {
    format!("forge/a/{name}")
}

pub const EVENTS: &str = "forge/events";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectId {
    pub owner: String,
    pub name: String,
}

impl ProjectId {
    pub fn new(owner: &str, name: &str) -> Result<ProjectId> {
        check_owner(owner)?;
        check(name, "project")?;
        Ok(ProjectId { owner: owner.into(), name: name.into() })
    }

    /// `<owner>/<name>`.
    pub fn parse(s: &str) -> Result<ProjectId> {
        let (o, n) = s.split_once('/').ok_or_else(|| Error::bad(format!("{s:?}: expected owner/project")))?;
        ProjectId::new(o, n)
    }

    /// The prefix of the project's variables, `p/<owner>/<name>/`.
    pub fn prefix(&self) -> String {
        format!("p/{}/{}/", self.owner, self.name)
    }

    /// A variable of the project.
    pub fn var(&self, rest: &str) -> String {
        format!("{}{rest}", self.prefix())
    }

    pub fn meta(&self) -> String {
        self.var("meta")
    }

    pub fn proposal(&self, n: u64) -> String {
        self.var(&format!("r/{n}"))
    }

    pub fn issues(&self) -> String {
        self.var("i")
    }

    /// The project a variable is in, and its name in the project.
    pub fn of_var(key: &str) -> Option<(ProjectId, &str)> {
        let rest = key.strip_prefix("p/")?;
        let (owner, rest) = rest.split_once('/')?;
        let (name, var) = rest.split_once('/')?;
        (valid(owner) && valid(name)).then(|| (ProjectId { owner: owner.into(), name: name.into() }, var))
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// What a member may do (spec/forge/project.md, "Roles").
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Read,
    Write,
    Maintain,
    Admin,
}

impl Role {
    pub fn parse(s: &str) -> Result<Role> {
        Ok(match s {
            "read" => Role::Read,
            "write" => Role::Write,
            "maintain" => Role::Maintain,
            "admin" => Role::Admin,
            _ => return Err(Error::bad(format!("role {s:?}: read, write, maintain or admin"))),
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Read => "read",
            Role::Write => "write",
            Role::Maintain => "maintain",
            Role::Admin => "admin",
        }
    }
}

/// A history name in a project: `h/<name>` or `u/<user>/<name>`, each name
/// a path of [`cas_version::valid_name`]s.
pub fn check_history(var: &str) -> Result<()> {
    let bad = || Error::bad(format!("{var:?}: a history is h/<name> or u/<user>/<name>"));
    let rest = if let Some(r) = var.strip_prefix("h/") {
        r
    } else if let Some(r) = var.strip_prefix("u/") {
        let (user, r) = r.split_once('/').ok_or_else(bad)?;
        if !valid(user) {
            return Err(bad());
        }
        r
    } else {
        return Err(bad());
    };
    if rest.is_empty() || !rest.split('/').all(cas_version::valid_name) {
        return Err(bad());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_vars() {
        let p = ProjectId::new("alon", "cas").unwrap();
        assert_eq!(p.var("h/dev"), "p/alon/cas/h/dev");
        let (q, v) = ProjectId::of_var("p/alon/cas/u/alon/x").unwrap();
        assert_eq!((q, v), (p, "u/alon/x"));
        assert!(ProjectId::new("api", "x").is_err());
        assert!(check_history("h/dev").is_ok());
        assert!(check_history("u/alon/fix").is_ok());
        assert!(check_history("r/1").is_err());
    }
}
