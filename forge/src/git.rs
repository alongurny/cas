//! Git over HTTPS (spec/forge/git.md): the forge's histories, seen by the
//! git client as branches, cloned, fetched and pushed with git's own
//! commits. Fetch speaks protocol v2; push speaks receive-pack (v0).

use crate::accounts::{Caller, Scope, verify_password};
use crate::gate::as_actor;
use crate::names::{ProjectId, Role};
use crate::project::RELEASE;
use crate::{Error, Forge, Result};
use cas_core::Expect;
use cas_core::server::{Request, Response};
use cas_version::View;
use cas_version::gitobj::{self, Links};
use cas_version::history::{self, History, NodeId};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

/// Where a history's links are kept: which git commit each of its nodes
/// is, in project variable `git/<history>`.
pub fn links_var(history: &str) -> String {
    format!("git/{history}")
}

const ZERO: &str = "0000000000000000000000000000000000000000";
/// The most a request body may grow to once ungzipped.
const MAX_UNZIPPED: u64 = 4 << 30;

/// Whether `path` is a git endpoint: `/<o>/<p>[.git]/<what>`, with the
/// project and what is asked.
pub fn route(path: &str) -> Option<(ProjectId, &str)> {
    let rest = path.strip_prefix('/')?;
    let mut parts = rest.splitn(3, '/');
    let (o, n, what) = (parts.next()?, parts.next()?, parts.next()?);
    if !matches!(what, "info/refs" | "git-upload-pack" | "git-receive-pack") {
        return None;
    }
    let n = n.strip_suffix(".git").unwrap_or(n);
    Some((ProjectId::new(o, n).ok()?, what))
}

// ---- pkt-lines ----

fn pkt(out: &mut Vec<u8>, data: &[u8]) {
    out.extend_from_slice(format!("{:04x}", data.len() + 4).as_bytes());
    out.extend_from_slice(data);
}

fn pkt_str(out: &mut Vec<u8>, s: &str) {
    pkt(out, s.as_bytes());
}

const FLUSH: &[u8] = b"0000";
const DELIM: &[u8] = b"0001";

/// A packet: its data, or `None` for a flush or delimiter (with which).
enum Pkt<'a> {
    Data(&'a [u8]),
    Flush,
    Delim,
}

struct Pkts<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Pkts<'a> {
    fn new(b: &'a [u8]) -> Pkts<'a> {
        Pkts { b, at: 0 }
    }
    fn rest(&self) -> &'a [u8] {
        &self.b[self.at..]
    }
    fn next(&mut self) -> Result<Option<Pkt<'a>>> {
        if self.at >= self.b.len() {
            return Ok(None);
        }
        let head = self.b.get(self.at..self.at + 4).ok_or_else(|| Error::bad("a cut packet"))?;
        let n = usize::from_str_radix(std::str::from_utf8(head).unwrap_or("x"), 16)
            .map_err(|_| Error::bad("a packet length is not hex"))?;
        self.at += 4;
        Ok(Some(match n {
            0 => Pkt::Flush,
            1 => Pkt::Delim,
            2 | 3 => return Err(Error::bad("a packet of length 2 or 3")),
            _ => {
                let d = self.b.get(self.at..self.at + n - 4).ok_or_else(|| Error::bad("a cut packet"))?;
                self.at += n - 4;
                Pkt::Data(d)
            }
        }))
    }
}

fn line(d: &[u8]) -> &str {
    std::str::from_utf8(d).unwrap_or("").trim_end_matches('\n')
}

/// The pack in side-band 1, in packets git takes.
fn sideband(out: &mut Vec<u8>, band: u8, data: &[u8]) {
    for c in data.chunks(65515) {
        let mut d = Vec::with_capacity(c.len() + 1);
        d.push(band);
        d.extend_from_slice(c);
        pkt(out, &d);
    }
}

// ---- answers ----

fn answer(status: u16, kind: &str, body: Vec<u8>) -> Response {
    Response::data(body).status(status).with("Content-Type", kind).with("Cache-Control", "no-cache")
}

fn text(status: u16, msg: &str) -> Response {
    answer(status, "text/plain; charset=utf-8", format!("{msg}\n").into_bytes())
}

fn ask_to_sign_in() -> Response {
    text(401, "sign in: your name, and a token or your password").with("WWW-Authenticate", "Basic realm=\"cas\"")
}

fn from_error(e: &Error) -> Response {
    if e.status == 401 {
        return ask_to_sign_in();
    }
    text(e.status, &e.message)
}

/// The body, ungzipped when the client gzipped it.
fn body(req: &Request) -> Result<Vec<u8>> {
    match req.header("Content-Encoding") {
        None | Some("identity") => Ok(req.body.clone()),
        Some("gzip") | Some("x-gzip") => {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(&req.body[..])
                .take(MAX_UNZIPPED)
                .read_to_end(&mut out)
                .map_err(|e| Error::bad(format!("gzip: {e}")))?;
            Ok(out)
        }
        Some(other) => Err(Error::new(415, format!("content encoding {other}"))),
    }
}

fn base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.trim().bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

impl Forge {
    /// The caller of a git request: `Basic` with the user's name and a
    /// token or their password, or whatever the API takes.
    fn git_caller(&self, req: &Request) -> Result<Option<Caller>> {
        let Some(basic) = req.header("Authorization").and_then(|a| a.strip_prefix("Basic ")) else {
            return Ok(crate::api::caller(self, req));
        };
        let raw = base64(basic).ok_or_else(|| Error::bad("Basic credentials are not base64"))?;
        let raw = String::from_utf8(raw).map_err(|_| Error::bad("Basic credentials are not UTF-8"))?;
        let (user, secret) = raw.split_once(':').unwrap_or((&raw, ""));
        if let Some(c) = self.caller_for_token(secret) {
            if !user.is_empty() && user != c.user {
                return Err(Error::new(401, "that token is someone else's"));
            }
            return Ok(Some(c));
        }
        let a = self.account(user)?.filter(|a| a.s("kind") == "user");
        if a.as_ref().and_then(|a| a.opt("password")).is_some_and(|h| verify_password(h, secret)) {
            return Ok(Some(Caller { user: user.to_string(), scope: Scope::All }));
        }
        Err(Error::new(401, "wrong name, token or password"))
    }

    /// Answer git request `what` for project `p`.
    pub fn git(&self, req: &Request, p: &ProjectId, what: &str, query: &str) -> Response {
        let r = (|| {
            let c = self.git_caller(req)?;
            let service = query.split('&').find_map(|q| q.strip_prefix("service="));
            match (req.method.as_str(), what, service) {
                ("GET", "info/refs", Some("git-upload-pack")) => {
                    self.require(c.as_ref(), p, Role::Read)?;
                    Ok(self.advertise_v2(req))
                }
                ("GET", "info/refs", Some("git-receive-pack")) => {
                    if c.is_none() {
                        return Err(Error::unauthorized());
                    }
                    self.require(c.as_ref(), p, Role::Read)?;
                    self.advertise_receive(c.as_ref(), p)
                }
                ("POST", "git-upload-pack", _) => {
                    self.require(c.as_ref(), p, Role::Read)?;
                    self.upload_pack(p, &body(req)?)
                }
                ("POST", "git-receive-pack", _) => {
                    let Some(c) = c else { return Err(Error::unauthorized()) };
                    self.require(Some(&c), p, Role::Write)?;
                    self.receive_pack(&c, p, &body(req)?)
                }
                ("GET", "info/refs", _) => {
                    Err(Error::bad("this forge speaks git's smart HTTP only (git 2.18 or newer)"))
                }
                _ => Err(Error::new(405, "not a git request")),
            }
        })();
        match r {
            Ok(r) => r,
            // Git sends credentials only when asked: a private project
            // asks anyone not signed in to sign in.
            Err(e) if e.status == 404 && !req.header("Authorization").is_some_and(|a| !a.is_empty()) => {
                ask_to_sign_in()
            }
            Err(e) => from_error(&e),
        }
    }

    fn advertise_v2(&self, req: &Request) -> Response {
        let mut out = Vec::new();
        if !req.header("Git-Protocol").is_some_and(|g| g.split(':').any(|v| v == "version=2")) {
            // Only v2 is spoken; git sends it unless told not to.
            pkt_str(&mut out, "ERR this forge speaks git protocol version 2: git config protocol.version 2\n");
            return answer(200, "application/x-git-upload-pack-advertisement", out);
        }
        for l in ["version 2\n", "agent=cas-forge\n", "ls-refs=unborn\n", "fetch\n", "object-format=sha1\n"] {
            pkt_str(&mut out, l);
        }
        out.extend_from_slice(FLUSH);
        answer(200, "application/x-git-upload-pack-advertisement", out)
    }

    // ---- refs ----

    /// History `var`'s links and the view they were read from.
    pub fn git_links(&self, p: &ProjectId, var: &str) -> Result<(Option<View>, Links)> {
        match history::head(&*self.store, &p.var(&links_var(var)))? {
            None => Ok((None, Links::new())),
            Some(v) => Ok((Some(v.clone()), gitobj::read_links(&*self.store, &v)?)),
        }
    }

    /// Save history `var`'s `links` over `old`; a concurrent change is
    /// merged in (links only grow, and a node's commit never changes).
    pub fn save_links(&self, p: &ProjectId, var: &str, old: Option<View>, links: &Links) -> Result<()> {
        let (mut old, mut links) = (old, links.clone());
        for _ in 0..8 {
            let v = gitobj::write_links(&*self.store, self.alg, &links)?;
            if old.as_ref() == Some(&v) {
                return Ok(());
            }
            let expect = old.as_ref().map_or(Expect::Absent, |o| Expect::Value(o.to_string().into_bytes()));
            match self.store.write(&p.var(&links_var(var)), v.to_string().as_bytes(), &expect) {
                Ok(_) => return Ok(()),
                Err(cas_core::Error::Conflict) => {
                    let (now, theirs) = self.git_links(p, var)?;
                    for (n, c) in theirs {
                        links.entry(n).or_insert(c);
                    }
                    old = now;
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(Error::conflict(format!("the git links of {var} kept moving")))
    }

    /// Every history of `p` git sees, dev first, with its links.
    fn linked(&self, p: &ProjectId) -> Result<Vec<Linked>> {
        let prefix = p.prefix();
        let mut names: Vec<String> = self
            .store
            .list_all(&cas_core::Span::prefix(prefix.as_bytes()))?
            .into_iter()
            .map(|v| v[prefix.len()..].to_string())
            .filter(|r| ref_of(r).is_some())
            .collect();
        names.sort_by_key(|r| r != crate::project::DEV);
        let mut out = Vec::new();
        for var in names {
            let Some((view, h)) = self.history(p, &var)? else { continue };
            let (links_view, links) = self.git_links(p, &var)?;
            let before = links.len();
            out.push(Linked { var, view, h, links, links_view, before });
        }
        Ok(out)
    }

    /// Give every node of every history a commit, sharing commits between
    /// histories where nodes are the same, and save the links that grew.
    fn link_all(&self, p: &ProjectId) -> Result<Vec<Linked>> {
        let mut all = self.linked(p)?;
        for i in 0..all.len() {
            let mut links = std::mem::take(&mut all[i].links);
            let others: Vec<gitobj::Other<'_>> =
                all.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, l)| (&*l.h, &l.links)).collect();
            gitobj::to_git_from(&*self.store, &all[i].h, &mut links, &others)?;
            all[i].links = links;
        }
        for l in &all {
            if l.links.len() != l.before {
                self.save_links(p, &l.var, l.links_view.clone(), &l.links)?;
            }
        }
        Ok(all)
    }

    /// Every ref git sees, by name, with its commit (or tag object), made
    /// from the project's histories.
    fn refs(&self, p: &ProjectId) -> Result<(BTreeMap<String, String>, Vec<Linked>)> {
        let all = self.link_all(p)?;
        let mut out = BTreeMap::new();
        let mut tags: Vec<(String, NodeId, usize)> = Vec::new();
        let mut by_version: BTreeMap<View, String> = BTreeMap::new();
        for (i, l) in all.iter().enumerate() {
            if l.var != RELEASE {
                for (n, c) in &l.links {
                    by_version.entry(n.version()).or_insert_with(|| c.clone());
                }
            }
            if let (Some(r), Some(t)) = (ref_of(&l.var), tip_of(&l.h)) {
                out.insert(r, l.links[&t].clone());
            }
            for (name, n) in &l.h.tags {
                tags.push((name.clone(), n.clone(), i));
            }
        }
        // A release's tag names the commit of a non-release history with
        // its version when there is one: the commit the work made.
        tags.sort_by_key(|(_, _, i)| all[*i].var != RELEASE);
        for (name, n, i) in tags {
            let c = by_version.get(&n.version()).or_else(|| all[i].links.get(&n)).cloned();
            if let Some(c) = c {
                out.entry(format!("refs/tags/{name}")).or_insert(c);
            }
        }
        Ok((out, all))
    }

    fn ls_refs(&self, p: &ProjectId, args: &[&str]) -> Result<Vec<u8>> {
        let (refs, _) = self.refs(p)?;
        let prefixes: Vec<&str> = args.iter().filter_map(|a| a.strip_prefix("ref-prefix ")).collect();
        let (symrefs, peel, unborn) = (args.contains(&"symrefs"), args.contains(&"peel"), args.contains(&"unborn"));
        let wanted = |r: &str| prefixes.is_empty() || prefixes.iter().any(|x| r.starts_with(x));
        let mut out = Vec::new();
        let head = "refs/heads/dev";
        if wanted("HEAD") {
            match refs.get(head) {
                Some(c) => {
                    let s = if symrefs { format!(" symref-target:{head}") } else { String::new() };
                    pkt_str(&mut out, &format!("{c} HEAD{s}\n"));
                }
                None if unborn && symrefs => pkt_str(&mut out, &format!("unborn HEAD symref-target:{head}\n")),
                None => {}
            }
        }
        for (r, c) in &refs {
            if !wanted(r) {
                continue;
            }
            let mut l = format!("{c} {r}");
            if peel
                && let Ok(("tag", body)) = gitobj::get(&*self.store, c)
                && let Some(t) = gitobj::split(&body).and_then(|(_, b)| peeled(b))
            {
                l.push_str(&format!(" peeled:{t}"));
            }
            l.push('\n');
            pkt_str(&mut out, &l);
        }
        out.extend_from_slice(FLUSH);
        Ok(out)
    }

    fn upload_pack(&self, p: &ProjectId, body: &[u8]) -> Result<Response> {
        let mut ps = Pkts::new(body);
        let mut command = String::new();
        while let Some(Pkt::Data(d)) = ps.next()? {
            if let Some(c) = line(d).strip_prefix("command=") {
                command = c.to_string();
            }
        }
        let mut args = Vec::new();
        while let Some(Pkt::Data(d)) = ps.next()? {
            args.push(line(d).to_string());
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = match command.as_str() {
            "ls-refs" => self.ls_refs(p, &args)?,
            "fetch" => self.fetch(p, &args)?,
            "" => return Err(Error::bad("no command")),
            c => return Err(Error::bad(format!("command {c} is not spoken here"))),
        };
        Ok(answer(200, "application/x-git-upload-pack-result", out))
    }

    fn fetch(&self, p: &ProjectId, args: &[&str]) -> Result<Vec<u8>> {
        if args.iter().any(|a| a.starts_with("deepen") || a.starts_with("shallow ") || a.starts_with("filter ")) {
            return Err(Error::bad("shallow and partial clones are not served; clone in full"));
        }
        let (refs, all) = self.refs(p)?;
        // What a caller may ask for: what the refs name and every commit of
        // the project's histories, never another project's objects.
        let mine: BTreeSet<&str> = refs
            .values()
            .map(String::as_str)
            .chain(all.iter().flat_map(|l| l.links.values().map(String::as_str)))
            .collect();
        let mut wants = Vec::new();
        for a in args.iter().filter_map(|a| a.strip_prefix("want ")) {
            if !mine.contains(a) {
                return Err(Error::bad(format!("not our ref {a}")));
            }
            wants.push(a.to_string());
        }
        let haves: Vec<String> = args
            .iter()
            .filter_map(|a| a.strip_prefix("have "))
            .filter(|h| mine.contains(h))
            .map(String::from)
            .collect();
        let done = args.contains(&"done");
        let mut out = Vec::new();
        if !done {
            pkt_str(&mut out, "acknowledgments\n");
            if haves.is_empty() {
                pkt_str(&mut out, "NAK\n");
            }
            for h in &haves {
                pkt_str(&mut out, &format!("ACK {h}\n"));
            }
            pkt_str(&mut out, "ready\n");
            out.extend_from_slice(DELIM);
        }
        let mut ids = gitobj::closure(&*self.store, &wants, &haves)?;
        if args.contains(&"include-tag") {
            let have: BTreeSet<String> = ids.iter().cloned().collect();
            for c in refs.values() {
                if !have.contains(c)
                    && let Ok(("tag", b)) = gitobj::get(&*self.store, c)
                    && gitobj::split(&b).and_then(|(_, b)| peeled(b)).is_some_and(|t| have.contains(&t))
                {
                    ids.push(c.clone());
                }
            }
        }
        let pack = gitobj::pack(&*self.store, &ids)?;
        pkt_str(&mut out, "packfile\n");
        sideband(&mut out, 1, &pack);
        out.extend_from_slice(FLUSH);
        Ok(out)
    }

    // ---- push ----

    fn advertise_receive(&self, _c: Option<&Caller>, p: &ProjectId) -> Result<Response> {
        let (refs, _) = self.refs(p)?;
        let mut out = Vec::new();
        pkt_str(&mut out, "# service=git-receive-pack\n");
        out.extend_from_slice(FLUSH);
        let caps = "report-status delete-refs ofs-delta quiet atomic object-format=sha1 agent=cas-forge";
        if refs.is_empty() {
            pkt_str(&mut out, &format!("{ZERO} capabilities^{{}}\0{caps}\n"));
        }
        for (i, (r, c)) in refs.iter().enumerate() {
            if i == 0 {
                pkt_str(&mut out, &format!("{c} {r}\0{caps}\n"));
            } else {
                pkt_str(&mut out, &format!("{c} {r}\n"));
            }
        }
        out.extend_from_slice(FLUSH);
        Ok(answer(200, "application/x-git-receive-pack-advertisement", out))
    }

    fn receive_pack(&self, c: &Caller, p: &ProjectId, body: &[u8]) -> Result<Response> {
        let mut ps = Pkts::new(body);
        let mut cmds: Vec<(String, String, String)> = Vec::new();
        let mut atomic = false;
        while let Some(Pkt::Data(d)) = ps.next()? {
            let (cmd, caps) = match d.iter().position(|&b| b == 0) {
                Some(i) => (&d[..i], line(&d[i + 1..])),
                None => (d, ""),
            };
            if caps.split(' ').any(|c| c == "atomic") {
                atomic = true;
            }
            let mut w = line(cmd).split(' ');
            let (Some(o), Some(n), Some(r)) = (w.next(), w.next(), w.next()) else {
                return Err(Error::bad("a push command is <old> <new> <ref>"));
            };
            cmds.push((o.into(), n.into(), r.into()));
        }
        let pack = ps.rest();
        let unpacked = if pack.is_empty() { Ok(()) } else { gitobj::unpack(&*self.store, pack).map(|_| ()) };
        let mut report = Vec::new();
        match &unpacked {
            Ok(()) => pkt_str(&mut report, "unpack ok\n"),
            Err(e) => pkt_str(&mut report, &format!("unpack {}\n", one_line(&e.to_string()))),
        }
        let results: Vec<std::result::Result<(), String>> = if unpacked.is_err() {
            cmds.iter().map(|_| Err("unpacker error".to_string())).collect()
        } else if atomic {
            // All or nothing: check every update first, then make them.
            let checks: Vec<_> = cmds.iter().map(|(o, n, r)| self.push_one(c, p, o, n, r, true)).collect();
            if checks.iter().all(Result::is_ok) {
                cmds.iter().map(|(o, n, r)| self.push_one(c, p, o, n, r, false).map_err(|e| e.message)).collect()
            } else {
                checks.into_iter().map(|r| r.map_err(|e| e.message).and(Err("atomic push failed".into()))).collect()
            }
        } else {
            cmds.iter().map(|(o, n, r)| self.push_one(c, p, o, n, r, false).map_err(|e| e.message)).collect()
        };
        for ((_, _, r), res) in cmds.iter().zip(results) {
            match res {
                Ok(()) => pkt_str(&mut report, &format!("ok {r}\n")),
                Err(m) => pkt_str(&mut report, &format!("ng {r} {}\n", one_line(&m))),
            }
        }
        report.extend_from_slice(FLUSH);
        Ok(answer(200, "application/x-git-receive-pack-result", report))
    }

    /// Move the history behind `r` from commit `old` to `new` (either may
    /// be zero), as the wire would let `c`; with `dry`, only check.
    fn push_one(&self, c: &Caller, p: &ProjectId, old: &str, new: &str, r: &str, dry: bool) -> Result<()> {
        if r.starts_with("refs/tags/") {
            return Err(Error::denied("tags are made by releases here: cas release publish"));
        }
        let var = r
            .strip_prefix("refs/heads/")
            .and_then(var_of)
            .ok_or_else(|| Error::bad(format!("{r} is not a branch here: push to dev or u/{}/<name>", c.user)))?;
        crate::names::check_history(&var)?;
        if !self.may_move(Some(c), &p.var(&var)) {
            let why = if var.starts_with("h/") && self.is_protected(p, &var) {
                format!("{var} is protected: open a proposal from u/{}/<name>", c.user)
            } else if var.starts_with("h/") {
                format!("moving {var} needs the maintain role")
            } else {
                format!("push your own histories, u/{}/<name>", c.user)
            };
            return Err(Error::denied(why));
        }
        let mut all = self.link_all(p)?;
        let at = all.iter().position(|l| l.var == var);
        let (old_view, h, lv, mut links) = match at {
            Some(i) => {
                let l = all.remove(i);
                (Some(l.view), (*l.h).clone(), l.links_view, l.links)
            }
            None => (None, History::default(), self.git_links(p, &var)?.0, Links::new()),
        };
        let current = tip_of(&h).map(|t| links[&t].clone()).unwrap_or_else(|| ZERO.to_string());
        if current != old {
            return Err(Error::conflict("stale info: fetch first"));
        }
        if new == ZERO {
            if dry {
                return Ok(());
            }
            let Some(ov) = old_view else { return Ok(()) };
            as_actor(Some(&c.user), || self.store.remove(&p.var(&var), &Expect::Value(ov.to_string().into_bytes())))?;
            if let Some(lv) = lv {
                self.store.remove(&p.var(&links_var(&var)), &Expect::Value(lv.to_string().into_bytes()))?;
            }
            return Ok(());
        }
        let mut h2 = h.clone();
        let others: Vec<gitobj::Other<'_>> = all.iter().map(|l| (&*l.h, &l.links)).collect();
        let tip = gitobj::extend_from(&*self.store, self.alg, &mut h2, &mut links, new, &others)?;
        if h2.tips() != BTreeSet::from([tip.clone()]) || !h2.extends(&h) {
            return Err(Error::conflict("non-fast-forward: a history only grows"));
        }
        if dry || h2 == h {
            return Ok(());
        }
        self.save_links(p, &var, lv, &links)?;
        as_actor(Some(&c.user), || history::save(&*self.store, self.alg, &p.var(&var), old_view.as_ref(), &h2))?;
        Ok(())
    }
}

/// A history git sees, with its links.
struct Linked {
    var: String,
    view: View,
    h: std::sync::Arc<History>,
    links: Links,
    links_view: Option<View>,
    before: usize,
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The ref of history `rest`: `h/<x>` is branch `<x>`, `u/<user>/<n>`
/// branch `u/<user>/<n>`.
fn ref_of(rest: &str) -> Option<String> {
    if let Some(x) = rest.strip_prefix("h/") {
        return Some(format!("refs/heads/{x}"));
    }
    rest.starts_with("u/").then(|| format!("refs/heads/{rest}"))
}

/// The history of branch `b`.
fn var_of(b: &str) -> Option<String> {
    if b.starts_with("u/") {
        return Some(b.to_string());
    }
    (!b.is_empty() && !b.contains('/')).then(|| format!("h/{b}"))
}

/// The node a branch names: the tip, or the newest when the history
/// forks.
fn tip_of(h: &History) -> Option<NodeId> {
    h.tip().or_else(|| h.log().into_iter().next())
}

/// The object an annotated tag names.
fn peeled(body: &[u8]) -> Option<String> {
    let first = body.split(|&b| b == b'\n').next()?;
    let s = std::str::from_utf8(first.strip_prefix(b"object ")?).ok()?;
    Some(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_packets() {
        assert_eq!(var_of("dev").as_deref(), Some("h/dev"));
        assert_eq!(var_of("u/al/x").as_deref(), Some("u/al/x"));
        assert_eq!(var_of("a/b"), None);
        assert_eq!(ref_of("h/dev").as_deref(), Some("refs/heads/dev"));
        assert_eq!(ref_of("u/al/x").as_deref(), Some("refs/heads/u/al/x"));
        assert_eq!(base64("YWw6c2VjcmV0").unwrap(), b"al:secret");
        let (p, w) = route("/al/proj.git/info/refs").unwrap();
        assert_eq!((p.to_string().as_str(), w), ("al/proj", "info/refs"));
        assert!(route("/al/proj/tree").is_none());
        let mut b = Vec::new();
        pkt_str(&mut b, "command=ls-refs\n");
        b.extend_from_slice(DELIM);
        b.extend_from_slice(FLUSH);
        let mut ps = Pkts::new(&b);
        assert!(matches!(ps.next().unwrap(), Some(Pkt::Data(b"command=ls-refs\n"))));
        assert!(matches!(ps.next().unwrap(), Some(Pkt::Delim)));
        assert!(matches!(ps.next().unwrap(), Some(Pkt::Flush)));
    }
}
