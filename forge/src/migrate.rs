//! Moving repositories from Gitea into the forge (spec/forge/migrate.md).
//!
//! Gitea is only read: API `GET` requests, and `git fetch` of each
//! repository into the forge's mirror cache. Nothing is written to it.

use crate::names::{ProjectId, RESERVED, valid};
use crate::record::{self, Doc, Record};
use crate::{Error, Forge, Result};
use cas_version::{Edit, Path};
use serde_json::Value as J;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path as FsPath;

/// Gitea's API, read-only: this type has no way to send anything but `GET`.
pub struct Gitea {
    agent: ureq::Agent,
    /// The web base, e.g. `https://gitea.example`; the API is under `/api/v1`.
    pub web: String,
    token: Option<String>,
}

impl Gitea {
    /// `proxy`: an `http://`, `socks5://` or `socks5h://` proxy for every request.
    pub fn new(web: &str, token: Option<String>, proxy: Option<&str>) -> Result<Gitea> {
        let mut cfg = ureq::Agent::config_builder().http_status_as_error(false).https_only(true);
        if let Some(p) = proxy {
            cfg = cfg.proxy(Some(ureq::Proxy::new(p).map_err(|e| Error::bad(format!("proxy {p}: {e}")))?));
        }
        Ok(Gitea { agent: cfg.build().into(), web: web.trim_end_matches('/').to_string(), token })
    }

    /// `GET /api/v1/<path>`; `None` for 404 (and for 403, which Gitea also
    /// answers for what a token's scope does not cover).
    pub fn get(&self, path: &str) -> Result<Option<J>> {
        let url = format!("{}/api/v1/{path}", self.web);
        let mut r = self.agent.get(&url).header("Accept", "application/json");
        if let Some(t) = &self.token {
            r = r.header("Authorization", &format!("token {t}"));
        }
        let mut resp = r.call().map_err(|e| Error::new(502, format!("gitea {path}: {e}")))?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().with_config().limit(256 << 20).read_to_string().unwrap_or_default();
        match status {
            200 => serde_json::from_str(&text).map(Some).map_err(|e| Error::new(502, format!("gitea {path}: {e}"))),
            403 | 404 => Ok(None),
            s => Err(Error::new(502, format!("gitea {path}: {s} {}", text.chars().take(200).collect::<String>()))),
        }
    }

    /// Every item of a paged list.
    pub fn all(&self, path: &str) -> Result<Vec<J>> {
        let sep = if path.contains('?') { '&' } else { '?' };
        let mut out = Vec::new();
        for page in 1.. {
            let Some(J::Array(items)) = self.get(&format!("{path}{sep}limit=50&page={page}"))? else { break };
            let n = items.len();
            out.extend(items);
            if n < 50 {
                break;
            }
        }
        Ok(out)
    }

    /// The repositories the token sees: `owner/name`.
    pub fn repos(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for page in 1.. {
            let Some(j) = self.get(&format!("repos/search?limit=50&page={page}"))? else { break };
            let items = j["data"].as_array().cloned().unwrap_or_default();
            out.extend(items.iter().filter_map(|r| r["full_name"].as_str().map(String::from)));
            if items.len() < 50 {
                break;
            }
        }
        Ok(out)
    }
}

/// A Gitea name as a forge name: lowercase, `.` `_` and spaces as `-`, a
/// reserved name prefixed with `gitea-`.
pub fn forge_name(name: &str) -> Option<String> {
    let mut n: String = name
        .chars()
        .map(|c| match c {
            'A'..='Z' => c.to_ascii_lowercase(),
            '.' | '_' | ' ' => '-',
            c => c,
        })
        .collect();
    n = n.trim_start_matches('-').to_string();
    if RESERVED.contains(&n.as_str()) {
        n = format!("gitea-{n}");
    }
    valid(&n).then_some(n)
}

/// Seconds since 1970 of an RFC 3339 time (`2026-11-10T21:11:01+02:00`).
pub fn rfc3339(t: &str) -> Option<u64> {
    let b = t.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| t.get(r)?.parse::<i64>().ok();
    let (y, m, d, hh, mm, ss) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    // Days from civil (Howard Hinnant).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let mut secs = days * 86400 + hh * 3600 + mm * 60 + ss;
    let mut rest = &t[19..];
    if let Some(r) = rest.strip_prefix('.') {
        rest = r.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    if let Some(sign @ ('+' | '-')) = rest.chars().next() {
        let off = num_of(&rest[1..3])? * 3600 + num_of(&rest[4..6])? * 60;
        secs -= if sign == '+' { off } else { -off };
    }
    u64::try_from(secs).ok()
}

fn num_of(s: &str) -> Option<i64> {
    s.parse().ok()
}

fn s(j: &J, k: &str) -> String {
    match &j[k] {
        J::String(x) => x.clone(),
        J::Null => String::new(),
        x => x.to_string(),
    }
}

fn time(j: &J, k: &str) -> u64 {
    rfc3339(&s(j, k)).unwrap_or(0)
}

/// What a migration does with one repository.
#[derive(Debug, Default)]
pub struct Moved {
    pub project: String,
    pub commits: usize,
    pub histories: Vec<String>,
    pub labels: usize,
    pub issues: usize,
    pub comments: usize,
    pub proposals: usize,
    pub accounts: Vec<String>,
    pub notes: Vec<String>,
}

/// How a migration runs.
pub struct Plan<'a> {
    /// The forge account that makes everything (an administrator).
    pub admin: &'a str,
    /// The git URL of a repository from its `owner/name` (default: the web
    /// base, `/owner/name.git`).
    pub git_base: Option<&'a str>,
    /// Only read Gitea and say what would happen.
    pub dry_run: bool,
    pub cache: &'a FsPath,
}

impl Forge {
    fn ensure_user(&self, g: &Gitea, login: &str, made: &mut Vec<String>, dry: bool) -> Result<String> {
        let name = forge_name(login).ok_or_else(|| Error::bad(format!("Gitea user {login:?} has no forge name")))?;
        if self.account(&name)?.is_none() && !made.contains(&name) {
            if !dry {
                let email = g.get(&format!("users/{login}"))?.map(|u| s(&u, "email")).filter(|m| !m.is_empty());
                self.create_user(&name, None, email.as_deref())?;
            }
            made.push(name.clone());
        }
        Ok(name)
    }

    /// Move one Gitea repository, `owner/name`, into a new project.
    pub fn migrate_repo(&self, g: &Gitea, full: &str, plan: &Plan) -> Result<Moved> {
        let mut out = Moved::default();
        let repo =
            g.get(&format!("repos/{full}"))?.ok_or_else(|| Error::missing(format!("no Gitea repository {full}")))?;
        let login = s(&repo["owner"], "login");
        let (owner, name) = match (forge_name(&login), forge_name(&s(&repo, "name"))) {
            (Some(o), Some(n)) => (o, n),
            _ => return Err(Error::bad(format!("{full} has no forge name"))),
        };
        let p = ProjectId::new(&owner, &name)?;
        out.project = p.to_string();
        if self.settings(&p)?.is_some() {
            return Err(Error::conflict(format!("{p} exists; delete it to migrate {full} again")));
        }
        let dry = plan.dry_run;
        // The owner: an organization when Gitea knows one by that name.
        if self.account(&owner)?.is_none() {
            match g.get(&format!("orgs/{login}"))? {
                Some(_) => {
                    if !dry {
                        self.create_org(&owner, plan.admin)?;
                    }
                    out.accounts.push(format!("{owner} (organization)"));
                    for m in g.all(&format!("orgs/{login}/members"))? {
                        let u = self.ensure_user(g, &s(&m, "login"), &mut out.accounts, dry)?;
                        if !dry {
                            self.edit_account(&owner, |d| {
                                d.edit("members", |x| {
                                    x.set(&u, "member");
                                });
                                Ok(())
                            })?;
                        }
                    }
                }
                None => {
                    self.ensure_user(g, &login, &mut out.accounts, dry)?;
                }
            }
        }
        let default = s(&repo, "default_branch");
        let public = repo["private"].as_bool() == Some(false);
        if !dry {
            self.create_project(plan.admin, &p, &s(&repo, "description"), public)?;
            self.edit_settings(plan.admin, &p, "migrated from Gitea", |d| {
                d.set("from", format!("{}/{full}", g.web));
                Ok(())
            })?;
        }
        // Collaborators keep their permission as a role.
        for c in g.all(&format!("repos/{full}/collaborators"))? {
            let u = self.ensure_user(g, &s(&c, "login"), &mut out.accounts, dry)?;
            let perm = g.get(&format!("repos/{full}/collaborators/{}/permission", s(&c, "login")))?;
            let role = match perm.as_ref().map(|x| s(x, "permission")).as_deref() {
                Some("owner" | "admin") => crate::names::Role::Admin,
                Some("write") => crate::names::Role::Write,
                _ => crate::names::Role::Read,
            };
            if !dry && u != owner {
                self.set_member(plan.admin, &p, &u, Some(role))?;
            }
        }
        // Labels; Gitea's exclusive scoped labels (`scope/name`) are a set
        // with rule `one`.
        let labels = g.all(&format!("repos/{full}/labels"))?;
        out.labels = labels.len();
        if !dry {
            self.edit_settings(plan.admin, &p, "labels from Gitea", |d| {
                let mut sets = Vec::new();
                d.edit("labels", |l| {
                    for x in &labels {
                        let n = s(x, "name");
                        let color = format!("#{}", s(x, "color").trim_start_matches('#'));
                        let set = (x["exclusive"].as_bool() == Some(true))
                            .then(|| n.rsplit_once('/').map(|(a, _)| a.to_string()))
                            .flatten();
                        l.edit(&n, |y| {
                            y.set("color", color);
                            if let Some(st) = &set {
                                y.set("set", st.as_str());
                            }
                            let desc = s(x, "description");
                            if !desc.is_empty() {
                                y.set("description", desc);
                            }
                        });
                        sets.extend(set);
                    }
                });
                d.edit("sets", |x| {
                    for st in &sets {
                        if x.opt(st).is_none() {
                            x.set(st, "one");
                        }
                    }
                });
                Ok(())
            })?;
        }
        // The code, through a mirror: git fetch only reads.
        let git = match plan.git_base {
            Some(b) => format!("{}/{full}.git", b.trim_end_matches('/')),
            None => format!("{}/{full}.git", g.web),
        };
        if !dry && !repo["empty"].as_bool().unwrap_or(false) {
            self.add_mirror(plan.admin, &p, "gitea", &git)?;
            let pulled = self.pull_mirror(plan.admin, &p, "gitea", plan.cache)?;
            out.commits = pulled.commits;
            out.histories = pulled.moved;
        }
        // Issues and pull requests share Gitea's numbers; they keep them.
        let mut top = 0;
        let issues = g.all(&format!("repos/{full}/issues?state=all&type=issues"))?;
        let mut edits: BTreeMap<Path, Edit> = BTreeMap::new();
        for i in &issues {
            let n = i["number"].as_u64().unwrap_or(0);
            top = top.max(n);
            let author = self.ensure_user(g, &s(&i["user"], "login"), &mut out.accounts, dry)?;
            let mut d = Doc::new();
            d.set("title", s(i, "title").trim())
                .set("state", if s(i, "state") == "closed" { "closed" } else { "open" });
            d.set("author", &author)
                .set_num("created", time(i, "created_at"))
                .set_num("updated", time(i, "updated_at"));
            d.set_opt("body", Some(&s(i, "body")));
            if i["closed_at"].is_string() {
                d.set_num("closed", time(i, "closed_at"));
            }
            d.set("from", s(i, "html_url"));
            let mut ls = Doc::new();
            for l in i["labels"].as_array().into_iter().flatten() {
                ls.set(&s(l, "name"), "");
            }
            d.set_doc("labels", ls);
            let mut a = Doc::new();
            for x in i["assignees"].as_array().into_iter().flatten() {
                a.set(&self.ensure_user(g, &s(x, "login"), &mut out.accounts, dry)?, "");
            }
            d.set_doc("assignees", a);
            if !dry {
                edits.insert(Path::new(format!("{n}/issue.uni"))?, Edit::Set(self.put_doc(&d)?));
            }
            if i["comments"].as_u64().unwrap_or(0) > 0 {
                for (k, c) in g.all(&format!("repos/{full}/issues/{n}/comments"))?.iter().enumerate() {
                    let who = self.ensure_user(g, &s(&c["user"], "login"), &mut out.accounts, dry)?;
                    let mut cd = Doc::new();
                    cd.set("author", who).set_num("time", time(c, "created_at")).set("text", s(c, "body"));
                    out.comments += 1;
                    if !dry {
                        edits.insert(Path::new(format!("{n}/c/{}.uni", k + 1))?, Edit::Set(self.put_doc(&cd)?));
                    }
                }
            }
        }
        out.issues = issues.len();
        if !dry && !edits.is_empty() {
            // One step for the whole import.
            self.commit_issues(plan.admin, &p, |_| Ok(edits.clone()))?;
        }
        // Pull requests become proposals. An open one whose branch came
        // with the code is a live proposal; the rest keep their record.
        let pulls = g.all(&format!("repos/{full}/pulls?state=all"))?;
        out.proposals = pulls.len();
        for pr in &pulls {
            let n = pr["number"].as_u64().unwrap_or(0);
            top = top.max(n);
            let author = self.ensure_user(g, &s(&pr["user"], "login"), &mut out.accounts, dry)?;
            let mut head = s(&pr["head"], "ref");
            if head.starts_with("refs/pull/") {
                // Gitea names a deleted branch by its pull request.
                head = format!("pr-{n}");
            }
            let same_repo = s(&pr["head"]["repo"], "full_name") == full;
            let branch_var = format!("u/{owner}/{}", head.replace('/', "-"));
            let base = s(&pr["base"], "ref");
            let target = if base == default {
                crate::project::DEV.to_string()
            } else {
                format!("u/{owner}/{}", base.replace('/', "-"))
            };
            let state = if pr["merged"].as_bool() == Some(true) {
                "merged"
            } else if s(pr, "state") == "closed" {
                "closed"
            } else {
                "open"
            };
            let mut r = Record::default();
            r.doc.set_num("number", n).set("title", first_line(&s(pr, "title"))).set("author", &author);
            r.doc.set(
                "source",
                if same_repo { branch_var.clone() } else { format!("u/{author}/{}", head.replace('/', "-")) },
            );
            r.doc.set("target", &target).set("state", state);
            r.doc.set_num("created", time(pr, "created_at")).set_num("updated", time(pr, "updated_at"));
            // What Gitea had that a proposal has no field for.
            let mut im = Doc::new();
            im.set("from", s(pr, "html_url")).set_opt("body", Some(&s(pr, "body")));
            im.set("head", format!("{head} {}", s(&pr["head"], "sha")))
                .set("base", format!("{base} {}", s(&pr["base"], "sha")));
            if let Some(m) = pr["merge_commit_sha"].as_str() {
                im.set("merge_commit", m);
            }
            if pr["comments"].as_u64().unwrap_or(0) > 0 {
                for c in g.all(&format!("repos/{full}/issues/{n}/comments"))? {
                    let who = self.ensure_user(g, &s(&c["user"], "login"), &mut out.accounts, dry)?;
                    let mut cd = Doc::new();
                    cd.set("author", who).set_num("time", time(&c, "created_at")).set("text", s(&c, "body"));
                    im.push("comments", cd);
                    out.comments += 1;
                }
            }
            for rv in g.all(&format!("repos/{full}/pulls/{n}/reviews"))? {
                let who = self.ensure_user(g, &s(&rv["user"], "login"), &mut out.accounts, dry)?;
                let id = s(&rv, "id");
                let mut rd = Doc::new();
                rd.set("author", &who).set_num("time", time(&rv, "submitted_at")).set("state", s(&rv, "state"));
                rd.set("text", s(&rv, "body"));
                for c in g.all(&format!("repos/{full}/pulls/{n}/reviews/{id}/comments"))? {
                    let mut cd = Doc::new();
                    cd.set("path", s(&c, "path")).set("line", s(&c, "position")).set("text", s(&c, "body"));
                    cd.set("author", self.ensure_user(g, &s(&c["user"], "login"), &mut out.accounts, dry)?);
                    rd.push("comments", cd);
                }
                im.push("reviews", rd);
            }
            r.doc.set_doc("imported", im);
            if state == "open" && same_repo && out.histories.contains(&branch_var) && target.starts_with("h/") {
                self.revise(&p, &mut r)?;
            } else if state == "open" {
                out.notes.push(format!("#{n} stays open without a revision: its branch {head} did not come across"));
            }
            if !dry {
                record::save(&*self.store, self.alg, &p.proposal(n), None, &r)?;
            }
        }
        if !dry && top > 0 {
            record::update(&*self.store, self.alg, &p.meta(), |cur| {
                let mut r = cur.ok_or_else(|| Error::missing("project gone"))?;
                let next = r.doc.num("next").max(top + 1);
                r.doc.set_num("next", next);
                Ok((Some(r), ()))
            })?;
        }
        if g.all(&format!("repos/{full}/releases"))?.iter().any(|r| !s(r, "body").trim().is_empty()) {
            out.notes.push(
                "Gitea release notes are not copied; tags came across as releases with their tag messages".into(),
            );
        }
        if repo["has_wiki"].as_bool() == Some(true)
            && g.get(&format!("repos/{full}/wiki/pages?limit=1"))?
                .is_some_and(|w| w.as_array().is_some_and(|a| !a.is_empty()))
        {
            out.notes.push("the wiki is not migrated".into());
        }
        let users: BTreeSet<&String> = out.accounts.iter().collect();
        out.accounts = users.into_iter().cloned().collect();
        Ok(out)
    }
}

fn first_line(t: &str) -> String {
    t.lines().next().unwrap_or("").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_and_names() {
        assert_eq!(rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(rfc3339("2026-11-10T21:11:01+02:00"), Some(1794337861));
        assert_eq!(rfc3339("2026-11-10T19:11:01.123Z"), Some(1794337861));
        assert_eq!(forge_name("Thesis").as_deref(), Some("thesis"));
        assert_eq!(forge_name("gurny.me").as_deref(), Some("gurny-me"));
        assert_eq!(forge_name("Final-Project---Automata").as_deref(), Some("final-project---automata"));
        assert_eq!(forge_name("api").as_deref(), Some("gitea-api"));
    }
}
