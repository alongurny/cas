//! Proposals and review (spec/forge/proposal.md).

use crate::accounts::Caller;
use crate::gate::as_actor;
use crate::names::{ProjectId, Role, check_history};
use crate::record::{self, Doc, Record, list_key};
use crate::{Error, Forge, Result, now};
use cas_version::commits::{self, Commit, Notes, Outcome};
use cas_version::stepnotes::{self, StepNotes};
use cas_version::{View, annotated, object_at};
use serde_json::{Value as J, json};

/// A line of a file: its text, by side and revision, for anchoring.
fn file_lines(f: &Forge, version: &View, path: &str) -> Vec<String> {
    match f.file(version, path) {
        Ok((_, b)) => {
            annotated::lines(&b).iter().map(|l| String::from_utf8_lossy(l).trim_end_matches('\n').to_string()).collect()
        }
        Err(_) => Vec::new(),
    }
}

/// The index of the line in `lines` with `text` nearest to `near`.
fn find_line(lines: &[String], text: &str, near: usize) -> Option<usize> {
    lines.iter().enumerate().filter(|(_, l)| *l == text).map(|(i, _)| i).min_by_key(|i| i.abs_diff(near))
}

struct Gates {
    approvals: (usize, u64),
    checks: Vec<(String, String)>,
    unresolved: usize,
    need_resolved: bool,
}

impl Gates {
    fn ok(&self) -> bool {
        self.approvals.0 as u64 >= self.approvals.1
            && self.checks.iter().all(|(_, s)| s == "pass")
            && (!self.need_resolved || self.unresolved == 0)
    }

    fn json(&self) -> J {
        json!({
            "approvals": self.approvals.0, "approvals_needed": self.approvals.1,
            "checks": self.checks.iter().map(|(n, s)| json!({"name": n, "status": s})).collect::<Vec<_>>(),
            "unresolved": self.unresolved, "resolved_needed": self.need_resolved,
            "ok": self.ok(),
        })
    }
}

impl Forge {
    fn proposal(&self, p: &ProjectId, n: u64) -> Result<Record> {
        Ok(record::load(&*self.store, &p.proposal(n))?.ok_or_else(|| Error::missing(format!("no proposal {n}")))?.1)
    }

    fn edit_proposal<T>(&self, p: &ProjectId, n: u64, mut f: impl FnMut(&mut Record) -> Result<T>) -> Result<T> {
        record::update(&*self.store, self.alg, &p.proposal(n), |cur| {
            let mut r = cur.ok_or_else(|| Error::missing(format!("no proposal {n}")))?;
            let out = f(&mut r)?;
            r.doc.set_num("updated", now());
            Ok((Some(r), out))
        })
    }

    /// The version of the commit `view` names.
    fn version_at(&self, view: &View) -> Result<View> {
        Ok(commits::read(&*self.store, view.key())?.version())
    }

    /// The version of the commit with key `key`, if it is one here.
    fn version_of(&self, key: &str) -> Option<View> {
        commits::read(&*self.store, key).ok().map(|c| c.version())
    }

    /// Add a revision to `r` from the source's and target's current views,
    /// unless the latest has the same source.
    pub(crate) fn revise(&self, p: &ProjectId, r: &mut Record) -> Result<bool> {
        let src = record::head(&*self.store, &p.var(r.doc.s("source")))?
            .ok_or_else(|| Error::missing("the proposal's source is gone"))?;
        let k = r.doc.doc("revisions").0.len() as u64;
        if k > 0 && r.views.get(&format!("rev/{k}/source")) == Some(&src) {
            return Ok(false);
        }
        let tgt = record::head(&*self.store, &p.var(r.doc.s("target")))?;
        let mut rev = Doc::new();
        rev.set_num("time", now()).set("tip", src.key());
        rev.set("base", tgt.as_ref().map(|t| t.key().to_string()).unwrap_or_default());
        let i = r.doc.push("revisions", rev);
        r.views.insert(format!("rev/{i}/source"), src);
        if let Some(t) = tgt {
            r.views.insert(format!("rev/{i}/target"), t);
        }
        Ok(true)
    }

    pub fn open_proposal(&self, c: &Caller, p: &ProjectId, title: &str, source: &str, target: &str) -> Result<u64> {
        self.require(Some(c), p, Role::Write)?;
        check_history(source)?;
        check_history(target)?;
        if !source.starts_with(&format!("u/{}/", c.user)) {
            return Err(Error::bad(format!("a proposal comes from one of your histories, u/{}/<name>", c.user)));
        }
        if !target.starts_with("h/") {
            return Err(Error::bad("a proposal goes to a shared history, h/<name>"));
        }
        let title = title.trim();
        if title.is_empty() || title.contains('\n') {
            return Err(Error::bad("a title is one non-empty line"));
        }
        let n = self.take_number(p)?;
        let mut r = Record::default();
        r.doc.set_num("number", n).set("title", title).set("author", &c.user);
        r.doc.set("source", source).set("target", target).set("state", "open");
        r.doc.set_num("created", now()).set_num("updated", now());
        self.revise(p, &mut r)?;
        as_actor(Some(&c.user), || record::save(&*self.store, self.alg, &p.proposal(n), None, &r))?;
        self.event("proposal", Some(&c.user), |e| {
            e.set("project", p.to_string()).set_num("number", n).set("text", format!("opened: {title}"));
        });
        Ok(n)
    }

    /// The proposals of `p`, newest first, in `state` (empty: all).
    pub fn proposals(&self, p: &ProjectId, state: &str) -> Result<Vec<J>> {
        let prefix = p.var("r/");
        let mut out = Vec::new();
        for var in self.store.list_all(&cas_core::Span::prefix(prefix.as_bytes()))? {
            let Some((_, r)) = record::load(&*self.store, &var)? else { continue };
            if state.is_empty() || r.doc.s("state") == state {
                out.push(summary(&r.doc));
            }
        }
        out.sort_by_key(|j| std::cmp::Reverse(j["number"].as_str().and_then(|n| n.parse::<u64>().ok())));
        Ok(out)
    }

    /// Record a new revision of every open proposal whose source is `var`
    /// (a `u/` history of `p` that moved).
    pub fn source_moved(&self, p: &ProjectId, var: &str) {
        let Ok(list) = self.proposals(p, "open") else { return };
        for s in list {
            if s["source"] != var {
                continue;
            }
            let Some(n) = s["number"].as_str().and_then(|n| n.parse().ok()) else { continue };
            let revised = self.edit_proposal(p, n, |r| self.revise(p, r));
            if let Ok(true) = revised {
                let author = s["author"].as_str().unwrap_or("").to_string();
                self.event("proposal", Some(&author), |e| {
                    e.set("project", p.to_string()).set_num("number", n).set("text", "new revision");
                });
            }
        }
    }

    fn gates(&self, p: &ProjectId, r: &Record) -> Result<Gates> {
        let s = self.settings(p)?.ok_or_else(|| Error::missing("no project"))?;
        let g = s.doc("gates").doc(r.doc.s("target"));
        let latest = r.doc.doc("revisions").0.len() as u64;
        let approvals = r.doc.doc("approvals").0.values().filter(|v| v.as_str() == Some(&latest.to_string())).count();
        let tip = r.views.get(&format!("rev/{latest}/source")).map(|v| self.version_at(v)).transpose()?;
        let mut checks = Vec::new();
        for name in g.keys("checks") {
            let status = match &tip {
                Some(t) => self.check_status(t, &name)?,
                None => "pending".into(),
            };
            checks.push((name, status));
        }
        let unresolved = r.doc.entries("threads").iter().filter(|(_, t)| !t.flag("resolved")).count();
        Ok(Gates { approvals: (approvals, g.num("approvals")), checks, unresolved, need_resolved: g.flag("resolved") })
    }

    /// A proposal with its revisions, threads (anchored to the latest
    /// revision), gates and diff.
    pub fn proposal_info(&self, p: &ProjectId, n: u64) -> Result<J> {
        let r = self.proposal(p, n)?;
        let latest = r.doc.doc("revisions").0.len() as u64;
        let src = r.views.get(&format!("rev/{latest}/source")).cloned();
        let tip = src.as_ref().map(|v| self.version_at(v)).transpose()?;
        let base = r.doc.list("revisions").last().map(|rv| rv.s("base").to_string()).unwrap_or_default();
        let base_version = self.version_of(&base);
        let mut threads = Vec::new();
        for (id, t) in r.doc.entries("threads") {
            let mut j = t.json();
            j["id"] = json!(id.trim_start_matches('0'));
            j["comments"] = json!(t.list("comments").iter().map(Doc::json).collect::<Vec<_>>());
            // Where the thread's line is now.
            let (now_line, outdated) = self.anchor_now(&r, &t, latest, tip.as_ref(), base_version.as_ref());
            j["line_now"] = json!(now_line);
            j["outdated"] = json!(outdated);
            threads.push(j);
        }
        let diff = match &tip {
            Some(t) => self.compare(base_version.as_ref(), t)?,
            None => json!(null),
        };
        let mut j = summary(&r.doc);
        j["revisions"] = json!(r.doc.list("revisions").iter().map(Doc::json).collect::<Vec<_>>());
        j["approvals"] = r.doc.doc("approvals").json();
        j["threads"] = json!(threads);
        j["gates"] = self.gates(p, &r)?.json();
        j["diff"] = diff;
        j["merged"] = json!(r.doc.opt("merged"));
        // What a migration kept from elsewhere (spec/forge/migrate.md).
        let im = r.doc.doc("imported");
        if !im.0.is_empty() {
            let mut x = im.json();
            x["comments"] = json!(im.list("comments").iter().map(Doc::json).collect::<Vec<_>>());
            x["reviews"] = json!(
                im.list("reviews")
                    .iter()
                    .map(|rv| {
                        let mut y = rv.json();
                        y["comments"] = json!(rv.list("comments").iter().map(Doc::json).collect::<Vec<_>>());
                        y
                    })
                    .collect::<Vec<_>>()
            );
            j["imported"] = x;
        }
        Ok(j)
    }

    /// The line (1-based) of thread `t` in the latest revision, and whether
    /// it is outdated (its line is gone).
    fn anchor_now(&self, r: &Record, t: &Doc, latest: u64, tip: Option<&View>, base: Option<&View>) -> (u64, bool) {
        let rev = t.num("revision");
        let line = t.num("line");
        if rev == latest {
            return (line, false);
        }
        let path = t.s("path");
        let side_new = t.s("side") != "old";
        let then = if side_new {
            r.views.get(&format!("rev/{rev}/source")).and_then(|v| self.version_at(v).ok())
        } else {
            r.doc.list("revisions").get(rev as usize - 1).and_then(|rv| self.version_of(rv.s("base")))
        };
        let now = if side_new { tip.cloned() } else { base.cloned() };
        let (Some(then), Some(now)) = (then, now) else { return (line, true) };
        let _ = then;
        let text = t.s("text");
        match find_line(&file_lines(self, &now, path), text, line.saturating_sub(1) as usize) {
            Some(i) => (i as u64 + 1, false),
            None => (line, true),
        }
    }

    /// Start a review thread on a line of the latest revision.
    #[allow(clippy::too_many_arguments)]
    pub fn add_thread(
        &self,
        c: &Caller,
        p: &ProjectId,
        n: u64,
        path: &str,
        side: &str,
        line: u64,
        text: &str,
        suggestion: Option<&str>,
    ) -> Result<u64> {
        self.require(Some(c), p, Role::Write)?;
        if !matches!(side, "old" | "new") || line == 0 || text.trim().is_empty() {
            return Err(Error::bad("a thread needs side old or new, a line from 1, and text"));
        }
        cas_version::Path::new(path)?;
        let id = self.edit_proposal(p, n, |r| {
            if r.doc.s("state") != "open" {
                return Err(Error::conflict("the proposal is not open"));
            }
            let latest = r.doc.doc("revisions").0.len() as u64;
            let version = if side == "new" {
                r.views.get(&format!("rev/{latest}/source")).map(|v| self.version_at(v)).transpose()?
            } else {
                r.doc.list("revisions").last().and_then(|rv| self.version_of(rv.s("base")))
            };
            let lines = version.map(|v| file_lines(self, &v, path)).unwrap_or_default();
            let Some(line_text) = lines.get(line as usize - 1) else {
                return Err(Error::bad(format!("{path} has no {side} line {line}")));
            };
            let id = r.doc.doc("threads").0.len() as u64 + 1;
            let mut t = Doc::new();
            t.set_num("revision", latest).set("path", path).set("side", side).set_num("line", line);
            t.set("text", line_text.replace(char::is_control, " "));
            t.set("author", &c.user).set_num("time", now());
            let mut cm = Doc::new();
            cm.set("author", &c.user).set_num("time", now()).set("text", text).set_opt("suggestion", suggestion);
            t.push("comments", cm);
            r.doc.edit("threads", |ts| {
                ts.0.insert(list_key(id), t.value());
            });
            Ok(id)
        })?;
        self.event("review", Some(&c.user), |e| {
            e.set("project", p.to_string()).set_num("number", n).set("text", format!("{path}:{line}: {text}"));
        });
        Ok(id)
    }

    fn edit_thread(
        &self,
        p: &ProjectId,
        n: u64,
        id: u64,
        mut f: impl FnMut(&mut Doc, &Doc) -> Result<()>,
    ) -> Result<()> {
        self.edit_proposal(p, n, |r| {
            let doc = r.doc.clone();
            let mut ts = r.doc.doc("threads");
            let key = list_key(id);
            let mut t = Doc::of(ts.0.get(&key).ok_or_else(|| Error::missing(format!("no thread {id}")))?);
            f(&mut t, &doc)?;
            ts.0.insert(key, t.value());
            r.doc.set_doc("threads", ts);
            Ok(())
        })
    }

    pub fn reply(
        &self,
        c: &Caller,
        p: &ProjectId,
        n: u64,
        id: u64,
        text: &str,
        suggestion: Option<&str>,
    ) -> Result<()> {
        self.require(Some(c), p, Role::Write)?;
        self.edit_thread(p, n, id, |t, _| {
            let mut cm = Doc::new();
            cm.set("author", &c.user).set_num("time", now()).set("text", text).set_opt("suggestion", suggestion);
            t.push("comments", cm);
            Ok(())
        })?;
        self.event("review", Some(&c.user), |e| {
            e.set("project", p.to_string()).set_num("number", n).set("text", text);
        });
        Ok(())
    }

    pub fn resolve_thread(&self, c: &Caller, p: &ProjectId, n: u64, id: u64, resolved: bool) -> Result<()> {
        self.require(Some(c), p, Role::Write)?;
        self.edit_thread(p, n, id, |t, _| {
            t.set_opt("resolved", resolved.then_some("true"));
            Ok(())
        })
    }

    /// The proposal's author accepts (or no longer accepts) a thread's first
    /// note as a step note of its line.
    pub fn promote_thread(&self, c: &Caller, p: &ProjectId, n: u64, id: u64, promote: bool) -> Result<()> {
        self.edit_thread(p, n, id, |t, doc| {
            if doc.s("author") != c.user {
                return Err(Error::denied("only the proposal's author decides what its steps say"));
            }
            if promote && t.s("side") != "new" {
                return Err(Error::bad("only a note on a new line can become a step note"));
            }
            t.set_opt("promote", promote.then_some("true"));
            Ok(())
        })
    }

    pub fn approve(&self, c: &Caller, p: &ProjectId, n: u64) -> Result<()> {
        self.require(Some(c), p, Role::Maintain)?;
        self.edit_proposal(p, n, |r| {
            if r.doc.s("author") == c.user {
                return Err(Error::bad("authors do not approve their own proposals"));
            }
            let latest = r.doc.doc("revisions").0.len() as u64;
            r.doc.edit("approvals", |a| {
                a.set_num(&c.user, latest);
            });
            Ok(())
        })?;
        self.event("review", Some(&c.user), |e| {
            e.set("project", p.to_string()).set_num("number", n).set("text", "approved");
        });
        Ok(())
    }

    pub fn set_proposal_state(&self, c: &Caller, p: &ProjectId, n: u64, open: bool) -> Result<()> {
        let role = self.require(Some(c), p, Role::Write)?;
        self.edit_proposal(p, n, |r| {
            if r.doc.s("author") != c.user && role < Role::Maintain {
                return Err(Error::denied("only the author or a maintainer closes a proposal"));
            }
            match (r.doc.s("state"), open) {
                ("merged", _) => return Err(Error::conflict("the proposal is merged")),
                ("closed", true) => {
                    r.doc.set("state", "open");
                    self.revise(p, r)?;
                }
                ("open", false) => {
                    r.doc.set("state", "closed");
                }
                _ => {}
            }
            Ok(())
        })?;
        self.event("proposal", Some(&c.user), |e| {
            e.set("project", p.to_string()).set_num("number", n).set("text", if open { "reopened" } else { "closed" });
        });
        Ok(())
    }

    pub fn retitle(&self, c: &Caller, p: &ProjectId, n: u64, title: &str) -> Result<()> {
        let role = self.require(Some(c), p, Role::Write)?;
        let title = title.trim();
        if title.is_empty() || title.contains('\n') {
            return Err(Error::bad("a title is one non-empty line"));
        }
        self.edit_proposal(p, n, |r| {
            if r.doc.s("author") != c.user && role < Role::Maintain {
                return Err(Error::denied("only the author or a maintainer retitles a proposal"));
            }
            r.doc.set("title", title);
            Ok(())
        })
    }

    /// Merge: rebase the source onto the target and move the target, once
    /// the gates hold. The new tip of the target.
    pub fn merge(&self, c: &Caller, p: &ProjectId, n: u64) -> Result<String> {
        self.require(Some(c), p, Role::Maintain)?;
        let r = self.proposal(p, n)?;
        if r.doc.s("state") != "open" {
            return Err(Error::conflict("the proposal is not open"));
        }
        let mut r = r;
        self.revise(p, &mut r)?;
        let g = self.gates(p, &r)?;
        if !g.ok() {
            return Err(Error::conflict(format!("the gates do not hold: {}", g.json())));
        }
        let (source, target) = (r.doc.s("source").to_string(), r.doc.s("target").to_string());
        let src = commits::tip(&*self.store, &p.var(&source))?
            .ok_or_else(|| Error::missing("the proposal's source is gone"))?;
        let promoted: Vec<Doc> =
            r.doc.entries("threads").into_iter().map(|(_, t)| t).filter(|t| t.flag("promote")).collect();
        let tip = loop {
            let old = commits::tip(&*self.store, &p.var(&target))?;
            let mut tip = match &old {
                // Into an empty history: the source's commits as they are.
                None => src.clone(),
                Some(onto) => match commits::rebase(&*self.store, self.alg, &src, onto)? {
                    // The source is behind the target or at it.
                    Outcome::FastForward(_) => {
                        return Err(Error::conflict("nothing to merge: the target has every step"));
                    }
                    // The source is on top of the target already.
                    Outcome::UpToDate => src.clone(),
                    Outcome::Done(t) => t,
                    Outcome::Conflicts(m) => {
                        let paths: Vec<String> = m.conflicts.iter().map(|c| c.path.to_string()).collect();
                        return Err(Error::conflict(format!(
                            "rebase onto {target} conflicts at {}; rebase and push again",
                            paths.join(", ")
                        )));
                    }
                },
            };
            for t in &promoted {
                tip = self.promote_into(&tip, old.as_deref(), t)?;
            }
            match as_actor(Some(&c.user), || commits::advance(&*self.store, &p.var(&target), old.as_deref(), &tip)) {
                Ok(()) => break tip,
                Err(cas_version::Error::Store(cas_core::Error::Conflict)) => continue,
                Err(e) => return Err(e.into()),
            }
        };
        self.edit_proposal(p, n, |r| {
            r.doc.set("state", "merged").set("merged", tip.clone()).set("merged_by", &c.user);
            Ok(())
        })?;
        self.event("merge", Some(&c.user), |e| {
            e.set("project", p.to_string()).set_num("number", n).set("var", target.clone()).set("new", tip.clone());
        });
        Ok(tip)
    }

    /// Put thread `t`'s first note on its line, in the newest commit from
    /// `tip` back to `onto` (not included) that added a line with its
    /// text. That commit and those after it are written again; the new tip.
    fn promote_into(&self, tip: &str, onto: Option<&str>, t: &Doc) -> Result<String> {
        let Some(note) = t.list("comments").first().map(|c| c.s("text").replace(char::is_control, " ")) else {
            return Ok(tip.to_string());
        };
        let (store, path) = (&*self.store, t.s("path").to_string());
        // The replayed commits, newest first, each with its one parent.
        let mut chain: Vec<(Commit, String)> = Vec::new();
        let mut at = tip.to_string();
        while Some(at.as_str()) != onto {
            let c = commits::read(store, &at)?;
            let mut parents = c.parents.keys();
            let (Some(pred), None) = (parents.next().cloned(), parents.next()) else { break };
            chain.push((c, pred.clone()));
            at = pred;
        }
        for (i, (c, pred)) in chain.iter().enumerate() {
            let (before, after) = (commits::read(store, pred)?.version(), c.version());
            let (d, lens) = annotated::diff_with_lens(store, Some(&before), &after)?;
            let Some(line) = added_line(&d, &path, &self.thread_text(t)) else { continue };
            let said = match &c.parents[pred] {
                Some(k) => commits::read_notes(store, k)?,
                None => Notes::default(),
            };
            let sn =
                StepNotes { before: Some(before.clone()), after: after.clone(), note: said.note, paths: said.paths };
            let mut notes: Vec<String> = stepnotes::line_notes(&sn, &d).into_iter().map(String::from).collect();
            if line >= notes.len() {
                return Ok(tip.to_string());
            }
            notes[line] = note;
            let Some(sn) = stepnotes::from_lines(Some(&before), &after, &d, &lens, &notes).map_err(Error::bad)? else {
                return Ok(tip.to_string());
            };
            let is_dir = |p: &str| -> cas_version::Result<bool> {
                Ok(object_at(store, &after, &cas_version::Path::new(p)?)?.is_some_and(|v| v.is_dir_view()))
            };
            let step = commits::write_notes(store, self.alg, &Notes { note: sn.note, paths: sn.paths }, &is_dir)?;
            let mut noted = c.clone();
            noted.parents.insert(pred.clone(), step);
            let mut key = commits::write(store, self.alg, &noted)?;
            // The commits after it follow the new one.
            for (child, _) in chain[..i].iter().rev() {
                let mut child = child.clone();
                let (_, step) = child.parents.pop_first().expect("one parent");
                child.parents.insert(key, step);
                key = commits::write(store, self.alg, &child)?;
            }
            return Ok(key);
        }
        Ok(tip.to_string())
    }

    /// The text of the line a thread is on, in the revision it was made on.
    fn thread_text(&self, t: &Doc) -> String {
        // Threads keep their line's text from when they were made.
        t.s("text").to_string()
    }
}

/// The index of the diff line in `d` adding `text` to `path`.
fn added_line(d: &[u8], path: &str, text: &str) -> Option<usize> {
    let mut cur = "";
    for (i, l) in annotated::lines(d).iter().enumerate() {
        let s = std::str::from_utf8(l).unwrap_or("").trim_end_matches('\n');
        if let Some(p) = s.strip_prefix("path ") {
            cur = p;
        } else if cur == path && s.strip_prefix('+') == Some(text) {
            return Some(i);
        }
    }
    None
}

fn summary(d: &Doc) -> J {
    json!({
        "number": d.s("number"), "title": d.s("title"), "author": d.s("author"),
        "source": d.s("source"), "target": d.s("target"), "state": d.s("state"),
        "created": d.s("created"), "updated": d.s("updated"),
        "revisions": d.doc("revisions").0.len(),
    })
}
