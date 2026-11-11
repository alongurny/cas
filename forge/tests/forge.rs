//! The forge end to end: a server on a fresh store, driven through the
//! wire and the API as people would.

use cas_core::client::HttpStore;
use cas_core::disk::DiskBackend;
use cas_core::server::{DEFAULT_MAX_BODY, Server, self_signed};
use cas_core::{Store, Who};
use cas_forge::api::{self, Options};
use cas_forge::gate;
use cas_version::commits::{self, Commit, Notes};
use cas_version::{Node, Path, View, build, by_alias, put};
use serde_json::{Value as J, json};
use std::collections::BTreeMap;
use std::sync::Arc;

struct Site {
    url: String,
    cert: Vec<u8>,
    http: ureq::Agent,
    _server: Server,
    _dir: tempfile::TempDir,
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
    Site { url, cert, http, _server: server, _dir: dir }
}

impl Site {
    fn call(&self, method: &str, path: &str, token: Option<&str>, body: Option<J>) -> (u16, J) {
        let url = format!("{}/api/v1/{path}", self.url);
        let auth = token.map(|t| format!("Bearer {t}"));
        let mut resp = match (method, body) {
            ("GET", _) => {
                let mut r = self.http.get(&url);
                if let Some(a) = &auth {
                    r = r.header("Authorization", a);
                }
                r.call().unwrap()
            }
            (m, b) => {
                let mut r = match m {
                    "POST" => self.http.post(&url),
                    "PUT" => self.http.put(&url),
                    "PATCH" => self.http.patch(&url),
                    _ => {
                        let mut r = self.http.delete(&url);
                        if let Some(a) = &auth {
                            r = r.header("Authorization", a);
                        }
                        let mut resp = r.call().unwrap();
                        let s = resp.status().as_u16();
                        let t = resp.body_mut().read_to_string().unwrap();
                        return (s, serde_json::from_str(&t).unwrap_or(J::Null));
                    }
                };
                if let Some(a) = &auth {
                    r = r.header("Authorization", a);
                }
                r.header("Content-Type", "application/json")
                    .send(&serde_json::to_vec(&b.unwrap_or(json!({}))).unwrap()[..])
                    .unwrap()
            }
        };
        let s = resp.status().as_u16();
        let bytes = resp.body_mut().read_to_vec().unwrap();
        (s, serde_json::from_slice(&bytes).unwrap_or(J::Null))
    }

    fn ok(&self, method: &str, path: &str, token: Option<&str>, body: Option<J>) -> J {
        let (s, j) = self.call(method, path, token, body);
        assert!((200..300).contains(&s), "{method} {path}: {s} {j}");
        j
    }

    fn user(&self, name: &str, admin_token: Option<&str>) -> String {
        self.ok("POST", "users", admin_token, Some(json!({ "name": name, "password": "correct horse" })));
        self.ok("POST", "login", None, Some(json!({ "name": name, "password": "correct horse" })))["token"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// The project's store, as `token`'s holder sees it on the wire.
    fn wire(&self, project: &str, token: &str) -> HttpStore {
        HttpStore::new(format!("{}/p/{project}", self.url)).unwrap().trusting(&self.cert).unwrap().with_token(token)
    }
}

fn b3() -> &'static cas_core::algo::Algorithm {
    cas_core::algo::lookup("blake3").unwrap()
}

fn version(store: &dyn Store, files: &[(&str, &str)]) -> View {
    let mut e = BTreeMap::new();
    for (p, text) in files {
        e.insert(Path::new(*p).unwrap(), Node::File(put(store, b3(), "raw", text.as_bytes()).unwrap()));
    }
    build(store, b3(), by_alias("directory").unwrap(), &e).unwrap()
}

/// A commit of `version` after `parent`, noted `note`; its key.
fn commit(store: &dyn Store, parent: Option<&str>, version: &View, note: &str) -> String {
    let notes = Notes { note: note.to_string(), paths: BTreeMap::new() };
    let step = commits::write_notes(store, b3(), &notes, &|_| Ok(false)).unwrap();
    let c = Commit {
        version: version.key().to_string(),
        author: commits::put_text(store, b3(), "author", "Test <test@example.org>").unwrap(),
        notes: if parent.is_none() { step.clone() } else { None },
        time: 1_762_880_000,
        parents: parent.map(|p| (p.to_string(), step)).into_iter().collect(),
    };
    commits::write(store, b3(), &c).unwrap()
}

#[test]
fn people_projects_proposals_issues_releases() {
    let s = site();
    let alice = s.user("alice", None);
    // Sign-up is closed after the first user: only an admin makes accounts.
    assert_eq!(s.call("POST", "users", None, Some(json!({ "name": "eve", "password": "12345678" }))).0, 403);
    let bob = s.user("bob", Some(&alice));
    let carol = s.user("carol", Some(&alice));
    assert_eq!(s.ok("GET", "user", Some(&alice), None)["admin"], true);

    s.ok("POST", "projects", Some(&alice), Some(json!({ "name": "demo", "description": "a test" })));
    // Private: others do not see it.
    assert_eq!(s.call("GET", "projects/alice/demo", Some(&bob), None).0, 404);
    assert_eq!(s.call("GET", "projects/alice/demo", None, None).0, 404);

    // Alice pushes a private history; h/dev is protected, even for her.
    let aw = s.wire("alice/demo", &alice);
    let v1 = version(&aw, &[("README.md", "hello\n"), ("src/a.txt", "one\ntwo\nthree\n")]);
    let c1 = commit(&aw, None, &v1, "");
    commits::advance(&aw, "u/alice/init", None, &c1).unwrap();
    assert!(commits::advance(&aw, "h/dev", None, &c1).is_err());

    let n = s.ok(
        "POST",
        "projects/alice/demo/proposals",
        Some(&alice),
        Some(json!({ "title": "first", "source": "u/alice/init" })),
    )["number"]
        .as_u64()
        .unwrap();
    assert_eq!(n, 1);
    s.ok("POST", "projects/alice/demo/proposals/1/merge", Some(&alice), None);
    let p = s.ok("GET", "projects/alice/demo", Some(&alice), None);
    assert_eq!(p["readme"]["text"], "hello\n");
    let tree = s.ok("GET", "projects/alice/demo/tree", Some(&alice), None);
    assert_eq!(tree["entries"].as_array().unwrap().len(), 2);

    // Bob cannot read the project's blobs, and asking reveals nothing.
    let bw = s.wire("alice/demo", &bob);
    let readme = cas_version::object_at(&aw, &v1, &Path::new("README.md").unwrap()).unwrap().unwrap();
    assert!(bw.read(readme.key()).is_err());
    assert_eq!(bw.has_many(&[readme.key().to_string()]).unwrap(), vec![false]);
    assert!(aw.has_many(&[readme.key().to_string()]).unwrap()[0]);

    // Bob joins as a writer and proposes a change.
    s.ok("PUT", "projects/alice/demo/members/bob", Some(&alice), Some(json!({ "role": "write" })));
    assert!(bw.read(readme.key()).unwrap().is_some());
    let v2 = version(&bw, &[("README.md", "hello\n"), ("src/a.txt", "one\ntwo and a half\nthree\n")]);
    let dev = commits::tip(&bw, "h/dev").unwrap().unwrap();
    let c2 = commit(&bw, Some(&dev), &v2, "");
    commits::advance(&bw, "u/bob/fix", None, &c2).unwrap();
    // Not his to write: another user's history, or a shared one.
    assert!(commits::advance(&bw, "u/alice/x", None, &c2).is_err());
    let n = s.ok(
        "POST",
        "projects/alice/demo/proposals",
        Some(&bob),
        Some(json!({ "title": "better two", "source": "u/bob/fix" })),
    )["number"]
        .as_u64()
        .unwrap();
    assert_eq!(n, 2);

    // One approval needed; a writer cannot merge.
    s.ok("PUT", "projects/alice/demo/gates/h/dev", Some(&alice), Some(json!({ "approvals": 1, "resolved": true })));
    assert_eq!(s.call("POST", "projects/alice/demo/proposals/2/merge", Some(&bob), None).0, 403);
    // Review: a thread on the new line, which the author promotes.
    s.ok(
        "POST",
        "projects/alice/demo/proposals/2/threads",
        Some(&alice),
        Some(json!({
            "path": "src/a.txt", "side": "new", "line": 2, "text": "two and a half: rounding up",
        })),
    );
    let info = s.ok("GET", "projects/alice/demo/proposals/2", Some(&bob), None);
    assert_eq!(info["gates"]["ok"], false);
    assert_eq!(info["threads"][0]["text"], "two and a half");
    s.ok("POST", "projects/alice/demo/proposals/2/threads/1/promote", Some(&bob), None);
    assert_eq!(s.call("POST", "projects/alice/demo/proposals/2/threads/1/promote", Some(&alice), None).0, 403);
    assert_eq!(s.call("POST", "projects/alice/demo/proposals/2/merge", Some(&alice), None).0, 409);
    s.ok("POST", "projects/alice/demo/proposals/2/threads/1/resolve", Some(&bob), None);
    s.ok("POST", "projects/alice/demo/proposals/2/approve", Some(&alice), None);
    let tip =
        s.ok("POST", "projects/alice/demo/proposals/2/merge", Some(&alice), None)["tip"].as_str().unwrap().to_string();
    let step = s.ok("GET", &format!("projects/alice/demo/step?node={}", urlenc(&tip)), Some(&bob), None);
    let noted: Vec<&J> = step["lines"].as_array().unwrap().iter().filter(|l| l["note"] != "").collect();
    assert_eq!(noted.len(), 1, "{step}");
    assert_eq!(noted[0]["text"], "+two and a half");
    assert_eq!(noted[0]["note"], "two and a half: rounding up");
    assert_eq!(s.ok("GET", "projects/alice/demo/proposals/2", Some(&bob), None)["state"], "merged");

    // Blame: line 2 from the merge, the others from the first node.
    let blame = s.ok("GET", "projects/alice/demo/blame?path=src/a.txt", Some(&bob), None);
    let lines = blame["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["node"], lines[2]["node"]);
    assert_ne!(lines[0]["node"], lines[1]["node"]);
    assert_eq!(lines[1]["node"], tip.as_str());

    // Issues, with exclusive priority labels.
    let n = s.ok(
        "POST",
        "projects/alice/demo/issues",
        Some(&bob),
        Some(json!({ "title": "it breaks", "body": "how" })),
    )["number"]
        .as_u64()
        .unwrap();
    assert_eq!(n, 3);
    s.ok("PATCH", "projects/alice/demo/issues/3", Some(&alice), Some(json!({ "add_labels": ["P1"] })));
    s.ok(
        "PATCH",
        "projects/alice/demo/issues/3",
        Some(&alice),
        Some(json!({ "add_labels": ["P2", "difficulty:easy"] })),
    );
    s.ok("POST", "projects/alice/demo/issues/3/comments", Some(&bob), Some(json!({ "text": "still" })));
    let i = s.ok("GET", "projects/alice/demo/issues/3", Some(&bob), None);
    assert_eq!(i["labels"], json!(["P2", "difficulty:easy"]));
    assert_eq!(i["comments"][0]["text"], "still");
    assert_eq!(
        s.call("PATCH", "projects/alice/demo/issues/3", Some(&carol), Some(json!({ "add_labels": ["P1"] }))).0,
        404
    );
    s.ok("PATCH", "projects/alice/demo/issues/3", Some(&bob), Some(json!({ "state": "closed" })));
    assert_eq!(s.ok("GET", "projects/alice/demo/issues?state=open", Some(&bob), None).as_array().unwrap().len(), 0);

    // A release, and its archive.
    s.ok(
        "POST",
        "projects/alice/demo/releases",
        Some(&alice),
        Some(json!({ "tag": "1.0.0", "changelog": "two is now two and a half" })),
    );
    let rel = s.ok("GET", "projects/alice/demo/releases", Some(&bob), None);
    assert_eq!(rel[0]["tags"], json!(["1.0.0"]));
    let mut resp = s
        .http
        .get(format!("{}/api/v1/projects/alice/demo/releases/1.0.0/archive.tar.gz", s.url))
        .header("Authorization", &format!("Bearer {bob}"))
        .call()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert!(resp.body_mut().read_to_vec().unwrap().len() > 50);

    // The event log saw it all; carol sees none of this private project.
    let ev = s.ok("GET", "events", Some(&alice), None);
    let kinds: Vec<&str> = ev["events"].as_array().unwrap().iter().filter_map(|e| e["kind"].as_str()).collect();
    for k in ["project", "move", "proposal", "review", "merge", "issue", "comment", "release"] {
        assert!(kinds.contains(&k), "{k} in {kinds:?}");
    }
    let ce = s.ok("GET", "events", Some(&carol), None);
    assert!(ce["events"].as_array().unwrap().iter().all(|e| e["project"] != "alice/demo"));

    // Everything is in the store: settings, proposals, issues, accounts.
    let ex = s.ok("GET", "projects/alice/demo", Some(&alice), None);
    assert!(ex["histories"].as_array().unwrap().iter().any(|h| h["name"] == "h/release"));
}

fn urlenc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.:".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
