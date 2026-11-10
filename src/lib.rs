//! Core of a content-addressed store: opaque byte values in blob and variable
//! slots, reached through four uniform operations (read, write, remove, list).
//!
//! The crate follows the Stage 1 core spec. A [`CoreStore`] validates every op
//! in spec order (gate, syntax, store policy, content, state) on top of a
//! [`Backend`] that only stores bytes.

pub mod algo;
pub mod backend;
pub mod cache;
pub mod chunk;
pub mod client;
pub mod disk;
pub mod gate;
pub mod key;
pub mod keyhash;
pub mod mem;
pub mod pack;
pub mod policy;
pub mod server;
pub mod store;
pub mod threads;
pub mod types;
pub mod wire;

#[cfg(feature = "conformance")]
pub mod conformance;

pub use backend::Backend;
pub use gate::{AllowAll, DenyAll, Gate, Op, Who};
pub use policy::Policy;
pub use store::{CoreStore, Form, Put, Session, Store, Stored, StoredPut, list_all};
pub use types::{Error, Expect, Invalid, ListPage, Result, Span};
