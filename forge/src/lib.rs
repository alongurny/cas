//! A forge for cas: projects, people, proposals, review, issues, releases,
//! events and checks over one cas store, keeping every bit of its state in
//! that store (spec/forge/store.md).

pub mod accounts;
pub mod api;
pub mod blame;
pub mod browse;
pub mod checks;
pub mod events;
pub mod forge;
pub mod gate;
pub mod issues;
pub mod mirror;
pub mod names;
pub mod project;
pub mod proposals;
pub mod reach;
pub mod record;
pub mod releases;
pub mod web;

pub use forge::Forge;
pub use names::{ProjectId, Role};

use std::fmt;

/// A failure, with the HTTP status the API answers it with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub status: u16,
    pub message: String,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(status: u16, message: impl Into<String>) -> Error {
        Error { status, message: message.into() }
    }
    pub fn bad(message: impl Into<String>) -> Error {
        Error::new(400, message)
    }
    pub fn denied(message: impl Into<String>) -> Error {
        Error::new(403, message)
    }
    pub fn unauthorized() -> Error {
        Error::new(401, "sign in first")
    }
    pub fn missing(message: impl Into<String>) -> Error {
        Error::new(404, message)
    }
    pub fn conflict(message: impl Into<String>) -> Error {
        Error::new(409, message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.status, self.message)
    }
}

impl std::error::Error for Error {}

impl From<cas_version::Error> for Error {
    fn from(e: cas_version::Error) -> Error {
        use cas_version::Error as E;
        let status = match &e {
            E::Store(cas_core::Error::Conflict) => 409,
            E::Store(cas_core::Error::Denied) => 403,
            E::Diverged(_) | E::NotRelease(_) => 409,
            E::BadPath(_) | E::BadKey(_) | E::PathConflict(_) | E::Policy(_) => 400,
            E::Missing(_) => 404,
            _ => 500,
        };
        Error::new(status, e.to_string())
    }
}

impl From<cas_core::Error> for Error {
    fn from(e: cas_core::Error) -> Error {
        Error::from(cas_version::Error::Store(e))
    }
}

/// Seconds since 1970.
pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
