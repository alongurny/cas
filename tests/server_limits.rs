//! `cas serve` reads request bodies only up to its bound (spec/wire.md: 413).

use cas_core::client::HttpStore;
use cas_core::mem::MemBackend;
use cas_core::server::Server;
use cas_core::{AllowAll, CoreStore, Error, Expect, Policy, Store, Who};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

const MAX: usize = 1024;

fn serve() -> (Server, String) {
    let core = CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive());
    let server =
        Server::start_with_max_body(core, "127.0.0.1:0", 2, Arc::new(|_: &tiny_http::Request| Who::anonymous()), MAX)
            .unwrap();
    let url = server.url();
    (server, url)
}

/// Send `head` and then `body`, and return the status line of the answer.
fn raw(url: &str, head: &str, body: &[u8]) -> String {
    let mut s = TcpStream::connect(url.trim_start_matches("http://")).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
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
    let (_server, url) = serve();
    // A gigabyte is declared and none of it sent: the answer comes from the headers alone.
    let head = "PUT /k/main HTTP/1.1\r\nHost: x\r\nContent-Length: 1073741824\r\n\r\n";
    assert!(raw(&url, head, b"").starts_with("HTTP/1.1 413"));
    let head = "POST /remove/main HTTP/1.1\r\nHost: x\r\nContent-Length: 1073741824\r\n\r\n";
    assert!(raw(&url, head, b"").starts_with("HTTP/1.1 413"));
}

#[test]
fn a_chunked_body_is_read_no_further_than_the_bound() {
    let (_server, url) = serve();
    let chunk = vec![b'a'; 4096];
    let mut body = Vec::new();
    for _ in 0..4 {
        body.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        body.extend_from_slice(&chunk);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"0\r\n\r\n");
    let head = "PUT /k/main HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n";
    assert!(raw(&url, head, &body).starts_with("HTTP/1.1 413"));
}

#[test]
fn bodies_within_the_bound_go_through_and_a_client_sees_too_large() {
    let (_server, url) = serve();
    let store = HttpStore::new(&url);
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
