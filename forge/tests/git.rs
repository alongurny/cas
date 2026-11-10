//! The forge as a git server: the real git client clones, fetches and
//! pushes, and what it pushes lands in the cas histories.

use cas_core::client::HttpStore;
use cas_core::disk::DiskBackend;
use cas_core::server::{DEFAULT_MAX_BODY, Server, self_signed};
use cas_core::{Store, Who};
use cas_forge::api::{self, Options};
use cas_forge::gate;
use cas_version::history::{self, Step};
use cas_version::{Node, Path, View, build, by_alias, put};
use serde_json::{Value as J, json};
use std::collections::BTreeMap;
use std::path::Path as FsPath;
use std::process::Command;
use std::sync::Arc;

struct Site {
    url: String,
    cert: Vec<u8>,
    cafile: std::path::PathBuf,
    http: ureq::Agent,
    dir: tempfile::TempDir,
    _server: Server,
}

fn site() -> Site {
    let dir = tempfile::tempdir().unwrap();
    let forge = gate::open(DiskBackend::open(dir.path().join("store")).unwrap());
    let (tls, cert) = self_signed();
    let f2 = forge.clone();
    let auth: Arc<cas_core::server::Authenticate> =
        Arc::new(move |req| Who(api::caller(&f2, req).map(|c| c.who_name())));
    let opts = Options { web: None, cache: dir.path().join("cache"), open_registration: false };
    let server = Server::start_routed(
        forge.core.clone(),
        "127.0.0.1:0",
        8,
        auth,
        tls,
        DEFAULT_MAX_BODY,
        Some(api::router(forge, opts)),
    )
    .unwrap();
    let url = server.url().replace("127.0.0.1", "localhost");
    let cafile = dir.path().join("ca.pem");
    std::fs::write(&cafile, &cert).unwrap();
    let certs: Vec<_> = ureq::tls::parse_pem(&cert)
        .filter_map(|i| match i {
            Ok(ureq::tls::PemItem::Certificate(c)) => Some(c),
            _ => None,
        })
        .collect();
    let http = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .tls_config(ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::Specific(Arc::new(certs))).build())
        .build()
        .into();
    Site { url, cert, cafile, http, dir, _server: server }
}

impl Site {
    fn ok(&self, method: &str, path: &str, token: Option<&str>, body: Option<J>) -> J {
        let url = format!("{}/api/v1/{path}", self.url);
        let mut r = match method {
            "POST" => self.http.post(&url),
            _ => self.http.put(&url),
        };
        if let Some(t) = token {
            r = r.header("Authorization", &format!("Bearer {t}"));
        }
        let mut resp = r
            .header("Content-Type", "application/json")
            .send(&serde_json::to_vec(&body.unwrap_or(json!({}))).unwrap()[..])
            .unwrap();
        let s = resp.status().as_u16();
        let j: J = serde_json::from_slice(&resp.body_mut().read_to_vec().unwrap()).unwrap_or(J::Null);
        assert!((200..300).contains(&s), "{method} {path}: {s} {j}");
        j
    }

    fn user(&self, name: &str, admin: Option<&str>) -> String {
        self.ok("POST", "users", admin, Some(json!({ "name": name, "password": "correct horse" })));
        self.ok("POST", "login", None, Some(json!({ "name": name, "password": "correct horse" })))["token"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn wire(&self, project: &str, token: &str) -> HttpStore {
        HttpStore::new(format!("{}/p/{project}", self.url)).unwrap().trusting(&self.cert).unwrap().with_token(token)
    }

    /// The project's git URL with `user` and `secret` in it.
    fn remote(&self, project: &str, user: &str, secret: &str) -> String {
        self.url.replacen("https://", &format!("https://{user}:{}@", secret.replace(' ', "%20")), 1)
            + &format!("/{project}.git")
    }

    fn git(&self, dir: &FsPath, args: &[&str]) -> (bool, String) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false"])
            .args(["-c", "init.defaultBranch=main", "-c", "protocol.version=2"])
            .args(args)
            .env("GIT_SSL_CAINFO", &self.cafile)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "true")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        (out.status.success(), text)
    }

    fn must(&self, dir: &FsPath, args: &[&str]) -> String {
        let (ok, t) = self.git(dir, args);
        assert!(ok, "git {args:?}: {t}");
        t.trim().to_string()
    }
}

fn b3() -> &'static cas_core::algo::Algorithm {
    cas_core::algo::lookup("blake3").unwrap()
}

fn version(s: &dyn Store, files: &[(&str, &str)]) -> View {
    let mut m = BTreeMap::new();
    for (p, t) in files {
        m.insert(Path::new(*p).unwrap(), Node::File(put(s, b3(), "raw", t.as_bytes()).unwrap()));
    }
    build(s, b3(), by_alias("dir").unwrap(), &m).unwrap()
}

#[test]
fn git_clones_fetches_and_pushes_cas_histories() {
    let s = site();
    let alice = s.user("alice", None);
    let bob = s.user("bob", Some(&alice));
    s.ok("POST", "projects", Some(&alice), Some(json!({ "name": "demo", "description": "a test" })));
    let work = s.dir.path().join("work");
    std::fs::create_dir(&work).unwrap();

    // A history made in cas, merged to dev.
    let aw = s.wire("alice/demo", &alice);
    let v1 = version(&aw, &[("README.md", "hello\n"), ("src/a.txt", "one\n")]);
    history::commit(&aw, b3(), "u/alice/init", &v1, Step::default(), Some("the start".into())).unwrap();
    s.ok(
        "POST",
        "projects/alice/demo/proposals",
        Some(&alice),
        Some(json!({ "title": "first", "source": "u/alice/init" })),
    );
    s.ok("POST", "projects/alice/demo/proposals/1/merge", Some(&alice), None);

    // Private: no one signed in clones it, nor does bob.
    let (ok, t) = s.git(&work, &["clone", "-q", &format!("{}/alice/demo.git", s.url), "anon"]);
    assert!(!ok, "{t}");
    let (ok, _) = s.git(&work, &["clone", "-q", &s.remote("alice/demo", "bob", &bob), "bob"]);
    assert!(!ok);

    // Alice clones with her token, and with her password.
    s.must(&work, &["clone", "-q", &s.remote("alice/demo", "alice", &alice), "a"]);
    s.must(&work, &["clone", "-q", &s.remote("alice/demo", "alice", "correct horse"), "a2"]);
    let a = work.join("a");
    assert_eq!(std::fs::read_to_string(a.join("README.md")).unwrap(), "hello\n");
    assert_eq!(s.must(&a, &["rev-parse", "--abbrev-ref", "HEAD"]), "dev");
    assert_eq!(s.must(&a, &["log", "--format=%s"]), "the start");
    assert_eq!(
        s.must(&work.join("a2"), &["rev-parse", "HEAD"]),
        s.must(&a, &["rev-parse", "HEAD"]),
        "the same commits every time"
    );
    let first = s.must(&a, &["rev-parse", "HEAD"]);

    // dev is protected: a push to it is refused; her own history takes it.
    std::fs::write(a.join("src/a.txt"), "one\ntwo\n").unwrap();
    s.must(&a, &["commit", "-q", "-am", "two lines\n\nwith a body"]);
    let (ok, t) = s.git(&a, &["push", "origin", "HEAD:dev"]);
    assert!(!ok && t.contains("protected"), "{t}");
    let (ok, t) = s.git(&a, &["push", "origin", "HEAD:u/bob/x"]);
    assert!(!ok && t.contains("u/alice/"), "{t}");
    s.must(&a, &["push", "-q", "origin", "HEAD:u/alice/more"]);
    let pushed = s.must(&a, &["rev-parse", "HEAD"]);

    // It is a cas history: the commit's message is the step's note.
    let (_, h) = history::load(&aw, "u/alice/more").unwrap();
    assert_eq!(h.nodes.len(), 2);
    let tip = h.tip().unwrap();
    let step = h.nodes[&tip].after.values().next().unwrap();
    assert_eq!(step.line(&aw).unwrap().as_deref(), Some("two lines / with a body"));
    let txt = cas_version::object_at(&aw, &tip.version(), &Path::new("src/a.txt").unwrap()).unwrap().unwrap();
    assert_eq!(cas_version::read_blob(&aw, txt.key()).unwrap(), b"one\ntwo\n");

    // Merged in cas, dev moves on to git's own commit.
    s.ok(
        "POST",
        "projects/alice/demo/proposals",
        Some(&alice),
        Some(json!({ "title": "more", "source": "u/alice/more" })),
    );
    s.ok("POST", "projects/alice/demo/proposals/2/merge", Some(&alice), None);
    s.must(&a, &["fetch", "-q", "origin"]);
    let dev = s.must(&a, &["rev-parse", "origin/dev"]);
    assert_eq!(s.must(&a, &["rev-parse", "origin/dev^"]), first, "{dev}");
    let _ = pushed;
    s.must(&a, &["fsck", "--strict", "--no-dangling"]);

    // A rewrite is refused: histories only grow.
    s.must(&a, &["reset", "-q", "--hard", &first]);
    std::fs::write(a.join("other"), "x\n").unwrap();
    s.must(&a, &["add", "other"]);
    s.must(&a, &["commit", "-q", "-m", "elsewhere"]);
    let (ok, t) = s.git(&a, &["push", "-f", "origin", "HEAD:u/alice/more"]);
    assert!(!ok && t.contains("non-fast-forward"), "{t}");
    // A new branch, then deleted.
    s.must(&a, &["push", "-q", "origin", "HEAD:u/alice/gone"]);
    assert!(history::load(&aw, "u/alice/gone").unwrap().0.is_some());
    s.must(&a, &["push", "-q", "origin", ":u/alice/gone"]);
    assert!(history::load(&aw, "u/alice/gone").unwrap().0.is_none());

    // Bob, once a writer, fetches incrementally and pushes his own.
    s.ok("PUT", "projects/alice/demo/members/bob", Some(&alice), Some(json!({ "role": "write" })));
    let b = work.join("b");
    s.must(&work, &["clone", "-q", &s.remote("alice/demo", "bob", &bob), "b"]);
    assert_eq!(s.must(&b, &["rev-parse", "HEAD"]), dev);
    std::fs::write(b.join("bob.txt"), "bob\n").unwrap();
    s.must(&b, &["add", "bob.txt"]);
    s.must(&b, &["commit", "-q", "-m", "bob's"]);
    s.must(&b, &["push", "-q", "origin", "HEAD:u/bob/feature"]);
    s.must(&a, &["fetch", "-q", "origin"]);
    assert_eq!(s.must(&a, &["rev-parse", "origin/u/bob/feature"]), s.must(&b, &["rev-parse", "HEAD"]));
    s.must(&a, &["fsck", "--strict", "--no-dangling"]);
}

#[test]
fn a_mirrored_repository_keeps_its_commit_ids() {
    let s = site();
    let alice = s.user("alice", None);
    s.ok("POST", "projects", Some(&alice), Some(json!({ "name": "m", "description": "" })));
    let up = s.dir.path().join("up");
    std::fs::create_dir(&up).unwrap();
    s.must(&up, &["init", "-q", "-b", "main"]);
    std::fs::write(up.join("a"), "1\n").unwrap();
    s.must(&up, &["add", "a"]);
    s.must(&up, &["commit", "-q", "-m", "one"]);
    s.must(&up, &["tag", "-a", "v1", "-m", "first release"]);
    s.must(&up, &["checkout", "-q", "-b", "side"]);
    std::fs::write(up.join("b"), "2\n").unwrap();
    s.must(&up, &["add", "b"]);
    s.must(&up, &["commit", "-q", "-m", "side work"]);
    s.must(&up, &["checkout", "-q", "main"]);
    std::fs::write(up.join("a"), "1\n1\n").unwrap();
    s.must(&up, &["commit", "-q", "-am", "two"]);
    s.ok("POST", "projects/alice/m/mirrors", Some(&alice), Some(json!({ "name": "up", "url": up.to_str().unwrap() })));
    s.ok("POST", "projects/alice/m/mirrors/up/pull", Some(&alice), None);

    let work = s.dir.path().join("work");
    std::fs::create_dir(&work).unwrap();
    s.must(&work, &["clone", "-q", &s.remote("alice/m", "alice", &alice), "c"]);
    let c = work.join("c");
    assert_eq!(s.must(&c, &["rev-parse", "HEAD"]), s.must(&up, &["rev-parse", "main"]));
    assert_eq!(s.must(&c, &["rev-parse", "origin/u/alice/side"]), s.must(&up, &["rev-parse", "side"]));
    assert_eq!(s.must(&c, &["rev-parse", "v1^{commit}"]), s.must(&up, &["rev-parse", "v1^{commit}"]));
    s.must(&c, &["fsck", "--strict", "--no-dangling"]);

    // A later pull brings only what is new, and the ids still hold.
    std::fs::write(up.join("a"), "1\n1\n1\n").unwrap();
    s.must(&up, &["commit", "-q", "-am", "three"]);
    s.ok("POST", "projects/alice/m/mirrors/up/pull", Some(&alice), None);
    s.must(&c, &["pull", "-q", "--ff-only"]);
    assert_eq!(s.must(&c, &["rev-parse", "HEAD"]), s.must(&up, &["rev-parse", "main"]));
}
