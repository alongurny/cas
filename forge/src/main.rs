//! `cas-forge`: the forge server, and its few local commands.

use cas_core::client::HttpStore;
use cas_core::disk::DiskBackend;
use cas_core::server::{DEFAULT_MAX_BODY, Server, Tls};
use cas_forge::api::{self, Options};
use cas_forge::names::ProjectId;
use cas_forge::{Forge, gate};
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const USAGE: &str = "usage:
  cas-forge serve <store> <addr> --cert <file> --key <file> [--web <dir>] [--cache <dir>] [--open-registration]
      serve the forge: the wire at /p/<owner>/<project>, the API at /api/v1, the web pages
  cas-forge user <store> <name> [--admin]
      make a user; the password is read from standard input
  cas-forge token <store> <user> [<label>]
      make a token for a user and print it once
  cas-forge pull <store> <owner>/<project> <mirror> [--cache <dir>]
      pull a mirror now
  cas-forge export <store> <owner>/<project> <history> <git-repo> [<branch>]
      write a history to a git repository, one commit per node
  cas-forge runner <url> --token-file <file> [--ca <file>] [--once]
      run checks for a forge: take jobs, run them, post results";

type Res<T> = Result<T, String>;

struct Args {
    pos: Vec<String>,
    flags: Vec<String>,
}

impl Args {
    fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|f| f == name)
    }
    fn value(&self, name: &str) -> Option<String> {
        let i = self.flags.iter().position(|f| f == name)?;
        self.flags.get(i + 1).cloned()
    }
    fn pos(&self, i: usize, what: &str) -> Res<&str> {
        self.pos.get(i).map(String::as_str).ok_or(format!("missing {what}\n{USAGE}"))
    }
}

fn parse(raw: Vec<String>) -> Args {
    const VALUED: &[&str] = &["--cert", "--key", "--web", "--cache", "--token-file", "--ca"];
    let (mut pos, mut flags) = (Vec::new(), Vec::new());
    let mut it = raw.into_iter();
    while let Some(a) = it.next() {
        if a.starts_with("--") {
            let valued = VALUED.contains(&a.as_str());
            flags.push(a);
            if valued && let Some(v) = it.next() {
                flags.push(v);
            }
        } else {
            pos.push(a);
        }
    }
    Args { pos, flags }
}

fn e(x: impl std::fmt::Display) -> String {
    x.to_string()
}

fn open(dir: &str) -> Res<Arc<Forge>> {
    Ok(gate::open(DiskBackend::open(dir).map_err(|x| format!("{dir}: {x}"))?))
}

fn cache_dir(a: &Args, store: &str) -> PathBuf {
    a.value("--cache").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(store).join("..").join("forge-cache"))
}

fn run(a: Args) -> Res<()> {
    match a.pos.first().map(String::as_str) {
        Some("serve") => {
            let (store, addr) = (a.pos(1, "<store>")?, a.pos(2, "<addr>")?);
            let tls = Tls::from_files(
                a.value("--cert").ok_or("serve needs --cert <file>: it speaks only HTTPS")?.as_ref(),
                a.value("--key").ok_or("serve needs --key <file>")?.as_ref(),
            )
            .map_err(e)?;
            let forge = open(store)?;
            let opts = Options {
                web: a.value("--web").map(PathBuf::from),
                cache: cache_dir(&a, store),
                open_registration: a.flag("--open-registration"),
            };
            let auth_forge = forge.clone();
            let auth: Arc<cas_core::server::Authenticate> =
                Arc::new(move |req| cas_core::Who(api::caller(&auth_forge, req).map(|c| c.who_name())));
            let server = Server::start_routed(
                forge.core.clone(),
                addr,
                16,
                auth,
                tls,
                DEFAULT_MAX_BODY,
                Some(api::router(forge.clone(), opts)),
            )
            .map_err(e)?;
            // Build the read sets now rather than at the first read.
            let warm = forge.clone();
            std::thread::spawn(move || {
                if let Ok(ps) = warm.projects() {
                    let ps: Vec<ProjectId> = ps.iter().cloned().collect();
                    warm.reach.warm(&*warm.store, &ps);
                }
            });
            println!("forge serving {store} at {}", server.url());
            loop {
                std::thread::park();
            }
        }
        Some("user") => {
            let forge = open(a.pos(1, "<store>")?)?;
            let name = a.pos(2, "<name>")?;
            let mut pw = String::new();
            std::io::stdin().read_to_string(&mut pw).map_err(e)?;
            let pw = pw.trim_end_matches(['\n', '\r']);
            forge.create_user(name, Some(pw), None).map_err(e)?;
            if a.flag("--admin") {
                forge
                    .edit_account(name, |d| {
                        d.set("admin", "true");
                        Ok(())
                    })
                    .map_err(e)?;
            }
            println!("made user {name}");
            Ok(())
        }
        Some("token") => {
            let forge = open(a.pos(1, "<store>")?)?;
            let user = a.pos(2, "<user>")?;
            let label = a.pos.get(3).map_or("cli", String::as_str);
            let t = forge.make_token(user, label, &cas_forge::accounts::Scope::All, None).map_err(e)?;
            println!("{t}");
            Ok(())
        }
        Some("pull") => {
            let store = a.pos(1, "<store>")?;
            let forge = open(store)?;
            let p = ProjectId::parse(a.pos(2, "<owner>/<project>")?).map_err(e)?;
            let r = forge.pull_mirror("", &p, a.pos(3, "<mirror>")?, &cache_dir(&a, store)).map_err(e)?;
            println!(
                "{} commits; moved {}",
                r.commits,
                if r.moved.is_empty() { "nothing".into() } else { r.moved.join(", ") }
            );
            Ok(())
        }
        Some("export") => {
            let forge = open(a.pos(1, "<store>")?)?;
            let p = ProjectId::parse(a.pos(2, "<owner>/<project>")?).map_err(e)?;
            let history = a.pos(3, "<history>")?;
            let repo = PathBuf::from(a.pos(4, "<git-repo>")?);
            let branch =
                a.pos.get(5).cloned().unwrap_or_else(|| history.rsplit('/').next().unwrap_or("main").to_string());
            let n = forge.export(&p, history, &repo, &branch).map_err(e)?;
            println!("exported {n} nodes to {} branch {branch}", repo.display());
            Ok(())
        }
        Some("runner") => runner(&a),
        _ => Err(USAGE.into()),
    }
}

/// An HTTPS agent trusting `ca` (PEM) or the public roots.
fn agent(ca: Option<&[u8]>) -> Res<ureq::Agent> {
    use ureq::tls::{PemItem, RootCerts, TlsConfig};
    let roots = match ca {
        None => RootCerts::WebPki,
        Some(pem) => {
            let certs: Vec<_> = ureq::tls::parse_pem(pem)
                .filter_map(|i| match i {
                    Ok(PemItem::Certificate(c)) => Some(c),
                    _ => None,
                })
                .collect();
            RootCerts::Specific(Arc::new(certs))
        }
    };
    Ok(ureq::Agent::config_builder()
        .http_status_as_error(false)
        .https_only(true)
        .tls_config(TlsConfig::builder().root_certs(roots).build())
        .build()
        .into())
}

/// Take jobs, check out each version, run its command, post the result.
fn runner(a: &Args) -> Res<()> {
    let url = a.pos(1, "<url>")?.trim_end_matches('/').to_string();
    let token = std::fs::read_to_string(a.value("--token-file").ok_or("runner needs --token-file <file>")?)
        .map_err(e)?
        .trim()
        .to_string();
    let ca = a.value("--ca").map(std::fs::read).transpose().map_err(e)?;
    let http = agent(ca.as_deref())?;
    loop {
        let mut resp = http
            .get(format!("{url}/api/v1/runner/jobs"))
            .header("Authorization", &format!("Bearer {token}"))
            .call()
            .map_err(e)?;
        let text = resp.body_mut().read_to_string().map_err(e)?;
        if !resp.status().is_success() {
            return Err(format!("jobs: {} {text}", resp.status()));
        }
        let jobs: serde_json::Value = serde_json::from_str(&text).map_err(e)?;
        let jobs = jobs.as_array().cloned().unwrap_or_default();
        for j in &jobs {
            let s = |k: &str| j[k].as_str().unwrap_or("").to_string();
            let (project, version, check, run) = (s("project"), s("version"), s("check"), s("run"));
            eprintln!("runner: {project} {check} on {}", &version[version.len().saturating_sub(12)..]);
            let mut store = HttpStore::new(format!("{url}/p/{project}")).map_err(e)?;
            if let Some(pem) = &ca {
                store = store.trusting(pem).map_err(e)?;
            }
            let store = store.with_token(token.clone());
            let dir = tempfile_dir()?;
            let v = cas_version::View::parse(&version).map_err(e)?;
            let (pass, log) = match cas_version::fs::checkout(&store, &v, None, &dir) {
                Err(x) => (false, format!("checkout failed: {x}\n").into_bytes()),
                Ok(_) => {
                    let out = std::process::Command::new("sh").arg("-c").arg(&run).current_dir(&dir).output();
                    match out {
                        Ok(o) => {
                            let mut log = o.stdout;
                            log.extend_from_slice(&o.stderr);
                            (o.status.success(), log)
                        }
                        Err(x) => (false, format!("could not run: {x}\n").into_bytes()),
                    }
                }
            };
            let _ = std::fs::remove_dir_all(&dir);
            let body = serde_json::json!({
                "project": project, "version": version, "check": check,
                "status": if pass { "pass" } else { "fail" }, "log": String::from_utf8_lossy(&log),
            });
            let r = http
                .post(format!("{url}/api/v1/runner/results"))
                .header("Authorization", &format!("Bearer {token}"))
                .header("Content-Type", "application/json")
                .send(&serde_json::to_vec(&body).unwrap_or_default()[..])
                .map_err(e)?;
            if !r.status().is_success() {
                eprintln!("runner: posting {check} failed: {}", r.status());
            }
        }
        if a.flag("--once") {
            return Ok(());
        }
        if jobs.is_empty() {
            std::thread::sleep(Duration::from_secs(10));
        }
    }
}

fn tempfile_dir() -> Res<PathBuf> {
    let d = std::env::temp_dir().join(format!("cas-runner-{}-{}", std::process::id(), cas_forge::now()));
    let mut n = 0;
    loop {
        let p = d.with_extension(n.to_string());
        if std::fs::create_dir(&p).is_ok() {
            return Ok(p);
        }
        n += 1;
        if n > 1000 {
            return Err("cannot make a temporary directory".into());
        }
    }
}

fn main() -> ExitCode {
    match run(parse(std::env::args().skip(1).collect())) {
        Ok(()) => ExitCode::SUCCESS,
        Err(m) => {
            eprintln!("{m}");
            ExitCode::FAILURE
        }
    }
}
