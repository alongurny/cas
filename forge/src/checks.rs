//! Checks (spec/forge/checks.md): commands defined in the version, run by
//! listed runners, their results derived data of the version.

use crate::accounts::Caller;
use crate::names::ProjectId;
use crate::record::{self, Doc, Record};
use crate::{Error, Forge, Result, now};
use cas_core::Expect;
use cas_version::uni::Value;
use cas_version::{View, read_blob};
use serde_json::{Value as J, json};

pub const DEFINITIONS: &str = ".cas/checks.uni";

/// The checks a version defines: name to command.
pub fn definitions(f: &Forge, version: &View) -> Vec<(String, String)> {
    let Ok((_, bytes)) = f.file(version, DEFINITIONS) else { return Vec::new() };
    let Ok(text) = String::from_utf8(bytes) else { return Vec::new() };
    let Ok(v) = Value::from_text(&text) else { return Vec::new() };
    Doc::of(&v).0.iter().filter_map(|(name, d)| Doc::of(d).opt("run").map(|r| (name.clone(), r.to_string()))).collect()
}

/// The function object of check `name` running `run`, and its id.
pub fn function(f: &Forge, name: &str, run: &str) -> String {
    let obj = Value::map([("check", Value::str(name)), ("name", Value::str("check")), ("run", Value::str(run))]);
    f.alg.key(&obj.serialize())
}

fn var(f: &Forge, name: &str, run: &str, version: &View) -> String {
    format!("derived/{}/{}", function(f, name, run), version.key())
}

impl Forge {
    /// `pass`, `fail`, `pending` (no result yet) or `missing` (the version
    /// does not define the check).
    pub fn check_status(&self, version: &View, name: &str) -> Result<String> {
        let Some((_, run)) = definitions(self, version).into_iter().find(|(n, _)| n == name) else {
            return Ok("missing".into());
        };
        Ok(match record::load(&*self.store, &var(self, name, &run, version))? {
            Some((_, r)) => r.doc.s("status").to_string(),
            None => "pending".into(),
        })
    }

    /// Every check of a version, with its result.
    pub fn checks(&self, version: &View) -> Result<Vec<J>> {
        let mut out = Vec::new();
        for (name, run) in definitions(self, version) {
            let r = record::load(&*self.store, &var(self, &name, &run, version))?;
            let mut j = json!({ "name": name, "run": run, "status": "pending" });
            if let Some((_, r)) = r {
                j["status"] = json!(r.doc.s("status"));
                j["runner"] = json!(r.doc.s("runner"));
                j["time"] = json!(r.doc.s("time"));
                if let Some(log) = r.views.get("log") {
                    let b = read_blob(&*self.store, log.key())?;
                    j["log"] = json!(String::from_utf8_lossy(&b[b.len().saturating_sub(64 << 10)..]));
                }
            }
            out.push(j);
        }
        Ok(out)
    }

    /// Versions a runner should check in `p`: the tips of open proposals,
    /// of `h/dev` and of `h/release`.
    fn check_targets(&self, p: &ProjectId) -> Result<Vec<View>> {
        let mut out = Vec::new();
        for var in [crate::project::DEV, crate::project::RELEASE] {
            out.extend(self.tip_version(p, var)?);
        }
        for s in self.proposals(p, "open")? {
            out.extend(self.tip_version(p, s["source"].as_str().unwrap_or(""))?);
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// The jobs for `runner`: checks without results in projects listing it.
    pub fn jobs(&self, runner: &Caller) -> Result<Vec<J>> {
        let mut out = Vec::new();
        for p in self.projects()?.iter() {
            let Some(s) = self.settings(p)? else { continue };
            if !s.doc("runners").0.contains_key(&runner.user) || !runner.may_read(p) {
                continue;
            }
            for v in self.check_targets(p)? {
                for (name, run) in definitions(self, &v) {
                    if record::head(&*self.store, &var(self, &name, &run, &v))?.is_none() {
                        out.push(
                            json!({ "project": p.to_string(), "version": v.to_string(), "check": name, "run": run }),
                        );
                    }
                }
            }
        }
        Ok(out)
    }

    /// A runner's result. The first result for a check of a version stays.
    pub fn post_result(
        &self,
        runner: &Caller,
        p: &ProjectId,
        version: &View,
        name: &str,
        pass: bool,
        log: &[u8],
    ) -> Result<()> {
        let s = self.settings(p)?.ok_or_else(|| Error::missing(format!("no project {p}")))?;
        if !s.doc("runners").0.contains_key(&runner.user) || !runner.may_write(p) {
            return Err(Error::denied(format!("{} is not a runner of {p}", runner.user)));
        }
        if !self.may_read_blob(Some(runner), version.key()) {
            return Err(Error::missing("no such version"));
        }
        let (_, run) = definitions(self, version)
            .into_iter()
            .find(|(n, _)| n == name)
            .ok_or_else(|| Error::bad(format!("the version defines no check {name}")))?;
        let mut r = Record::default();
        r.doc.set("status", if pass { "pass" } else { "fail" }).set("runner", &runner.user).set_num("time", now());
        r.doc.set("check", name).set("version", version.to_string());
        r.views.insert("log".into(), record::raw(&*self.store, self.alg, log)?);
        let root = r.write(&*self.store, self.alg)?;
        match self.store.write(&var(self, name, &run, version), root.to_string().as_bytes(), &Expect::Absent) {
            Ok(()) | Err(cas_core::Error::Conflict) => {}
            Err(e) => return Err(e.into()),
        }
        self.event("check", Some(&runner.user), |e| {
            e.set("project", p.to_string()).set("new", version.to_string());
            e.set("text", format!("{name} {}", if pass { "pass" } else { "fail" }));
        });
        Ok(())
    }
}
