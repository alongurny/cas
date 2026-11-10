//! The forge's commands (spec/forge): accounts, projects and their
//! settings, proposals, issues, releases, mirrors and events, over the
//! forge's JSON API. Everything the web pages do, `cas` does too.
//!
//! The forge is the host of `--store`: `--store https://host/p/<owner>/<project>`
//! names the project as well (the same URL the history commands push to);
//! `--project <owner>/<project>` names another. `--json` prints the API's
//! answer as it is. Tokens and passwords come from files or stdin, never
//! from the command line or the environment.

use super::{Args, Res, e};
use serde_json::{Value as J, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Read};
use ureq::Agent;
use ureq::tls::{PemItem, RootCerts, TlsConfig};

pub const COMMANDS: &[&str] = &[
    "signup",
    "login",
    "whoami",
    "account",
    "password",
    "token",
    "org",
    "projects",
    "project",
    "histories",
    "blame",
    "checks",
    "proposal",
    "issue",
    "release",
    "mirror",
    "events",
];

pub const USAGE: &str = "Forge commands (the forge is the host of --store; --store https://<host>/p/<owner>/<project>
or --project <owner>/<project> names the project; --json prints the API's answer):
  signup <name> [--email <e>]      make an account (password on stdin)
  login <name>                     sign in (password on stdin); writes the token to
                                   --token-file, or prints it
  whoami | account <name>          an account: kind, organizations, members
  account edit [--display <d>] [--email <e>]
  password                         change it (old and new password on stdin, a line each)
  token list | token revoke <key>
  token make <label> [--scope all|read|project:<owner>/<project>] [--days <n>]
  org create <name> | org member <org> <user> owner|member|remove
  projects                         the projects you can read
  project create <name> [--owner <o>] [--description <d>] [--public]
  project show | project delete --yes | project fork [--owner <o>] [--name <n>]
  project edit [--description <d>] [--visibility public|private] [--readme <path>]
  project member <who> read|write|maintain|admin|remove
  project protect <history> on|off
  project gate <history> [--approvals <n>] [--checks <a,b>] [--resolved]
  project label <name> [--color <c>] [--set <s>] [--rule one|any] | project label <name> remove
  project webhook list | project webhook add <url> [--secret-file <f>] [--events <k,..>]
  project webhook remove <id>
  project runner list | project runner <account> add|remove
  histories                        the project's histories and their tips
  blame <path>                     each line with the node that last changed it (-H, -n)
  checks                           check results for a node (-H, -n)
  proposal list [--state open|merged|closed|all]
  proposal open <title> --source <history> [--target <history>]
  proposal show <n> | proposal edit <n> --title <t>
  proposal close|reopen|approve|merge <n>
  proposal thread <n> <path>:<line> [--old] <text>|-   a review thread on a line of the diff
                                   (--old: the line's old side) [--suggestion <text>]
  proposal reply <n> <thread> <text>|- [--suggestion <text>]
  proposal resolve|promote <n> <thread> [--undo]    promote: the thread's note becomes the
                                   merged step's note on its line (author only)
  issue list [--state open|closed|all] [--label <l>]
  issue open <title> [--body <text>|-] [--label <l>]... [--at <path>:<line>] [--suggestion <text>]
  issue show <n> | issue comment <n> <text>|-
  issue edit <n> [--title <t>] [--body <text>|-] [--assign <a,b>]
  issue close|reopen <n> | issue label <n> +<add> -<remove> ...
  release list | release publish <tag> --changelog <file> [--compatible] [-n <node>]
  release archive <tag> <file.tar.gz>
  mirror add <name> <git url> | mirror pull <name>
  events [--after <n>] [--follow]   the event log; --follow waits for new events";

const FLAGS: &[&str] = &["--public", "--resolved", "--undo", "--yes", "--follow", "--json", "--old"];

/// A command's own words: positionals, `--name value` options (several
/// times for a list) and flags.
struct Opts {
    pos: Vec<String>,
    vals: BTreeMap<String, Vec<String>>,
    flags: BTreeSet<String>,
}

impl Opts {
    fn parse(words: &[String]) -> Res<Opts> {
        let mut o = Opts { pos: Vec::new(), vals: BTreeMap::new(), flags: BTreeSet::new() };
        let mut it = words.iter();
        while let Some(w) = it.next() {
            if FLAGS.contains(&w.as_str()) {
                o.flags.insert(w.clone());
            } else if w.starts_with("--") && w.len() > 2 {
                let v = it.next().ok_or(format!("{w} needs a value"))?;
                o.vals.entry(w.clone()).or_default().push(v.clone());
            } else {
                o.pos.push(w.clone());
            }
        }
        Ok(o)
    }
    fn get(&self, k: &str) -> Option<&str> {
        self.vals.get(k).and_then(|v| v.last()).map(String::as_str)
    }
    fn all(&self, k: &str) -> Vec<String> {
        self.vals.get(k).cloned().unwrap_or_default()
    }
    fn flag(&self, k: &str) -> bool {
        self.flags.contains(k)
    }
    fn arg(&self, i: usize, what: &str) -> Res<&str> {
        self.pos.get(i).map(String::as_str).ok_or(format!("missing {what}\n{USAGE}"))
    }
    /// Positionals from `i` on, as one text; `-` reads stdin.
    fn text(&self, i: usize, what: &str) -> Res<String> {
        if self.pos.len() <= i {
            return Err(format!("missing {what}\n{USAGE}"));
        }
        text_arg(&self.pos[i..].join(" "))
    }
}

/// `-` is stdin; anything else is the text itself.
fn text_arg(s: &str) -> Res<String> {
    if s == "-" {
        let mut t = String::new();
        std::io::stdin().read_to_string(&mut t).map_err(e)?;
        Ok(t)
    } else {
        Ok(s.to_string())
    }
}

/// The forge's API: base URL, token, and the project named.
struct Api {
    agent: Agent,
    base: String,
    project: Option<String>,
    token: Option<String>,
}

/// Percent-encode a path segment or a query value (everything but the
/// unreserved characters; `/` kept when `slash`).
fn enc(s: &str, slash: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) || (slash && b == b'/') || b == b':' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn query(pairs: &[(&str, &str)]) -> String {
    let q: Vec<String> =
        pairs.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| format!("{k}={}", enc(v, false))).collect();
    if q.is_empty() { String::new() } else { format!("?{}", q.join("&")) }
}

impl Api {
    fn new(store: &str, project: Option<&str>, token: Option<String>, ca: Option<&[u8]>) -> Res<Api> {
        if !store.starts_with("https://") {
            return Err(format!("forge commands need --store https://<host>[/p/<owner>/<project>], not {store:?}"));
        }
        let store = store.trim_end_matches('/');
        let (base, from_url) = match store.split_once("/p/") {
            Some((b, p)) => (b.to_string(), Some(p.to_string())),
            None => (store.to_string(), None),
        };
        let roots = match ca {
            Some(pem) => {
                let mut certs = Vec::new();
                for item in ureq::tls::parse_pem(pem) {
                    if let PemItem::Certificate(c) = item.map_err(e)? {
                        certs.push(c);
                    }
                }
                if certs.is_empty() {
                    return Err("--ca: no certificate in the file".into());
                }
                RootCerts::Specific(std::sync::Arc::new(certs))
            }
            None => RootCerts::WebPki,
        };
        let agent: Agent = Agent::config_builder()
            .http_status_as_error(false)
            .https_only(true)
            .tls_config(TlsConfig::builder().root_certs(roots).build())
            .build()
            .into();
        Ok(Api { agent, base, project: project.map(String::from).or(from_url), token })
    }

    fn project(&self) -> Res<String> {
        let p = self.project.clone().ok_or(
            "which project? --store https://<host>/p/<owner>/<project>, or --project <owner>/<project>".to_string(),
        )?;
        if p.split('/').count() != 2 {
            return Err(format!("{p:?}: a project is <owner>/<project>"));
        }
        Ok(p)
    }

    /// `projects/<owner>/<project>/<rest>`.
    fn p(&self, rest: &str) -> Res<String> {
        let p = self.project()?;
        Ok(if rest.is_empty() { format!("projects/{p}") } else { format!("projects/{p}/{rest}") })
    }

    fn send(&self, method: &str, path: &str, body: Option<&J>) -> Res<ureq::http::Response<ureq::Body>> {
        let url = format!("{}/api/v1/{path}", self.base);
        let auth = self.token.as_ref().map(|t| format!("Bearer {t}"));
        let r = match method {
            "GET" | "DELETE" => {
                let mut r = if method == "GET" { self.agent.get(&url) } else { self.agent.delete(&url) };
                if let Some(a) = &auth {
                    r = r.header("Authorization", a);
                }
                r.call()
            }
            _ => {
                let mut r = match method {
                    "POST" => self.agent.post(&url),
                    "PUT" => self.agent.put(&url),
                    _ => self.agent.patch(&url),
                };
                if let Some(a) = &auth {
                    r = r.header("Authorization", a);
                }
                let bytes = serde_json::to_vec(body.unwrap_or(&json!({}))).map_err(e)?;
                r.header("Content-Type", "application/json").send(&bytes[..])
            }
        };
        r.map_err(|x| format!("{}: {x}", self.base))
    }

    fn call(&self, method: &str, path: &str, body: Option<J>) -> Res<J> {
        let mut resp = self.send(method, path, body.as_ref())?;
        let status = resp.status();
        let text = resp.body_mut().with_config().limit(1 << 30).read_to_string().map_err(e)?;
        let j: J = serde_json::from_str(&text).unwrap_or(J::String(text));
        if !status.is_success() {
            let m = j["message"].as_str().map(String::from).unwrap_or_else(|| j.to_string());
            return Err(format!("{} {m}", status.as_u16()));
        }
        Ok(j)
    }
    fn get(&self, path: &str) -> Res<J> {
        self.call("GET", path, None)
    }
}

fn s(j: &J, k: &str) -> String {
    match &j[k] {
        J::String(x) => x.clone(),
        J::Null => String::new(),
        x => x.to_string(),
    }
}

fn arr(j: &J) -> Vec<J> {
    j.as_array().cloned().unwrap_or_default()
}

/// The first part of a node id's or view's key, as the web shows it.
fn short(id: &str) -> String {
    let last = id.rsplit(' ').next().unwrap_or(id);
    let hex = last.split_once(':').map_or(last, |(_, h)| h);
    hex.chars().take(12).collect()
}

fn when(t: &str) -> String {
    let Ok(secs) = t.parse::<i64>() else { return t.to_string() };
    // Civil date from days since 1970 (Howard Hinnant's algorithm), UTC.
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, rem % 3600 / 60)
}

fn keys(j: &J) -> Vec<String> {
    j.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default()
}

fn first_line(t: &str) -> &str {
    t.lines().next().unwrap_or("")
}

/// Read the token file, when there is one.
fn read_token(a: &Args) -> Res<Option<String>> {
    match &a.token_file {
        Some(f) => Ok(Some(std::fs::read_to_string(f).map_err(|x| format!("{f}: {x}"))?.trim().to_string())),
        None => Ok(None),
    }
}

fn stdin_lines(n: usize) -> Res<Vec<String>> {
    let mut out = Vec::new();
    for l in std::io::stdin().lock().lines().take(n) {
        out.push(l.map_err(e)?.trim_end_matches('\r').to_string());
    }
    if out.len() < n {
        return Err(format!("expected {n} line(s) on stdin"));
    }
    Ok(out)
}

fn write_secret(path: &str, text: &str) -> Res<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path).map_err(|x| format!("{path}: {x}"))?;
    f.write_all(text.as_bytes()).map_err(e)?;
    f.write_all(b"\n").map_err(e)
}

/// Run a forge command; `words` start with its name.
pub fn run(a: &Args, words: &[String], ca: Option<&[u8]>) -> Res<()> {
    let cmd = words[0].as_str();
    let o = Opts::parse(&words[1..])?;
    let token = if cmd == "login" || cmd == "signup" { read_token(a).ok().flatten() } else { read_token(a)? };
    let api = Api::new(&a.store, o.get("--project"), token, ca)?;
    let json_out = o.flag("--json");
    // Print the API's answer as it is with --json, else through `human`.
    let show = |j: J, human: &dyn Fn(&J)| {
        if json_out {
            out!("{}", serde_json::to_string_pretty(&j).unwrap_or_default());
        } else {
            human(&j);
        }
    };
    let ok = |j: J| show(j, &|_| {});
    let history = || if a.history == "main" { "h/dev".to_string() } else { a.history.clone() };
    let node = || if a.node == "latest" { String::new() } else { a.node.clone() };
    let sub = o.pos.first().map(String::as_str).unwrap_or("");

    match cmd {
        "signup" => {
            let name = o.arg(0, "<name>")?;
            let pw = stdin_lines(1)?.remove(0);
            let mut b = json!({ "name": name, "password": pw });
            if let Some(m) = o.get("--email") {
                b["email"] = json!(m);
            }
            api.call("POST", "users", Some(b))?;
            out!("made user {name}");
        }
        "login" => {
            let name = o.arg(0, "<name>")?;
            let pw = stdin_lines(1)?.remove(0);
            let j = api.call("POST", "login", Some(json!({ "name": name, "password": pw })))?;
            let t = s(&j, "token");
            match &a.token_file {
                Some(f) => {
                    write_secret(f, &t)?;
                    out!("signed in as {name}; token in {f}");
                }
                None => out!("{t}"),
            }
        }
        "whoami" => show(api.get("user")?, &print_account),
        "account" if sub == "edit" => {
            let mut b = json!({});
            for k in ["display", "email"] {
                if let Some(v) = o.get(&format!("--{k}")) {
                    b[k] = json!(v);
                }
            }
            ok(api.call("PATCH", "user", Some(b))?);
        }
        "account" => show(api.get(&format!("users/{}", enc(o.arg(0, "<name>")?, false)))?, &print_account),
        "password" => {
            let l = stdin_lines(2)?;
            api.call("POST", "user/password", Some(json!({ "old": l[0], "new": l[1] })))?;
            out!("password changed");
        }
        "token" => match sub {
            "list" | "" => show(api.get("user/tokens")?, &|j| {
                for t in arr(j) {
                    let exp = s(&t, "expires");
                    out!(
                        "{}  {}  {}  made {}{}",
                        s(&t, "key"),
                        s(&t, "label"),
                        s(&t, "scope"),
                        when(&s(&t, "created")),
                        if exp.is_empty() { String::new() } else { format!("  expires {}", when(&exp)) }
                    );
                }
            }),
            "make" => {
                let label = o.arg(1, "<label>")?;
                let scope = o.get("--scope").unwrap_or("all").replacen("project:", "project ", 1);
                let mut b = json!({ "label": label, "scope": scope });
                if let Some(d) = o.get("--days") {
                    b["days"] = json!(d);
                }
                let j = api.call("POST", "user/tokens", Some(b))?;
                match &a.token_file {
                    // The new token goes to --token-file only when asked with --out.
                    _ if o.get("--out").is_some() => {
                        let f = o.get("--out").unwrap();
                        write_secret(f, &s(&j, "token"))?;
                        out!("token {} written to {f}", s(&j, "key"));
                    }
                    _ => show(j, &|j| out!("{}\n(key {}; shown once)", s(j, "token"), s(j, "key"))),
                }
            }
            "revoke" => {
                api.call("DELETE", &format!("user/tokens/{}", enc(o.arg(1, "<key>")?, false)), None)?;
                out!("revoked");
            }
            x => return Err(format!("token {x}? list, make or revoke")),
        },
        "org" => match sub {
            "create" => {
                let n = o.arg(1, "<name>")?;
                api.call("POST", "orgs", Some(json!({ "name": n })))?;
                out!("made organization {n}");
            }
            "member" => {
                let (org, user, role) = (o.arg(1, "<org>")?, o.arg(2, "<user>")?, o.arg(3, "owner|member|remove")?);
                let path = format!("orgs/{}/members/{}", enc(org, false), enc(user, false));
                match role {
                    "remove" => api.call("DELETE", &path, None)?,
                    "owner" | "member" => api.call("PUT", &path, Some(json!({ "role": role })))?,
                    r => return Err(format!("{r:?}: owner, member or remove")),
                };
                out!("done");
            }
            x => return Err(format!("org {x}? create or member")),
        },
        "projects" => show(api.get("projects")?, &|j| {
            for p in arr(j) {
                out!(
                    "{}/{}  {}  {}  {}",
                    s(&p, "owner"),
                    s(&p, "name"),
                    s(&p, "visibility"),
                    s(&p, "role"),
                    s(&p, "description")
                );
            }
        }),
        "project" => project(&api, &o, sub, &show)?,
        "histories" => show(api.get(&api.p("histories")?)?, &|j| {
            for h in arr(j) {
                let tips: Vec<String> = arr(&h["tips"]).iter().map(|t| short(t.as_str().unwrap_or(""))).collect();
                out!("{}  {}", s(&h, "name"), tips.join(" "));
            }
        }),
        "blame" => {
            let path = o.arg(0, "<path>")?;
            let q = query(&[("history", &history()), ("node", &node()), ("path", path)]);
            show(api.get(&api.p(&format!("blame{q}"))?)?, &|j| {
                for (i, l) in arr(&j["lines"]).iter().enumerate() {
                    // `line` is what the node's step says, in one line.
                    let what: String = s(l, "line").chars().take(24).collect();
                    out!("{}  {what:24}  {:>5}  {}", short(&s(l, "node")), i + 1, s(l, "text"));
                }
            });
        }
        "checks" => {
            let q = query(&[("history", &history()), ("node", &node())]);
            show(api.get(&api.p(&format!("checks{q}"))?)?, &|j| {
                for c in arr(j) {
                    out!("{}  {}  {}  {}", s(&c, "name"), s(&c, "status"), s(&c, "runner"), when(&s(&c, "time")));
                }
            });
        }
        "proposal" => proposal(&api, &o, sub, &show)?,
        "issue" => issue(&api, &o, sub, &show)?,
        "release" => match sub {
            "list" | "" => show(api.get(&api.p("releases")?)?, &|j| {
                for r in arr(j) {
                    let tags: Vec<String> =
                        arr(&r["tags"]).iter().filter_map(|t| t.as_str().map(String::from)).collect();
                    let compat = if r["compatible"].as_bool() == Some(true) { "compatible" } else { "breaking" };
                    out!(
                        "{}  {}  {}  {}",
                        tags.join(","),
                        short(&s(&r, "id")),
                        compat,
                        first_line(&s(&r, "changelog"))
                    );
                }
            }),
            "publish" => {
                let tag = o.arg(1, "<tag>")?;
                let f = a.changelog.as_ref().ok_or("release publish needs --changelog <file>")?;
                let changelog = std::fs::read_to_string(f).map_err(|x| format!("{f}: {x}"))?;
                let b = json!({ "tag": tag, "changelog": changelog, "compatible": a.compatible, "node": node() });
                let j = api.call("POST", &api.p("releases")?, Some(b))?;
                show(j, &|j| out!("released {tag} as {}", short(&s(j, "id"))));
            }
            "archive" => {
                let (tag, file) = (o.arg(1, "<tag>")?, o.arg(2, "<file.tar.gz>")?);
                let mut resp =
                    api.send("GET", &api.p(&format!("releases/{}/archive.tar.gz", enc(tag, false)))?, None)?;
                let bytes = resp.body_mut().with_config().limit(u64::MAX).read_to_vec().map_err(e)?;
                if !resp.status().is_success() {
                    return Err(format!("{} {}", resp.status().as_u16(), String::from_utf8_lossy(&bytes)));
                }
                std::fs::write(file, &bytes).map_err(|x| format!("{file}: {x}"))?;
                out!("wrote {file} ({} bytes)", bytes.len());
            }
            x => return Err(format!("release {x}? list, publish or archive")),
        },
        "mirror" => match sub {
            "add" => {
                let (n, url) = (o.arg(1, "<name>")?, o.arg(2, "<git url>")?);
                api.call("POST", &api.p("mirrors")?, Some(json!({ "name": n, "url": url })))?;
                out!("mirror {n} follows {url}; mirror pull {n} imports it");
            }
            "pull" => {
                let n = o.arg(1, "<name>")?;
                let j = api.call("POST", &api.p(&format!("mirrors/{}/pull", enc(n, false)))?, None)?;
                show(j, &|j| out!("{} commits; moved {}", s(j, "commits"), s(j, "moved")));
            }
            x => return Err(format!("mirror {x}? add or pull")),
        },
        "events" => {
            if o.flag("--follow") {
                // From now on, unless --after says where to start.
                return follow(&api, o.get("--after").unwrap_or(""), json_out);
            }
            let after = o.get("--after").unwrap_or("0").to_string();
            show(api.get(&format!("events{}", query(&[("after", &after)])))?, &|j| {
                for ev in arr(&j["events"]) {
                    print_event(&ev);
                }
            });
        }
        _ => return Err(format!("unknown command {cmd}\n{USAGE}")),
    }
    Ok(())
}

fn print_account(j: &J) {
    out!("{}  {}{}", s(j, "name"), s(j, "kind"), if j["admin"].as_bool() == Some(true) { "  admin" } else { "" });
    for k in ["display", "email"] {
        let v = s(j, k);
        if !v.is_empty() {
            out!("{k}: {v}");
        }
    }
    if let Some(m) = j["members"].as_object() {
        for (u, r) in m {
            out!("member {u}  {}", r.as_str().unwrap_or(""));
        }
    }
    if let Some(m) = j["orgs"].as_object() {
        for (u, r) in m {
            out!("in {u}  {}", r.as_str().unwrap_or(""));
        }
    } else if let Some(m) = j["orgs"].as_array() {
        for x in m {
            out!("in {}", x.as_str().map(String::from).unwrap_or_else(|| x.to_string()));
        }
    }
}

fn print_event(ev: &J) {
    let mut rest = Vec::new();
    if let Some(o) = ev.as_object() {
        for (k, v) in o {
            if !matches!(k.as_str(), "number" | "time" | "kind" | "actor" | "project") {
                rest.push(format!("{k}={}", v.as_str().map(String::from).unwrap_or_else(|| v.to_string())));
            }
        }
    }
    out!(
        "{}  {}  {}  {}  {}  {}",
        s(ev, "number"),
        when(&s(ev, "time")),
        s(ev, "kind"),
        s(ev, "actor"),
        s(ev, "project"),
        rest.join(" ")
    );
}

/// Server-sent events until the connection ends.
fn follow(api: &Api, after: &str, json_out: bool) -> Res<()> {
    let url = format!("{}/api/v1/events{}", api.base, query(&[("after", after)]));
    let mut r = api.agent.get(&url).header("Accept", "text/event-stream");
    if let Some(t) = &api.token {
        r = r.header("Authorization", &format!("Bearer {t}"));
    }
    let mut resp = r.call().map_err(e)?;
    if !resp.status().is_success() {
        return Err(format!("events: {}", resp.status().as_u16()));
    }
    let reader = std::io::BufReader::new(resp.body_mut().with_config().limit(u64::MAX).reader());
    for line in reader.lines() {
        let line = line.map_err(e)?;
        if let Some(data) = line.strip_prefix("data: ") {
            let ev: J = serde_json::from_str(data).unwrap_or(J::Null);
            if json_out {
                out!("{data}");
            } else {
                print_event(&ev);
            }
            super::flush_out();
        }
    }
    Ok(())
}

type Show<'a> = dyn Fn(J, &dyn Fn(&J)) + 'a;

fn project(api: &Api, o: &Opts, sub: &str, show: &Show) -> Res<()> {
    let done = |what: &str| out!("{what}");
    match sub {
        "create" => {
            let name = o.arg(1, "<name>")?;
            let mut b = json!({ "name": name, "public": o.flag("--public") });
            if let Some(x) = o.get("--owner") {
                b["owner"] = json!(x);
            }
            if let Some(x) = o.get("--description") {
                b["description"] = json!(x);
            }
            let j = api.call("POST", "projects", Some(b))?;
            show(j, &|j| out!("made {}", s(j, "project")));
        }
        "show" | "" => show(api.get(&api.p("")?)?, &print_project),
        "edit" => {
            let mut b = json!({});
            for k in ["description", "visibility", "readme"] {
                if let Some(v) = o.get(&format!("--{k}")) {
                    b[k] = json!(v);
                }
            }
            api.call("PATCH", &api.p("")?, Some(b))?;
            done("saved");
        }
        "delete" => {
            if !o.flag("--yes") {
                return Err(format!("this deletes {} and everything in it; add --yes", api.project()?));
            }
            api.call("DELETE", &api.p("")?, None)?;
            done("deleted");
        }
        "fork" => {
            let mut b = json!({});
            for k in ["owner", "name"] {
                if let Some(v) = o.get(&format!("--{k}")) {
                    b[k] = json!(v);
                }
            }
            let j = api.call("POST", &api.p("fork")?, Some(b))?;
            show(j, &|j| out!("forked to {}", s(j, "project")));
        }
        "member" => {
            let (who, role) = (o.arg(1, "<who>")?, o.arg(2, "<role>|remove")?);
            let path = api.p(&format!("members/{}", enc(who, false)))?;
            if role == "remove" {
                api.call("DELETE", &path, None)?;
            } else {
                api.call("PUT", &path, Some(json!({ "role": role })))?;
            }
            done("done");
        }
        "protect" => {
            let (h, on) = (o.arg(1, "<history>")?, o.arg(2, "on|off")?);
            if !matches!(on, "on" | "off") {
                return Err("protect <history> on|off".into());
            }
            api.call("PUT", &api.p(&format!("protected/{}", enc(h, true)))?, Some(json!({ "on": on == "on" })))?;
            done(&format!("{h} {}", if on == "on" { "protected" } else { "unprotected" }));
        }
        "gate" => {
            let h = o.arg(1, "<history>")?;
            let checks: Vec<String> = o
                .get("--checks")
                .unwrap_or("")
                .split(',')
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(String::from)
                .collect();
            let b = json!({
                "approvals": o.get("--approvals").unwrap_or("0"), "checks": checks, "resolved": o.flag("--resolved"),
            });
            api.call("PUT", &api.p(&format!("gates/{}", enc(h, true)))?, Some(b))?;
            done(&format!("gates of {h} set"));
        }
        "label" => {
            let name = o.arg(1, "<name>")?;
            let path = api.p(&format!("labels/{}", enc(name, false)))?;
            if o.pos.get(2).map(String::as_str) == Some("remove") {
                api.call("DELETE", &path, None)?;
                done("removed");
            } else {
                let mut b = json!({});
                for k in ["color", "set", "rule"] {
                    if let Some(v) = o.get(&format!("--{k}")) {
                        b[k] = json!(v);
                    }
                }
                api.call("PUT", &path, Some(b))?;
                done(&format!("label {name} set"));
            }
        }
        "webhook" => match o.pos.get(1).map(String::as_str).unwrap_or("list") {
            "list" => show(api.get(&api.p("")?)?["webhooks"].clone(), &|j| {
                if let Some(m) = j.as_object() {
                    for (id, h) in m {
                        out!("{id}  {}  {}", s(h, "url"), s(h, "events"));
                    }
                }
            }),
            "add" => {
                let url = o.arg(2, "<url>")?;
                let secret = match o.get("--secret-file") {
                    Some(f) => std::fs::read_to_string(f).map_err(|x| format!("{f}: {x}"))?.trim().to_string(),
                    None => String::new(),
                };
                let b = json!({ "url": url, "secret": secret, "events": o.get("--events").unwrap_or("*") });
                show(api.call("POST", &api.p("webhooks")?, Some(b))?, &|j| out!("webhook {}", s(j, "id")));
            }
            "remove" => {
                api.call("DELETE", &api.p(&format!("webhooks/{}", enc(o.arg(2, "<id>")?, false)))?, None)?;
                done("removed");
            }
            x => return Err(format!("webhook {x}? list, add or remove")),
        },
        "runner" => match o.pos.get(1).map(String::as_str).unwrap_or("list") {
            "list" => show(api.get(&api.p("")?)?["runners"].clone(), &|j| {
                for r in keys(j) {
                    out!("{r}");
                }
            }),
            who => {
                let path = api.p(&format!("runners/{}", enc(who, false)))?;
                match o.arg(2, "add|remove")? {
                    "add" => api.call("PUT", &path, None)?,
                    "remove" => api.call("DELETE", &path, None)?,
                    x => return Err(format!("runner {who} {x}? add or remove")),
                };
                done("done");
            }
        },
        x => return Err(format!("project {x}?\n{USAGE}")),
    }
    Ok(())
}

fn print_project(j: &J) {
    out!("{}/{}  {}  your role: {}", s(j, "owner"), s(j, "name"), s(j, "visibility"), s(j, "role"));
    let d = s(j, "description");
    if !d.is_empty() {
        out!("{d}");
    }
    for h in arr(&j["histories"]) {
        let tips: Vec<String> = arr(&h["tips"]).iter().map(|t| short(t.as_str().unwrap_or(""))).collect();
        out!("history {}  {}", s(&h, "name"), tips.join(" "));
    }
    for (k, label) in [("members", "member"), ("runners", "runner")] {
        if let Some(m) = j[k].as_object() {
            for (u, r) in m {
                out!("{label} {u}  {}", r.as_str().unwrap_or(""));
            }
        }
    }
    for h in keys(&j["protected"]) {
        out!("protected {h}");
    }
    if let Some(g) = j["gates"].as_object() {
        for (h, x) in g {
            out!(
                "gate {h}: {} approvals; checks {}{}",
                s(x, "approvals"),
                keys(&x["checks"]).join(","),
                if s(x, "resolved") == "true" { "; threads resolved" } else { "" }
            );
        }
    }
    if let Some(l) = j["labels"].as_object() {
        let names: Vec<String> = l
            .iter()
            .map(|(n, x)| {
                let set = s(x, "set");
                if set.is_empty() { n.clone() } else { format!("{n} ({set})") }
            })
            .collect();
        out!("labels {}", names.join(", "));
    }
    if let Some(r) = arr(&j["releases"]).first() {
        let tags: Vec<String> = arr(&r["tags"]).iter().map(|t| t.as_str().unwrap_or("").to_string()).collect();
        out!("latest release {}", tags.join(","));
    }
}

fn proposal(api: &Api, o: &Opts, sub: &str, show: &Show) -> Res<()> {
    let n = || o.arg(1, "<n>");
    let at = |rest: &str| -> Res<String> { api.p(&format!("proposals/{}{rest}", n()?)) };
    match sub {
        "list" | "" => {
            let st = o.get("--state").unwrap_or("open");
            let q = query(&[("state", if st == "all" { "" } else { st })]);
            show(api.get(&api.p(&format!("proposals{q}"))?)?, &|j| {
                for p in arr(j) {
                    out!(
                        "#{}  {}  {}  {} -> {}  {}  {}",
                        s(&p, "number"),
                        s(&p, "state"),
                        s(&p, "title"),
                        s(&p, "source"),
                        s(&p, "target"),
                        s(&p, "author"),
                        when(&s(&p, "updated"))
                    );
                }
            });
        }
        "open" => {
            let title = o.text(1, "<title>")?;
            let source = o.get("--source").ok_or("proposal open needs --source <history>")?;
            let mut b = json!({ "title": title, "source": source });
            if let Some(t) = o.get("--target") {
                b["target"] = json!(t);
            }
            show(api.call("POST", &api.p("proposals")?, Some(b))?, &|j| out!("opened #{}", s(j, "number")));
        }
        "show" => show(api.get(&at("")?)?, &print_proposal),
        "edit" => {
            let t = o.get("--title").ok_or("proposal edit needs --title <t>")?;
            api.call("PATCH", &at("")?, Some(json!({ "title": t })))?;
            out!("retitled");
        }
        "close" | "reopen" => {
            let state = if sub == "close" { "closed" } else { "open" };
            api.call("PATCH", &at("")?, Some(json!({ "state": state })))?;
            out!("{state}");
        }
        "approve" => {
            api.call("POST", &at("/approve")?, None)?;
            out!("approved");
        }
        "merge" => show(api.call("POST", &at("/merge")?, None)?, &|j| out!("merged; tip {}", short(&s(j, "tip")))),
        "thread" => {
            let spot = o.arg(2, "<path>:<line>")?;
            let (path, line) = spot.rsplit_once(':').ok_or("a thread's place is <path>:<line>")?;
            let line: u64 = line.parse().map_err(|_| format!("{line:?} is not a line number"))?;
            let text = o.text(3, "<text>")?;
            let side = if o.flag("--old") { "old" } else { "new" };
            let mut b = json!({ "path": path, "line": line, "side": side, "text": text });
            if let Some(sg) = o.get("--suggestion") {
                b["suggestion"] = json!(text_arg(sg)?);
            }
            // The forge keeps the line's text with the thread, so it follows the line.
            let j = api.call("POST", &at("/threads")?, Some(b))?;
            let id = s(&j, "id");
            show(j, &|_| out!("thread {id} on {path}:{line}"));
        }
        "reply" => {
            let id = o.arg(2, "<thread>")?;
            let mut b = json!({ "text": o.text(3, "<text>")? });
            if let Some(sg) = o.get("--suggestion") {
                b["suggestion"] = json!(text_arg(sg)?);
            }
            api.call("POST", &at(&format!("/threads/{}/comments", enc(id, false)))?, Some(b))?;
            out!("replied");
        }
        "resolve" | "promote" => {
            let id = o.arg(2, "<thread>")?;
            let b = json!({ sub: !o.flag("--undo") });
            let b = if sub == "resolve" { json!({ "resolved": b[sub] }) } else { b };
            api.call("POST", &at(&format!("/threads/{}/{sub}", enc(id, false)))?, Some(b))?;
            out!("{}", if o.flag("--undo") { format!("{sub} undone") } else { format!("{sub}d") });
        }
        x => return Err(format!("proposal {x}?\n{USAGE}")),
    }
    Ok(())
}

fn print_proposal(j: &J) {
    out!("#{} {}  [{}]", s(j, "number"), s(j, "title"), s(j, "state"));
    out!(
        "{} proposes {} -> {}  revision {}  {}",
        s(j, "author"),
        s(j, "source"),
        s(j, "target"),
        arr(&j["revisions"]).len(),
        when(&s(j, "updated"))
    );
    let g = &j["gates"];
    let checks: Vec<String> =
        arr(&g["checks"]).iter().map(|c| format!("{}:{}", s(c, "name"), s(c, "status"))).collect();
    out!(
        "gates {}: approvals {}/{}{}{}",
        if g["ok"].as_bool() == Some(true) { "hold" } else { "not yet" },
        s(g, "approvals"),
        s(g, "approvals_needed"),
        if checks.is_empty() { String::new() } else { format!("; checks {}", checks.join(" ")) },
        if g["resolved_needed"].as_bool() == Some(true) {
            format!("; {} unresolved", s(g, "unresolved"))
        } else {
            String::new()
        }
    );
    let merged = s(j, "merged");
    if !merged.is_empty() {
        out!("merged as {}", short(&merged));
    }
    let im = &j["imported"];
    if im.is_object() {
        out!("from {}", s(im, "from"));
        let body = s(im, "body");
        if !body.is_empty() {
            out!("{body}");
        }
        for c in arr(&im["comments"]) {
            out!("  {} {}: {}", s(&c, "author"), when(&s(&c, "time")), s(&c, "text").replace('\n', "\n    "));
        }
        for r in arr(&im["reviews"]) {
            out!("  review by {} ({}) {}: {}", s(&r, "author"), s(&r, "state"), when(&s(&r, "time")), s(&r, "text"));
            for c in arr(&r["comments"]) {
                out!("    {}:{} {}", s(&c, "path"), s(&c, "line"), s(&c, "text"));
            }
        }
    }
    out!("");
    for l in arr(&j["diff"]["lines"]) {
        out!("{}", l.as_str().map(String::from).unwrap_or_else(|| s(&l, "text")));
    }
    for t in arr(&j["threads"]) {
        out!("");
        let mut tags = Vec::new();
        if t["outdated"].as_bool() == Some(true) {
            tags.push("outdated");
        }
        if s(&t, "resolved") == "true" {
            tags.push("resolved");
        }
        if s(&t, "promote") == "true" {
            tags.push("becomes a step note");
        }
        out!(
            "thread {}  {}:{} ({})  {}  {}",
            s(&t, "id"),
            s(&t, "path"),
            s(&t, "line_now"),
            s(&t, "side"),
            tags.join(", "),
            s(&t, "text")
        );
        for c in arr(&t["comments"]) {
            out!("  {} {}: {}", s(&c, "author"), when(&s(&c, "time")), s(&c, "text").replace('\n', "\n    "));
            let sg = s(&c, "suggestion");
            if !sg.is_empty() {
                out!("    suggests: {sg}");
            }
        }
    }
}

fn issue(api: &Api, o: &Opts, sub: &str, show: &Show) -> Res<()> {
    let n = || o.arg(1, "<n>");
    let at = |rest: &str| -> Res<String> { api.p(&format!("issues/{}{rest}", n()?)) };
    match sub {
        "list" | "" => {
            let st = o.get("--state").unwrap_or("open");
            let q = query(&[("state", if st == "all" { "" } else { st }), ("label", o.get("--label").unwrap_or(""))]);
            show(api.get(&api.p(&format!("issues{q}"))?)?, &|j| {
                for i in arr(j) {
                    let labels: Vec<String> =
                        arr(&i["labels"]).iter().map(|l| l.as_str().unwrap_or("").into()).collect();
                    out!(
                        "#{}  {}  {}  [{}]  {}  {}",
                        s(&i, "number"),
                        s(&i, "state"),
                        s(&i, "title"),
                        labels.join(","),
                        s(&i, "author"),
                        when(&s(&i, "updated"))
                    );
                }
            });
        }
        "open" => {
            let title = o.text(1, "<title>")?;
            let mut b = json!({ "title": title, "labels": o.all("--label") });
            if let Some(x) = o.get("--body") {
                b["body"] = json!(text_arg(x)?);
            }
            if let Some(x) = o.get("--suggestion") {
                b["suggestion"] = json!(text_arg(x)?);
            }
            if let Some(spot) = o.get("--at") {
                let (path, line) = spot.rsplit_once(':').ok_or("--at <path>:<line>")?;
                b["anchor"] = json!({ "path": path, "line": line });
            }
            show(api.call("POST", &api.p("issues")?, Some(b))?, &|j| out!("opened #{}", s(j, "number")));
        }
        "show" => show(api.get(&at("")?)?, &print_issue),
        "comment" => {
            let t = o.text(2, "<text>")?;
            api.call("POST", &at("/comments")?, Some(json!({ "text": t })))?;
            out!("commented");
        }
        "edit" => {
            let mut b = json!({});
            if let Some(x) = o.get("--title") {
                b["title"] = json!(x);
            }
            if let Some(x) = o.get("--body") {
                b["body"] = json!(text_arg(x)?);
            }
            if let Some(x) = o.get("--assign") {
                let a: Vec<&str> = x.split(',').map(str::trim).filter(|x| !x.is_empty()).collect();
                b["assignees"] = json!(a);
            }
            api.call("PATCH", &at("")?, Some(b))?;
            out!("saved");
        }
        "close" | "reopen" => {
            let state = if sub == "close" { "closed" } else { "open" };
            api.call("PATCH", &at("")?, Some(json!({ "state": state })))?;
            out!("{state}");
        }
        "label" => {
            let (mut add, mut remove) = (Vec::new(), Vec::new());
            for w in &o.pos[2..] {
                match w.split_at_checked(1) {
                    Some(("+", l)) => add.push(l.to_string()),
                    Some(("-", l)) => remove.push(l.to_string()),
                    _ => return Err(format!("{w:?}: +<label> adds, -<label> removes")),
                }
            }
            api.call("PATCH", &at("")?, Some(json!({ "add_labels": add, "remove_labels": remove })))?;
            out!("labels changed");
        }
        x => return Err(format!("issue {x}?\n{USAGE}")),
    }
    Ok(())
}

fn print_issue(j: &J) {
    let labels: Vec<String> = arr(&j["labels"]).iter().map(|l| l.as_str().unwrap_or("").into()).collect();
    out!("#{} {}  [{}]  {}", s(j, "number"), s(j, "title"), s(j, "state"), labels.join(","));
    out!("{} opened {}", s(j, "author"), when(&s(j, "created")));
    let a: Vec<String> = arr(&j["assignees"]).iter().map(|l| l.as_str().unwrap_or("").into()).collect();
    if !a.is_empty() {
        out!("assigned to {}", a.join(", "));
    }
    if j["anchor"].is_object() {
        out!("at {}:{}", s(&j["anchor"], "path"), s(&j["anchor"], "line"));
    }
    let body = s(j, "body");
    if !body.is_empty() {
        out!("\n{body}");
    }
    let sg = s(j, "suggestion");
    if !sg.is_empty() {
        out!("\nsuggestion:\n{sg}");
    }
    for c in arr(&j["comments"]) {
        out!("\n{} {}:\n{}", s(&c, "author"), when(&s(&c, "time")), s(&c, "text"));
    }
}
