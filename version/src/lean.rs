//! The formats of `cas lean` (spec/formats/lean-decl.md, lean-env.md,
//! lean-proof.md): a Lean declaration, an environment naming its
//! declarations, and the stored proof of a unit. Each is checked by
//! parsing it and printing it back.

use crate::uni::Value;
use crate::{View, by_alias};
use cas_lean::import::Env;
use cas_lean::unit::{self, Record};
use std::collections::BTreeMap;

pub fn check_decl(b: &[u8]) -> Result<(), String> {
    let text = std::str::from_utf8(b).map_err(|_| "not UTF-8".to_string())?;
    let env = Env::parse(text)?;
    if env.decls.len() != 1 {
        return Err(format!("{} declarations, not one", env.decls.len()));
    }
    if env.decl_text(0) != text { Err("not the canonical text of its declaration".into()) } else { Ok(()) }
}

/// An environment: the Lean version and modules it was exported from
/// (either may be empty), and the key of each declaration's blob by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LeanEnv {
    pub lean: String,
    pub modules: String,
    pub decls: BTreeMap<String, String>,
}

impl LeanEnv {
    pub fn print(&self) -> Vec<u8> {
        Value::map([
            ("decls", Value::map(self.decls.iter().map(|(n, k)| (Value::str(n.clone()), Value::str(k.clone()))))),
            ("lean", Value::str(self.lean.clone())),
            ("modules", Value::str(self.modules.clone())),
        ])
        .serialize()
    }

    pub fn parse(b: &[u8]) -> Result<LeanEnv, String> {
        let v = Value::deserialize(b)?;
        let m = v.as_map().ok_or("not a map")?;
        if m.len() != 3 {
            return Err("fields other than decls, lean and modules".into());
        }
        let text = |f: &str| -> Result<String, String> {
            Ok(m.get(&Value::str(f)).and_then(Value::as_str).ok_or(format!("no {f}"))?.to_string())
        };
        let mut decls = BTreeMap::new();
        for (n, k) in m.get(&Value::str("decls")).and_then(Value::as_map).ok_or("no decls")? {
            let (Some(n), Some(k)) = (n.as_str(), k.as_str()) else {
                return Err("a declaration entry is not text".into());
            };
            if !crate::is_blob_key(k) {
                return Err(format!("{n}: not a blob key"));
            }
            decls.insert(n.to_string(), k.to_string());
        }
        Ok(LeanEnv { lean: text("lean")?, modules: text("modules")?, decls })
    }
}

pub fn check_env(b: &[u8]) -> Result<(), String> {
    LeanEnv::parse(b).map(|_| ())
}

pub fn check_proof(b: &[u8]) -> Result<(), String> {
    let r: Record = unit::decode(b)?;
    if unit::encode(&r) != b {
        return Err("not the canonical encoding".into());
    }
    if r.against.iter().any(|(_, k)| !crate::is_blob_key(k)) {
        return Err("a declaration key is not a blob key".into());
    }
    if r.against.windows(2).any(|w| w[0].0 >= w[1].0) {
        return Err("the declarations are not in name order".into());
    }
    if r.outcome.is_ok() != (r.summary.status == "ok") {
        return Err("the status and the outcome disagree".into());
    }
    Ok(())
}

fn decl_view(key: &str) -> View {
    View { format: by_alias("lean-decl").unwrap().id().to_string(), key: key.to_string() }
}

/// The declarations an environment names.
pub fn env_children(b: &[u8]) -> Vec<View> {
    LeanEnv::parse(b).map(|e| e.decls.values().map(|k| decl_view(k)).collect()).unwrap_or_default()
}

/// The declarations a proof was made against.
pub fn proof_children(b: &[u8]) -> Vec<View> {
    let Ok(s) = unit::decode_prefix::<(unit::Summary, Vec<(String, String)>)>(b) else { return vec![] };
    s.1.iter().map(|(_, k)| decl_view(k)).collect()
}
