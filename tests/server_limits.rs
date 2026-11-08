//! `cas serve` reads request bodies only up to its bound (spec/wire.md: 413),
//! and takes no chunked request bodies (411).

use cas_core::client::HttpStore;
use cas_core::mem::MemBackend;
use cas_core::server::Request;
use cas_core::server::Server;
use cas_core::{AllowAll, CoreStore, Error, Expect, Policy, Store, Who};
use rustls_pki_types::pem::PemObject;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

const MAX: usize = 1024;

struct Served {
    _server: Server,
    url: String,
    ca: Vec<u8>,
}

fn serve() -> Served {
    let core = CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive());
    let (tls, ca) = cas_core::server::self_signed();
    let server =
        Server::start_with_max_body(core, "127.0.0.1:0", 2, Arc::new(|_: &Request| Who::anonymous()), tls, MAX)
            .unwrap();
    Served { url: server.url(), _server: server, ca }
}

/// Send `head` and then `body` over TLS, and return the status line of the answer.
fn raw(served: &Served, head: &str, body: &[u8]) -> String {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(rustls_pki_types::CertificateDer::from_pem_slice(&served.ca).unwrap()).unwrap();
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = rustls_pki_types::ServerName::try_from("127.0.0.1").unwrap();
    let conn = rustls::ClientConnection::new(Arc::new(config), name).unwrap();
    let tcp = TcpStream::connect(served.url.trim_start_matches("https://")).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut s = rustls::StreamOwned::new(conn, tcp);
    s.write_all(head.as_bytes()).unwrap();
    // The server may answer and close before taking the whole body.
    let _ = s.write_all(body);
    let mut answer = Vec::new();
    let mut buf = [0; 4096];
    while let Ok(n) = s.read(&mut buf) {
        if n == 0 {
            break;
        }
        answer.extend_from_slice(&buf[..n]);
        if answer.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8_lossy(&answer).lines().next().unwrap_or_default().to_string()
}

#[test]
fn a_declared_length_over_the_bound_is_refused_unread() {
    let served = serve();
    // A gigabyte is declared and none of it sent: the answer comes from the headers alone.
    let head = "PUT /k/main HTTP/1.1\r\nHost: x\r\nContent-Length: 1073741824\r\n\r\n";
    assert!(raw(&served, head, b"").starts_with("HTTP/1.1 413"));
    let head = "POST /remove/main HTTP/1.1\r\nHost: x\r\nContent-Length: 1073741824\r\n\r\n";
    assert!(raw(&served, head, b"").starts_with("HTTP/1.1 413"));
}

#[test]
fn a_chunked_body_is_refused_unread() {
    let served = serve();
    let chunk = vec![b'a'; 4096];
    let mut body = Vec::new();
    for _ in 0..4 {
        body.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        body.extend_from_slice(&chunk);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"0\r\n\r\n");
    let head = "PUT /k/main HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n";
    assert!(raw(&served, head, &body).starts_with("HTTP/1.1 411"));
}

#[test]
fn bodies_within_the_bound_go_through_and_a_client_sees_too_large() {
    let served = serve();
    let store = HttpStore::new(&served.url).unwrap().trusting(&served.ca).unwrap();
    // Blob bodies: variables hold only short views.
    let b3 = cas_core::algo::lookup("blake3").unwrap();
    let fits = vec![b'v'; MAX - 1];
    store.write(&b3.key(&fits), &fits, &Expect::Absent).unwrap();
    assert_eq!(store.read(&b3.key(&fits)).unwrap(), Some(fits));
    let big = vec![b'v'; MAX];
    match store.write(&b3.key(&big), &big, &Expect::Absent) {
        Err(Error::Refused { reason, .. }) => assert_eq!(reason, "too-large"),
        other => panic!("expected too-large, got {other:?}"),
    }
    assert_eq!(store.read(&b3.key(&big)).unwrap(), None);
}

#[test]
fn only_https_is_spoken() {
    let served = serve();
    // The client refuses any other scheme.
    assert!(HttpStore::new(served.url.replace("https://", "http://")).is_err());
    // The server answers plain HTTP with no HTTP response.
    let mut s = TcpStream::connect(served.url.trim_start_matches("https://")).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(b"GET /list HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    let mut answer = Vec::new();
    let _ = s.read_to_end(&mut answer);
    assert!(!answer.starts_with(b"HTTP/"));
    // A client that does not trust the certificate is refused.
    let untrusting = HttpStore::new(&served.url).unwrap();
    assert!(matches!(untrusting.read("main"), Err(Error::Unavailable(_))));
}
