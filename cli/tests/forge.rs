//! The forge's commands end to end: a forge on a fresh store, driven only
//! through `cas`, as a person at a terminal would.

use cas_core::Who;
use cas_core::disk::DiskBackend;
use cas_core::server::{DEFAULT_MAX_BODY, Server, self_signed};
use cas_forge::api::{self, Options};
use cas_forge::gate;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

struct Site {
    url: String,
    ca: String,
    dir: tempfile::TempDir,
    _server: Server,
}

fn site() -> Site {
    let dir = tempfile::tempdir().unwrap();
    let forge = gate::open(DiskBackend::open(dir.path().join("store")).unwrap());
    let (tls, cert) = self_signed();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, cert).unwrap();
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
    Site { url, ca: ca.to_str().unwrap().to_string(), dir, _server: server }
}

impl Site {
    /// `cas --store <url><at> --token-file <who>.token <args>`, with `stdin`.
    fn run(&self, who: &str, at: &str, args: &[&str], stdin: &str) -> (bool, String) {
        let tok = self.dir.path().join(format!("{who}.token"));
        let mut c = Command::new(env!("CARGO_BIN_EXE_cas"))
            .current_dir(self.dir.path())
            .env("CAS_AUTHOR", who)
            .args(["--ca", &self.ca, "--store", &format!("{}{at}", self.url), "--token-file", tok.to_str().unwrap()])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        c.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let o = c.wait_with_output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        (o.status.success(), text)
    }

    fn ok(&self, who: &str, at: &str, args: &[&str]) -> String {
        let (ok, out) = self.run(who, at, args, "");
        assert!(ok, "cas {args:?} as {who}: {out}");
        out
    }

    fn fails(&self, who: &str, at: &str, args: &[&str]) -> String {
        let (ok, out) = self.run(who, at, args, "");
        assert!(!ok, "cas {args:?} as {who} should fail: {out}");
        out
    }
}

fn write(dir: &Path, name: &str, text: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), text).unwrap();
}

#[test]
fn everything_the_web_does_from_the_terminal() {
    let s = site();
    let p = "/p/lab/tool";
    // Accounts: the first sign-up is the first user; the token goes to the file.
    assert!(s.run("alice", "", &["signup", "alice"], "pw-alice-1\n").0);
    assert!(s.run("alice", "", &["login", "alice"], "pw-alice-1\n").1.contains("token in"));
    assert!(!s.run("bob", "", &["signup", "bob"], "pw-bob-1\n").0, "sign-up is closed after the first");
    assert!(s.ok("alice", "", &["whoami"]).contains("alice  user"));
    // Organizations, projects, settings.
    s.ok("alice", "", &["org", "create", "lab"]);
    s.ok("alice", "", &["project", "create", "tool", "--owner", "lab", "--description", "A tool"]);
    assert!(s.ok("alice", "", &["projects"]).contains("lab/tool  private  admin  A tool"));
    s.ok("alice", p, &["project", "gate", "h/dev", "--approvals", "0", "--resolved"]);
    s.ok("alice", p, &["project", "label", "bug", "--color", "#ff0000"]);
    let show = s.ok("alice", p, &["project", "show"]);
    assert!(show.contains("protected h/dev") && show.contains("bug"), "{show}");
    assert!(s.fails("alice", p, &["project", "delete"]).contains("--yes"));
    // An option the command's forms do not name is refused, with its usage.
    let bad = s.fails("alice", p, &["issue", "list", "--colour", "red"]);
    assert!(bad.starts_with("issue takes no --colour\nusage: cas issue list"), "{bad}");
    // A private history, pushed over the wire, proposed and reviewed.
    let w = s.dir.path().join("w");
    write(&w, "f.txt", "one\ntwo\n");
    let wd = w.to_str().unwrap();
    s.ok("alice", p, &["-H", "u/alice/a", "commit", wd]);
    s.fails("alice", p, &["-H", "h/dev", "commit", wd]);
    s.ok("alice", p, &["proposal", "open", "Start", "--source", "u/alice/a"]);
    s.ok("alice", p, &["proposal", "merge", "1"]);
    write(&w, "f.txt", "one\ntwo\nthree\n");
    s.ok("alice", p, &["-H", "u/alice/a", "commit", wd]);
    assert!(s.ok("alice", p, &["proposal", "open", "Add", "three", "--source", "u/alice/a"]).contains("opened #2"));
    assert!(s.ok("alice", p, &["proposal", "thread", "2", "f.txt:3", "Why", "three?"]).contains("thread 1"));
    s.fails("alice", p, &["proposal", "thread", "2", "f.txt:9", "no such line"]);
    s.ok("alice", p, &["proposal", "reply", "2", "1", "Because."]);
    s.ok("alice", p, &["proposal", "promote", "2", "1"]);
    // An unresolved thread holds the merge.
    s.fails("alice", p, &["proposal", "merge", "2"]);
    s.ok("alice", p, &["proposal", "resolve", "2", "1"]);
    let shown = s.ok("alice", p, &["proposal", "show", "2"]);
    assert!(shown.contains("+three") && shown.contains("becomes a step note") && shown.contains("Because."), "{shown}");
    assert!(s.ok("alice", p, &["proposal", "merge", "2"]).contains("merged"));
    assert!(s.ok("alice", p, &["proposal", "list", "--state", "all"]).contains("#2  merged  Add three"));
    // The promoted note is the merged step's note on its line.
    assert!(s.ok("alice", p, &["-H", "h/dev", "diff", "--annotated"]).contains("# Why three?"));
    let blame = s.ok("alice", p, &["blame", "f.txt"]);
    assert_eq!(blame.lines().count(), 3, "{blame}");
    // Issues.
    assert!(
        s.ok("alice", p, &["issue", "open", "Crash", "--body", "It crashes", "--label", "bug", "--at", "f.txt:2"])
            .contains("opened #3")
    );
    s.ok("alice", p, &["issue", "label", "3", "+P2", "-bug"]);
    s.ok("alice", p, &["issue", "comment", "3", "Looking."]);
    s.ok("alice", p, &["issue", "close", "3"]);
    let issue = s.ok("alice", p, &["issue", "show", "3"]);
    assert!(issue.contains("[closed]  P2") && issue.contains("Looking.") && issue.contains("at f.txt:2"), "{issue}");
    assert!(s.ok("alice", p, &["issue", "list"]).trim().is_empty());
    // Releases and their archive.
    write(s.dir.path(), "cl", "First.\n");
    s.ok("alice", p, &["release", "publish", "1.0.0", "--changelog", "cl"]);
    assert!(s.ok("alice", p, &["release", "list"]).contains("1.0.0"));
    s.ok("alice", p, &["release", "archive", "1.0.0", "v1.tar.gz"]);
    assert!(std::fs::metadata(s.dir.path().join("v1.tar.gz")).unwrap().len() > 0);
    // A token for one project reads it and nothing else.
    s.ok("alice", "", &["token", "make", "ci", "--scope", "project:lab/tool", "--days", "1", "--out", "ci.token"]);
    assert!(s.ok("ci", p, &["issue", "list", "--state", "all"]).contains("Crash"));
    s.fails("ci", "", &["org", "create", "other"]);
    // The event log, and the API's own answer with --json.
    assert!(s.ok("alice", "", &["events"]).contains("merge"));
    let j: serde_json::Value = serde_json::from_str(&s.ok("alice", p, &["--json", "proposal", "show", "2"])).unwrap();
    assert_eq!(j["state"], "merged");
}
