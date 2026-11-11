//! The JSON API under `/api/v1`, the wire mounted at each project's path,
//! and the web pages.

use crate::accounts::{Caller, Scope};
use crate::browse::view;
use crate::issues::Change;
use crate::names::{ProjectId, Role};
use crate::record::Doc;
use crate::{Error, Forge, Result};
use cas_core::server::{Request, Response, Route, Router};
use serde_json::{Value as J, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// How the server runs.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// The built web pages (the SvelteKit app), if served.
    pub web: Option<PathBuf>,
    /// Where mirrors fetch into.
    pub cache: PathBuf,
    /// Anyone may sign up; otherwise only the first user, or an admin.
    pub open_registration: bool,
}

/// Wire paths: what the core server answers.
fn wire_path(rest: &str) -> bool {
    ["k/", "remove/", "batch/", "chunk/", "chunks/"].iter().any(|p| rest.starts_with(p)) || rest == "list"
}

pub fn router(forge: Arc<Forge>, opts: Options) -> Arc<Router> {
    Arc::new(move |req: &Request| {
        let (path, query) = req.url.split_once('?').unwrap_or((&req.url, ""));
        if let Some(rest) = path.strip_prefix("/p/") {
            let mut parts = rest.splitn(3, '/');
            let (Some(o), Some(n), Some(rest)) = (parts.next(), parts.next(), parts.next()) else {
                return Some(Route::Reply(Response::new(404)));
            };
            let Ok(p) = ProjectId::new(o, n) else { return Some(Route::Reply(Response::new(404))) };
            if wire_path(rest) {
                let url = if query.is_empty() { format!("/{rest}") } else { format!("/{rest}?{query}") };
                return Some(Route::Wire { prefix: p.prefix(), url });
            }
        }
        if let Some(rest) = path.strip_prefix("/api/v1/") {
            return Some(Route::Reply(api(&forge, &opts, req, rest, query)));
        }
        if let Some((p, what)) = crate::git::route(path) {
            return Some(Route::Reply(forge.git(req, &p, what, query, &opts.cache)));
        }
        if wire_path(path.trim_start_matches('/')) {
            return None;
        }
        Some(Route::Reply(web(&opts, path)))
    })
}

fn json_response(status: u16, j: &J) -> Response {
    Response::data(serde_json::to_vec(j).unwrap_or_default())
        .status(status)
        .with("Content-Type", "application/json")
        .with("Cache-Control", "no-store")
}

fn error(e: &Error) -> Response {
    json_response(e.status, &json!({ "message": e.message }))
}

/// The caller of a request: a token in `Authorization` (`Bearer` or
/// `token`, as Gitea's API takes it) or the `cas_token` cookie.
pub fn caller(forge: &Forge, req: &Request) -> Option<Caller> {
    let token = req
        .header("Authorization")
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("token ")))
        .map(str::trim)
        .map(String::from)
        .or_else(|| {
            req.header("Cookie")?.split(';').find_map(|c| c.trim().strip_prefix("cas_token=").map(String::from))
        })?;
    forge.caller_for_token(&token)
}

/// Query parameters, decoded.
fn params(query: &str) -> std::collections::BTreeMap<String, String> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            let dec = |s: &str| percent_decode(&s.replace('+', " "));
            (dec(k), dec(v))
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(x) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16)
        {
            out.push(x);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A request body, as JSON.
struct Body(J);

impl Body {
    fn of(req: &Request) -> Result<Body> {
        if req.body.is_empty() {
            return Ok(Body(json!({})));
        }
        serde_json::from_slice(&req.body).map(Body).map_err(|e| Error::bad(format!("body: {e}")))
    }
    fn s(&self, k: &str) -> &str {
        self.0.get(k).and_then(J::as_str).unwrap_or("")
    }
    fn opt(&self, k: &str) -> Option<&str> {
        self.0.get(k).and_then(J::as_str)
    }
    fn need(&self, k: &str) -> Result<&str> {
        self.opt(k).ok_or_else(|| Error::bad(format!("the body needs {k:?}")))
    }
    fn flag(&self, k: &str) -> bool {
        match self.0.get(k) {
            Some(J::Bool(b)) => *b,
            Some(J::String(s)) => s == "true",
            _ => false,
        }
    }
    fn num(&self, k: &str) -> Option<u64> {
        match self.0.get(k)? {
            J::Number(n) => n.as_u64(),
            J::String(s) => s.parse().ok(),
            _ => None,
        }
    }
    fn strings(&self, k: &str) -> Vec<String> {
        self.0
            .get(k)
            .and_then(J::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default()
    }
}

fn signed_in(c: &Option<Caller>) -> Result<&Caller> {
    c.as_ref().ok_or_else(Error::unauthorized)
}

/// Signed in with a token for everything: what the account itself does
/// (password, tokens, organizations, new projects) a narrower token may not.
fn full(c: &Option<Caller>) -> Result<&Caller> {
    let c = signed_in(c)?;
    match c.scope {
        crate::accounts::Scope::All => Ok(c),
        _ => Err(Error::denied("this token is limited; the account's own settings need a token for everything")),
    }
}

fn num(s: &str) -> Result<u64> {
    s.parse().map_err(|_| Error::bad(format!("{s:?} is not a number")))
}

fn api(forge: &Arc<Forge>, opts: &Options, req: &Request, rest: &str, query: &str) -> Response {
    let c = caller(forge, req);
    let q = params(query);
    // Events stream as server-sent events.
    if rest == "events" && req.method == "GET" && req.header("Accept").is_some_and(|a| a.contains("text/event-stream"))
    {
        return events_stream(forge.clone(), c, q.get("after").and_then(|a| a.parse().ok()));
    }
    // Archives are files, not JSON.
    let segs: Vec<&str> = rest.split('/').collect();
    if let ["projects", o, n, "releases", tag, "archive.tar.gz"] = segs[..] {
        let r = (|| {
            let p = ProjectId::new(o, n)?;
            forge.require(c.as_ref(), &p, Role::Read)?;
            forge.archive(&p, &percent_decode(tag))
        })();
        return match r {
            Ok((name, bytes)) => Response::data(bytes)
                .with("Content-Type", "application/gzip")
                .with("Content-Disposition", &format!("attachment; filename=\"{name}.tar.gz\"")),
            Err(e) => error(&e),
        };
    }
    if let ["projects", o, n, "raw"] = segs[..] {
        let r = (|| {
            let p = ProjectId::new(o, n)?;
            forge.require(c.as_ref(), &p, Role::Read)?;
            let v = version_of(forge, &p, &q)?;
            forge.file(&v, q.get("path").map_or("", String::as_str))
        })();
        return match r {
            Ok((v, bytes)) => {
                let bytes = match v.known_format().map(|f| f.alias) {
                    Some("git-blob") => {
                        cas_version::formats::git_blob_contents(&bytes).map(<[u8]>::to_vec).unwrap_or(bytes)
                    }
                    _ => bytes,
                };
                Response::data(bytes).with("Content-Type", "application/octet-stream")
            }
            Err(e) => error(&e),
        };
    }
    let r = Body::of(req).and_then(|b| route(forge, opts, req.method.as_str(), &segs, &q, &b, &c));
    match r {
        Ok((status, j)) => json_response(status, &j),
        Err(e) => error(&e),
    }
}

/// The version a browsing request names: `version`, or node `node` (empty:
/// the tip) of history `history` (default `h/dev`).
fn version_of(
    forge: &Forge,
    p: &ProjectId,
    q: &std::collections::BTreeMap<String, String>,
) -> Result<cas_version::View> {
    if let Some(v) = q.get("version").filter(|v| !v.is_empty()) {
        let v = view(v)?;
        // Only versions the project reaches.
        if !forge.reach.reaches(&*forge.store, p, v.key()) {
            return Err(Error::missing("no such version in this project"));
        }
        return Ok(v);
    }
    let var = q.get("history").map_or(crate::project::DEV, String::as_str);
    let (_, h) = forge.require_history(p, var)?;
    Ok(forge.node(&h, q.get("node").map_or("", String::as_str))?.version())
}

type Out = Result<(u16, J)>;

fn ok(j: J) -> Out {
    Ok((200, j))
}

fn done() -> Out {
    Ok((200, json!({ "ok": true })))
}

fn route(
    forge: &Arc<Forge>,
    opts: &Options,
    method: &str,
    segs: &[&str],
    q: &std::collections::BTreeMap<String, String>,
    b: &Body,
    c: &Option<Caller>,
) -> Out {
    let f = &**forge;
    let qs = |k: &str| q.get(k).map_or("", String::as_str).to_string();
    match (method, segs) {
        // Accounts.
        ("POST", ["users"]) => {
            let first = f.accounts()?.values().all(|a| a.s("kind") != "user");
            let admin = full(c).is_ok_and(|c| f.is_admin(&c.user));
            if !(first || admin || opts.open_registration) {
                return Err(Error::denied("sign-up is closed: an administrator makes accounts"));
            }
            let name = b.need("name")?;
            f.create_user(name, b.opt("password"), b.opt("email"))?;
            Ok((201, json!({ "name": name })))
        }
        ("POST", ["login"]) => {
            let token = f.sign_in(b.need("name")?, b.need("password")?)?;
            ok(json!({ "token": token, "expires_in": crate::accounts::SESSION }))
        }
        ("GET", ["user"]) => {
            let c = signed_in(c)?;
            let a = f.account(&c.user)?.ok_or_else(|| Error::missing("no account"))?;
            ok(account_json(&c.user, &a, true))
        }
        ("GET", ["users", name]) => {
            let a = f.account(name)?.ok_or_else(|| Error::missing(format!("no account {name}")))?;
            ok(account_json(name, &a, false))
        }
        ("POST", ["user", "password"]) => {
            let c = full(c)?;
            f.sign_in(&c.user, b.need("old")?)?;
            f.set_password(&c.user, b.need("new")?)?;
            done()
        }
        ("PATCH", ["user"]) => {
            let c = full(c)?;
            f.edit_account(&c.user, |d| {
                if let Some(x) = b.opt("display") {
                    d.set_opt("display", Some(x));
                }
                if let Some(x) = b.opt("email") {
                    d.set_opt("email", Some(x));
                }
                Ok(())
            })?;
            done()
        }
        ("GET", ["user", "tokens"]) => {
            let c = full(c)?;
            let a = f.account(&c.user)?.ok_or_else(|| Error::missing("no account"))?;
            let list: Vec<J> = a
                .entries("tokens")
                .into_iter()
                .map(|(k, t)| {
                    let mut j = t.json();
                    j["key"] = json!(k);
                    j
                })
                .collect();
            ok(json!(list))
        }
        ("POST", ["user", "tokens"]) => {
            let c = full(c)?;
            let scope = Scope::parse(b.s("scope"))?;
            let expires = b.num("days").map(|d| crate::now() + d * 86400);
            let token = f.make_token(&c.user, b.opt("label").unwrap_or("token"), &scope, expires)?;
            Ok((201, json!({ "token": token, "key": crate::accounts::token_key(&token) })))
        }
        ("DELETE", ["user", "tokens", key]) => {
            f.revoke_token(&full(c)?.user, key)?;
            done()
        }
        ("POST", ["logout"]) => done(),
        ("POST", ["orgs"]) => {
            let c = full(c)?;
            let name = b.need("name")?;
            f.create_org(name, &c.user)?;
            Ok((201, json!({ "name": name })))
        }
        ("PUT", ["orgs", org, "members", user]) | ("DELETE", ["orgs", org, "members", user]) => {
            let c = full(c)?;
            let a = f
                .account(org)?
                .filter(|a| a.s("kind") == "org")
                .ok_or_else(|| Error::missing(format!("no organization {org}")))?;
            if a.doc("members").s(&c.user) != "owner" && !f.is_admin(&c.user) {
                return Err(Error::denied("only an owner changes an organization's members"));
            }
            if f.account(user)?.is_none() {
                return Err(Error::missing(format!("no user {user}")));
            }
            let role =
                if method == "PUT" { Some(if b.s("role") == "owner" { "owner" } else { "member" }) } else { None };
            f.edit_account(org, |d| {
                d.edit("members", |m| match role {
                    Some(r) => {
                        m.set(user, r);
                    }
                    None => {
                        m.remove(user);
                    }
                });
                Ok(())
            })?;
            done()
        }

        // Projects.
        ("GET", ["projects"]) => {
            let list: Vec<J> = f
                .readable_projects(c.as_ref())?
                .into_iter()
                .map(|(p, r)| {
                    let s = f.settings(&p).ok().flatten();
                    json!({
                        "owner": p.owner, "name": p.name, "role": r.as_str(),
                        "description": s.as_ref().map(|s| s.s("description").to_string()),
                        "visibility": s.as_ref().map(|s| s.s("visibility").to_string()),
                    })
                })
                .collect();
            ok(json!(list))
        }
        ("POST", ["projects"]) => {
            let c = full(c)?;
            let p = ProjectId::new(b.opt("owner").unwrap_or(&c.user), b.need("name")?)?;
            f.create_project(&c.user, &p, b.s("description"), b.flag("public"))?;
            Ok((201, json!({ "project": p.to_string() })))
        }
        (_, ["projects", o, n, rest @ ..]) => {
            let p = ProjectId::new(o, n)?;
            project_route(f, opts, method, &p, rest, &qs, q, b, c.as_ref())
        }

        // Runners.
        ("GET", ["runner", "jobs"]) => ok(json!(f.jobs(signed_in(c)?)?)),
        ("POST", ["runner", "results"]) => {
            let c = signed_in(c)?;
            let p = ProjectId::parse(b.need("project")?)?;
            let v = view(b.need("version")?)?;
            f.post_result(c, &p, &v, b.need("check")?, b.s("status") == "pass", b.s("log").as_bytes())?;
            done()
        }

        // Events.
        ("GET", ["events"]) => {
            let after = q.get("after").and_then(|a| a.parse().ok()).unwrap_or(0);
            let list = f.events_after(after, 200)?;
            let visible: Vec<J> = list.iter().filter(|e| visible(f, c.as_ref(), e)).map(Doc::json).collect();
            ok(json!({ "last": f.last_event()?, "events": visible }))
        }
        _ => Err(Error::missing("no such API")),
    }
}

/// Whether `c` may see event `e`: events of projects they can read, and
/// their own.
fn visible(f: &Forge, c: Option<&Caller>, e: &Doc) -> bool {
    match e.opt("project").and_then(|p| ProjectId::parse(p).ok()) {
        Some(p) => f.caller_role(c, &p).ok().flatten().is_some(),
        None => c.is_some_and(|c| e.s("actor") == c.user || f.is_admin(&c.user)),
    }
}

fn account_json(name: &str, a: &Doc, own: bool) -> J {
    let mut j = json!({
        "name": name, "kind": a.s("kind"), "display": a.opt("display"), "created": a.s("created"),
        "admin": a.flag("admin"),
    });
    if own {
        j["email"] = json!(a.opt("email"));
    }
    if a.s("kind") == "org" {
        j["members"] = a.doc("members").json();
    }
    j
}

#[allow(clippy::too_many_arguments)]
fn project_route(
    f: &Forge,
    opts: &Options,
    method: &str,
    p: &ProjectId,
    rest: &[&str],
    qs: &dyn Fn(&str) -> String,
    q: &std::collections::BTreeMap<String, String>,
    b: &Body,
    c: Option<&Caller>,
) -> Out {
    let me = || c.ok_or_else(Error::unauthorized);
    match (method, rest) {
        ("GET", []) => {
            let role = f.require(c, p, Role::Read)?;
            let s = f.settings(p)?.ok_or_else(|| Error::missing("no project"))?;
            let mut j = s.json();
            if role < Role::Admin {
                j.as_object_mut().map(|o| o.remove("webhooks"));
            } else if let Some(w) = j.get_mut("webhooks").and_then(J::as_object_mut) {
                for (_, h) in w.iter_mut() {
                    h.as_object_mut().map(|o| o.insert("secret".into(), json!("(set)")));
                }
            }
            j["owner"] = json!(p.owner);
            j["name"] = json!(p.name);
            j["role"] = json!(role.as_str());
            j["histories"] = json!(f.histories(p)?);
            let readme =
                f.history(p, crate::project::DEV)?.and_then(|(_, h)| h.tip()).and_then(|t| f.readme(p, &t.version()));
            j["readme"] = json!(readme.map(|(path, text)| json!({ "path": path, "text": text })));
            j["releases"] = json!(f.releases(p)?.into_iter().take(1).collect::<Vec<_>>());
            ok(j)
        }
        ("PATCH", []) => {
            let c = me()?;
            f.require(Some(c), p, Role::Admin)?;
            f.edit_settings(&c.user, p, "settings", |d| {
                for k in crate::project::EDITABLE {
                    if let Some(v) = b.opt(k) {
                        if *k == "visibility" && !matches!(v, "public" | "private") {
                            return Err(Error::bad("visibility is public or private"));
                        }
                        d.set_opt(k, Some(v));
                    }
                }
                Ok(())
            })?;
            done()
        }
        ("DELETE", []) => {
            let c = me()?;
            f.require(Some(c), p, Role::Admin)?;
            f.delete_project(&c.user, p)?;
            done()
        }
        ("PUT", ["members", m]) | ("DELETE", ["members", m]) => {
            let c = me()?;
            f.require(Some(c), p, Role::Admin)?;
            let role = if method == "PUT" { Some(Role::parse(b.need("role")?)?) } else { None };
            f.set_member(&c.user, p, m, role)?;
            done()
        }
        ("PUT", ["protected", kind, name @ ..]) => {
            let c = me()?;
            f.require(Some(c), p, Role::Admin)?;
            f.set_protected(&c.user, p, &format!("{kind}/{}", name.join("/")), b.flag("on"))?;
            done()
        }
        ("PUT", ["gates", kind, name @ ..]) => {
            let c = me()?;
            f.require(Some(c), p, Role::Admin)?;
            let h = format!("{kind}/{}", name.join("/"));
            crate::names::check_history(&h)?;
            f.edit_settings(&c.user, p, &format!("gates of {h}"), |d| {
                d.edit("gates", |g| {
                    g.edit(&h, |x| {
                        x.set_num("approvals", b.num("approvals").unwrap_or(0));
                        x.set_opt("resolved", b.flag("resolved").then_some("true"));
                        let mut checks = Doc::new();
                        for n in b.strings("checks") {
                            checks.set(&n, "");
                        }
                        x.set_doc("checks", checks);
                    });
                });
                Ok(())
            })?;
            done()
        }
        ("PUT", ["labels", name]) | ("DELETE", ["labels", name]) => {
            let c = me()?;
            f.require(Some(c), p, Role::Maintain)?;
            let put = method == "PUT";
            f.edit_settings(&c.user, p, &format!("label {name}"), |d| {
                d.edit("labels", |l| {
                    if put {
                        l.edit(name, |x| {
                            x.set("color", b.opt("color").unwrap_or("#cccccc")).set_opt("set", b.opt("set"));
                        });
                    } else {
                        l.remove(name);
                    }
                });
                if put && let Some(set) = b.opt("set").filter(|s| !s.is_empty()) {
                    let rule = if b.s("rule") == "any" { "any" } else { "one" };
                    d.edit("sets", |s| {
                        if s.opt(set).is_none() {
                            s.set(set, rule);
                        }
                    });
                }
                Ok(())
            })?;
            done()
        }
        ("POST", ["webhooks"]) => {
            let c = me()?;
            f.require(Some(c), p, Role::Admin)?;
            let url = b.need("url")?.to_string();
            if !url.starts_with("https://") && !url.starts_with("http://") {
                return Err(Error::bad("a webhook URL is http(s)"));
            }
            let id = crate::accounts::new_token()?[..12].to_string();
            f.edit_settings(&c.user, p, "webhook", |d| {
                d.edit("webhooks", |w| {
                    w.edit(&id, |h| {
                        h.set("url", url.clone())
                            .set("secret", b.s("secret"))
                            .set("events", b.opt("events").unwrap_or("*"));
                    });
                });
                Ok(())
            })?;
            Ok((201, json!({ "id": id })))
        }
        ("DELETE", ["webhooks", id]) => {
            let c = me()?;
            f.require(Some(c), p, Role::Admin)?;
            f.edit_settings(&c.user, p, "webhook removed", |d| {
                d.edit("webhooks", |w| {
                    w.remove(id);
                });
                Ok(())
            })?;
            done()
        }
        ("PUT", ["runners", r]) | ("DELETE", ["runners", r]) => {
            let c = me()?;
            f.require(Some(c), p, Role::Admin)?;
            let on = method == "PUT";
            f.edit_settings(&c.user, p, &format!("runner {r}"), |d| {
                d.edit("runners", |x| {
                    if on {
                        x.set(r, "");
                    } else {
                        x.remove(r);
                    }
                });
                Ok(())
            })?;
            done()
        }
        ("POST", ["fork"]) => {
            let c = me()?;
            f.require(Some(c), p, Role::Read)?;
            let to = ProjectId::new(b.opt("owner").unwrap_or(&c.user), b.opt("name").unwrap_or(&p.name))?;
            f.fork(&c.user, p, &to)?;
            Ok((201, json!({ "project": to.to_string() })))
        }

        // Browsing.
        ("GET", ["histories"]) => {
            f.require(c, p, Role::Read)?;
            ok(json!(f.histories(p)?))
        }
        ("GET", ["log"]) => {
            f.require(c, p, Role::Read)?;
            let skip = qs("skip").parse().unwrap_or(0);
            let limit = qs("limit").parse().unwrap_or(50usize).min(500);
            let path = qs("path");
            ok(f.log(p, &history_param(qs), Some(&path), skip, limit)?)
        }
        ("GET", ["node"]) => {
            f.require(c, p, Role::Read)?;
            ok(f.node_info(p, &history_param(qs), &qs("node"))?)
        }
        ("GET", ["tree"]) => {
            f.require(c, p, Role::Read)?;
            let v = version_of(f, p, q)?;
            let mut j = f.tree(&v, Some(&qs("path")))?;
            j["version"] = json!(v.to_string());
            ok(j)
        }
        ("GET", ["file"]) => {
            f.require(c, p, Role::Read)?;
            let v = version_of(f, p, q)?;
            let path = qs("path");
            let (fv, bytes) = f.file(&v, &path)?;
            let bytes = match fv.known_format().map(|f| f.alias) {
                Some("git-blob") => {
                    cas_version::formats::git_blob_contents(&bytes).map(<[u8]>::to_vec).unwrap_or(bytes)
                }
                _ => bytes,
            };
            let text = (bytes.len() <= crate::browse::MAX_FILE && !bytes.contains(&0))
                .then(|| String::from_utf8(bytes.clone()).ok())
                .flatten();
            ok(json!({
                "path": path, "view": fv.to_string(), "format": crate::browse::format_name(&fv),
                "size": bytes.len(), "text": text,
            }))
        }
        ("GET", ["step"]) => {
            f.require(c, p, Role::Read)?;
            ok(f.step(p, &history_param(qs), &qs("node"))?)
        }
        ("GET", ["compare"]) => {
            f.require(c, p, Role::Read)?;
            let a = q.get("a").filter(|a| !a.is_empty()).map(|a| view(a)).transpose()?;
            let bv = view(&qs("b"))?;
            for v in a.iter().chain([&bv]) {
                if !f.reach.reaches(&*f.store, p, v.key()) {
                    return Err(Error::missing("no such version in this project"));
                }
            }
            ok(f.compare(a.as_ref(), &bv)?)
        }
        ("GET", ["blame"]) => {
            f.require(c, p, Role::Read)?;
            ok(f.blame(p, &history_param(qs), &qs("node"), &qs("path"))?)
        }
        ("GET", ["checks"]) => {
            f.require(c, p, Role::Read)?;
            let v = version_of(f, p, q)?;
            ok(json!(f.checks(&v)?))
        }

        // Proposals.
        ("GET", ["proposals"]) => {
            f.require(c, p, Role::Read)?;
            ok(json!(f.proposals(p, &qs("state"))?))
        }
        ("POST", ["proposals"]) => {
            let c = me()?;
            let target = b.opt("target").unwrap_or(crate::project::DEV);
            let n = f.open_proposal(c, p, b.need("title")?, b.need("source")?, target)?;
            Ok((201, json!({ "number": n })))
        }
        ("GET", ["proposals", n]) => {
            f.require(c, p, Role::Read)?;
            ok(f.proposal_info(p, num(n)?)?)
        }
        ("PATCH", ["proposals", n]) => {
            let c = me()?;
            let n = num(n)?;
            if let Some(t) = b.opt("title") {
                f.retitle(c, p, n, t)?;
            }
            match b.opt("state") {
                Some("closed") => f.set_proposal_state(c, p, n, false)?,
                Some("open") => f.set_proposal_state(c, p, n, true)?,
                Some(s) => return Err(Error::bad(format!("state {s:?}: open or closed (merge to merge)"))),
                None => {}
            }
            done()
        }
        ("POST", ["proposals", n, "threads"]) => {
            let c = me()?;
            let id = f.add_thread(
                c,
                p,
                num(n)?,
                b.need("path")?,
                b.opt("side").unwrap_or("new"),
                b.num("line").unwrap_or(0),
                b.need("text")?,
                b.opt("suggestion"),
            )?;
            Ok((201, json!({ "id": id })))
        }
        ("POST", ["proposals", n, "threads", id, "comments"]) => {
            f.reply(me()?, p, num(n)?, num(id)?, b.need("text")?, b.opt("suggestion"))?;
            done()
        }
        ("POST", ["proposals", n, "threads", id, "resolve"]) => {
            f.resolve_thread(me()?, p, num(n)?, num(id)?, b.0.get("resolved").is_none() || b.flag("resolved"))?;
            done()
        }
        ("POST", ["proposals", n, "threads", id, "promote"]) => {
            f.promote_thread(me()?, p, num(n)?, num(id)?, b.0.get("promote").is_none() || b.flag("promote"))?;
            done()
        }
        ("POST", ["proposals", n, "approve"]) => {
            f.approve(me()?, p, num(n)?)?;
            done()
        }
        ("POST", ["proposals", n, "merge"]) => {
            let tip = f.merge(me()?, p, num(n)?)?;
            ok(json!({ "tip": tip }))
        }

        // Issues.
        ("GET", ["issues"]) => {
            f.require(c, p, Role::Read)?;
            ok(json!(f.issues(p, &qs("state"), &qs("label"))?))
        }
        ("POST", ["issues"]) => {
            let c = me()?;
            let anchor = b.0.get("anchor").filter(|a| a.is_object()).map(|a| {
                let mut d = Doc::new();
                for (k, v) in a.as_object().unwrap() {
                    d.set(k, v.as_str().map(String::from).unwrap_or_else(|| v.to_string()));
                }
                d
            });
            let n =
                f.open_issue(c, p, b.need("title")?, b.s("body"), &b.strings("labels"), anchor, b.opt("suggestion"))?;
            Ok((201, json!({ "number": n })))
        }
        ("GET", ["issues", n]) => {
            f.require(c, p, Role::Read)?;
            ok(f.issue(p, num(n)?)?)
        }
        ("PATCH", ["issues", n]) => {
            let ch = Change {
                title: b.opt("title").map(String::from),
                body: b.opt("body").map(String::from),
                state: b.opt("state").map(String::from),
                add_labels: b.strings("add_labels"),
                remove_labels: b.strings("remove_labels"),
                assignees: b.0.get("assignees").map(|_| b.strings("assignees")),
            };
            f.edit_issue(me()?, p, num(n)?, &ch)?;
            done()
        }
        ("POST", ["issues", n, "comments"]) => {
            let k = f.comment_issue(me()?, p, num(n)?, b.need("text")?)?;
            Ok((201, json!({ "comment": k })))
        }

        // Releases.
        ("GET", ["releases"]) => {
            f.require(c, p, Role::Read)?;
            ok(json!(f.releases(p)?))
        }
        ("POST", ["releases"]) => {
            let tip =
                f.publish_release(me()?, p, b.s("node"), b.need("tag")?, b.need("changelog")?, b.flag("compatible"))?;
            Ok((201, json!({ "id": tip })))
        }

        // Mirrors.
        ("POST", ["mirrors"]) => {
            let c = me()?;
            f.require(Some(c), p, Role::Admin)?;
            f.add_mirror(&c.user, p, b.need("name")?, b.need("url")?)?;
            Ok((201, json!({ "ok": true })))
        }
        ("POST", ["mirrors", name, "pull"]) => {
            let c = me()?;
            f.require(Some(c), p, Role::Admin)?;
            let r = f.pull_mirror(&c.user, p, name, &opts.cache)?;
            ok(json!({ "commits": r.commits, "moved": r.moved }))
        }
        _ => Err(Error::missing("no such API")),
    }
}

fn history_param(qs: &dyn Fn(&str) -> String) -> String {
    let h = qs("history");
    if h.is_empty() { crate::project::DEV.to_string() } else { h }
}

/// The event log as server-sent events, from after `after` (default: now).
fn events_stream(forge: Arc<Forge>, c: Option<Caller>, after: Option<u64>) -> Response {
    let start = after.unwrap_or_else(|| forge.last_event().unwrap_or(0));
    Response::stream(Box::new(move |w| {
        let mut last = start;
        w.write_all(b": cas forge events\n\n")?;
        w.flush()?;
        loop {
            let newest = forge.events.wait(last, Duration::from_secs(15));
            if newest <= last {
                // A comment keeps the connection open and finds a gone client.
                w.write_all(b": keep-alive\n\n")?;
                w.flush()?;
                continue;
            }
            for e in forge.events_after(last, 500).map_err(|e| std::io::Error::other(e.message))? {
                last = last.max(e.num("number"));
                if visible(&forge, c.as_ref(), &e) {
                    let data = serde_json::to_string(&e.json()).unwrap_or_default();
                    write!(w, "id: {}\nevent: {}\ndata: {data}\n\n", e.num("number"), e.s("kind"))?;
                }
            }
            w.flush()?;
        }
    }))
    .with("Content-Type", "text/event-stream")
    .with("Cache-Control", "no-store")
}

/// The web pages: files of the built app, and its `index.html` for any
/// other path (the app routes in the browser).
fn web(opts: &Options, path: &str) -> Response {
    let Some(root) = &opts.web else {
        return Response::data("cas forge: the API is at /api/v1; no web pages are served (start with --web <dir>)\n")
            .with("Content-Type", "text/plain; charset=utf-8");
    };
    let rel = path.trim_start_matches('/');
    let safe = !rel.split('/').any(|s| s == ".." || s.starts_with('.'));
    let file = if safe && !rel.is_empty() { Some(root.join(rel)) } else { None };
    let (file, cache) = match file.filter(|f| f.is_file()) {
        Some(f) => (f, rel.starts_with("_app/immutable/")),
        None => (root.join("index.html"), false),
    };
    match std::fs::read(&file) {
        Ok(bytes) => Response::data(bytes)
            .with("Content-Type", mime(&file))
            .with("Cache-Control", if cache { "public, max-age=31536000, immutable" } else { "no-cache" }),
        Err(_) => Response::new(404),
    }
}

fn mime(p: &std::path::Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}
