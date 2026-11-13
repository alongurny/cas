//! Lean declarations checked through derivations of local rules (spec/lean/derivation.md).
//!
//! A prover runs a kernel and writes down the steps that held as rows
//! ([`derive`]); a checker looks at each row, its own fields and the rows it
//! names, and nothing else ([`check`]). Terms are compared by random
//! fingerprints ([`fp`]), so the derivation needs no hashing and no check that
//! rows are distinct. The same checks are meant to become a circuit.

pub mod block;
pub mod check;
pub mod derivation;
pub mod derive;
pub mod expr;
pub mod fp;
pub mod import;
pub mod level;
pub mod nat;
pub mod nested;

pub use check::Checker;
pub use derivation::{DeclKind, DeclRow, Derivation, Goal};
pub use derive::Builder;
