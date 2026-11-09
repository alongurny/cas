//! Serve a [`CoreStore`] over HTTPS (spec/wire.md). TLS only: there is no
//! plain HTTP listener. Each connection is one TLS session on one worker
//! thread, carrying HTTP/1.1 requests one after another (keep-alive).

use crate::chunk;
use crate::gate::Who;
use crate::key::{Parsed, classify};
use crate::store::{CoreStore, Session, Store};
use crate::types::{Error, Span};
use crate::wire;
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

/// One HTTP request, body read.
pub struct Request {
    pub method: String,
    /// Path and query, as sent.
    pub url: String,
    headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// The value of header `name` (case-insensitive), if present.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// Turns a request into the caller's identity. This is where the outer
/// protocol plugs in.
pub type Authenticate = dyn Fn(&Request) -> Who + Send + Sync;

/// The bearer token of a request, from `Authorization: Bearer <token>`.
pub fn bearer(req: &Request) -> Option<&str> {
    req.header("Authorization").and_then(|v| v.strip_prefix("Bearer ")).map(str::trim)
}

/// The largest request body [`Server::start`] reads: an expect frame and a
/// value. A body is read before the gate can see it (the gate may look at the
/// value), so without a bound any caller could make each worker buffer
/// without end. Larger requests are answered 413 before they are buffered.
pub const DEFAULT_MAX_BODY: usize = 64 << 20;

/// The largest request head (request line and headers).
const MAX_HEAD: usize = 64 << 10;

/// An idle kept-alive connection is closed after this, freeing its worker.
const IDLE: Duration = Duration::from_secs(30);

/// The server's certificate chain and private key.
///
/// With the `bench-insecure` feature, [`Tls::plain_http_for_benchmarks`]
/// also exists: no TLS at all. It is for benchmarks against git's plain
/// transports only; normal builds cannot speak anything but HTTPS.
#[derive(Clone)]
pub struct Tls(Option<Arc<ServerConfig>>);

impl Tls {
    /// From PEM: the certificate chain (leaf first) and its private key.
    pub fn from_pem(chain: &[u8], key: &[u8]) -> io::Result<Tls> {
        let bad =
            |what: &str, e: &dyn std::fmt::Display| io::Error::new(io::ErrorKind::InvalidData, format!("{what}: {e}"));
        let certs: Vec<CertificateDer<'static>> =
            CertificateDer::pem_slice_iter(chain).collect::<Result<_, _>>().map_err(|e| bad("certificate", &e))?;
        if certs.is_empty() {
            return Err(bad("certificate", &"no certificate in the PEM file"));
        }
        let key = PrivateKeyDer::from_pem_slice(key).map_err(|e| bad("private key", &e))?;
        let config = ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| bad("TLS", &e))?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| bad("TLS", &e))?;
        Ok(Tls(Some(Arc::new(config))))
    }

    /// No TLS: plain HTTP. Only in builds with the `bench-insecure` feature.
    #[cfg(feature = "bench-insecure")]
    pub fn plain_http_for_benchmarks() -> Tls {
        Tls(None)
    }

    /// [`Tls::from_pem`] from two files.
    pub fn from_files(chain: &std::path::Path, key: &std::path::Path) -> io::Result<Tls> {
        let read = |p: &std::path::Path| {
            std::fs::read(p).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", p.display())))
        };
        Tls::from_pem(&read(chain)?, &read(key)?)
    }
}

/// A fresh self-signed certificate for `localhost` and 127.0.0.1, for tests:
/// the server's TLS and the certificate (PEM) a client must trust.
#[cfg(feature = "test-tls")]
pub fn self_signed() -> (Tls, Vec<u8>) {
    let (cert, key) = self_signed_pem();
    (Tls::from_pem(&cert, &key).unwrap(), cert)
}

/// The certificate and private key of [`self_signed`], as PEM files hold them.
#[cfg(feature = "test-tls")]
pub fn self_signed_pem() -> (Vec<u8>, Vec<u8>) {
    let c = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    (c.cert.pem().into_bytes(), c.signing_key.serialize_pem().into_bytes())
}

pub struct Server {
    addr: SocketAddr,
    closed: Arc<AtomicBool>,
    plain: bool,
}

impl Server {
    /// Listen on `addr` (e.g. `127.0.0.1:0`) with `workers` connection
    /// threads, reading request bodies of at most [`DEFAULT_MAX_BODY`] bytes.
    pub fn start(
        store: Arc<CoreStore>,
        addr: &str,
        workers: usize,
        auth: Arc<Authenticate>,
        tls: Tls,
    ) -> io::Result<Server> {
        Server::start_with_max_body(store, addr, workers, auth, tls, DEFAULT_MAX_BODY)
    }

    /// [`Server::start`], reading request bodies of at most `max_body` bytes.
    pub fn start_with_max_body(
        store: Arc<CoreStore>,
        addr: &str,
        workers: usize,
        auth: Arc<Authenticate>,
        tls: Tls,
        max_body: usize,
    ) -> io::Result<Server> {
        let listener = TcpListener::bind(addr)?;
        let addr = listener.local_addr()?;
        let closed = Arc::new(AtomicBool::new(false));
        let (send, recv) = mpsc::channel::<TcpStream>();
        let recv = Arc::new(Mutex::new(recv));
        for _ in 0..workers.max(1) {
            let (recv, store, auth, tls) = (Arc::clone(&recv), Arc::clone(&store), Arc::clone(&auth), tls.clone());
            thread::spawn(move || {
                loop {
                    let next = recv.lock().unwrap().recv();
                    let Ok(tcp) = next else { return };
                    let _ = serve_connection(&store, &auth, &tls, tcp, max_body);
                }
            });
        }
        let plain = tls.0.is_none();
        let stop = Arc::clone(&closed);
        thread::spawn(move || {
            for tcp in listener.incoming() {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                if let Ok(tcp) = tcp
                    && send.send(tcp).is_err()
                {
                    return;
                }
            }
        });
        Ok(Server { addr, closed, plain })
    }

    pub fn url(&self) -> String {
        format!("{}://{}", if self.plain { "http" } else { "https" }, self.addr)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        // Wake the accept loop so it sees the flag.
        let _ = TcpStream::connect(self.addr);
    }
}

/// A connection: TLS, or (bench-insecure builds only) plain TCP.
enum Conn {
    Tls(Box<StreamOwned<ServerConnection, TcpStream>>),
    #[cfg_attr(not(feature = "bench-insecure"), allow(dead_code))]
    Plain(TcpStream),
}

impl Conn {
    fn close(&mut self) {
        if let Conn::Tls(s) = self {
            s.conn.send_close_notify();
        }
        let _ = self.flush();
    }
}

impl Read for Conn {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        match self {
            Conn::Tls(s) => s.read(b),
            Conn::Plain(s) => s.read(b),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        match self {
            Conn::Tls(s) => s.write(b),
            Conn::Plain(s) => s.write(b),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Conn::Tls(s) => s.flush(),
            Conn::Plain(s) => s.flush(),
        }
    }
}

/// One answer: status, headers and body.
pub(crate) struct Response {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
}

impl Response {
    fn new(status: u16) -> Response {
        Response { status, headers: Vec::new(), body: Vec::new() }
    }

    fn data(body: impl Into<Vec<u8>>) -> Response {
        Response { body: body.into(), ..Response::new(200) }
    }

    fn with(mut self, name: &'static str, value: &str) -> Response {
        self.headers.push((name, value.to_string()));
        self
    }

    fn status(mut self, status: u16) -> Response {
        self.status = status;
        self
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Content Too Large",
        422 => "Unprocessable Content",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "",
    }
}

fn write_response(w: &mut Conn, r: &Response, close: bool) -> io::Result<()> {
    let mut out = format!("HTTP/1.1 {} {}\r\nContent-Length: {}\r\n", r.status, reason(r.status), r.body.len());
    for (k, v) in &r.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    if close {
        out.push_str("Connection: close\r\n");
    }
    out.push_str("\r\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(&r.body);
    w.write_all(&bytes)?;
    w.flush()
}

/// What reading the next request found.
enum Next {
    Request(Request),
    /// The peer closed (or went idle) between requests.
    End,
    /// Answer this and close: the request could not be read through.
    Refuse(Response),
}

/// One line of a request head.
enum Line {
    Text(String),
    /// The connection ended (or went idle) before a line.
    End,
    /// Over the head budget, or not UTF-8.
    Bad,
}

fn read_line(r: &mut BufReader<Conn>, budget: &mut usize) -> io::Result<Line> {
    let mut line = Vec::new();
    let n = match r.by_ref().take(*budget as u64 + 1).read_until(b'\n', &mut line) {
        Ok(n) => n,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::UnexpectedEof
            ) =>
        {
            return Ok(Line::End);
        }
        Err(e) => return Err(e),
    };
    if n == 0 {
        return Ok(Line::End);
    }
    if n > *budget || !line.ends_with(b"\n") {
        return Ok(Line::Bad);
    }
    *budget -= n;
    line.truncate(line.len() - 1);
    if line.ends_with(b"\r") {
        line.pop();
    }
    Ok(String::from_utf8(line).map_or(Line::Bad, Line::Text))
}

fn bad_request() -> Response {
    Response::new(400).with("CAS-Reason", "request")
}

fn read_request(r: &mut BufReader<Conn>, max_body: usize) -> io::Result<Next> {
    let mut budget = MAX_HEAD;
    let line = match read_line(r, &mut budget)? {
        Line::Text(l) => l,
        Line::End => return Ok(Next::End),
        Line::Bad => return Ok(Next::Refuse(Response::new(431))),
    };
    let mut parts = line.split(' ');
    let (Some(method), Some(url), Some(version), None) = (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Ok(Next::Refuse(bad_request()));
    };
    if !version.starts_with("HTTP/1.") {
        return Ok(Next::Refuse(bad_request()));
    }
    let mut headers = Vec::new();
    loop {
        let h = match read_line(r, &mut budget)? {
            Line::Text(h) => h,
            Line::End => return Ok(Next::End),
            Line::Bad => return Ok(Next::Refuse(Response::new(431))),
        };
        if h.is_empty() {
            break;
        }
        let Some((k, v)) = h.split_once(':') else { return Ok(Next::Refuse(bad_request())) };
        headers.push((k.trim().to_string(), v.trim().to_string()));
    }
    let mut req = Request { method: method.to_string(), url: url.to_string(), headers, body: Vec::new() };
    if req.header("Transfer-Encoding").is_some() {
        // Clients send a Content-Length; chunked request bodies are not taken.
        return Ok(Next::Refuse(Response::new(411)));
    }
    let len = match req.header("Content-Length") {
        None => 0,
        Some(v) => match v.parse::<usize>() {
            Ok(n) => n,
            Err(_) => return Ok(Next::Refuse(bad_request())),
        },
    };
    if len > max_body {
        return Ok(Next::Refuse(
            Response::data(format!("request bodies are limited to {max_body} bytes"))
                .status(413)
                .with("CAS-Reason", "too-large"),
        ));
    }
    req.body = vec![0; len];
    r.read_exact(&mut req.body)?;
    Ok(Next::Request(req))
}

fn serve_connection(
    store: &Arc<CoreStore>,
    auth: &Arc<Authenticate>,
    tls: &Tls,
    tcp: TcpStream,
    max_body: usize,
) -> io::Result<()> {
    // Responses go out in one write each; Nagle would only delay them.
    tcp.set_nodelay(true)?;
    tcp.set_read_timeout(Some(IDLE))?;
    tcp.set_write_timeout(Some(IDLE))?;
    let conn = match &tls.0 {
        Some(config) => {
            let conn = ServerConnection::new(Arc::clone(config)).map_err(io::Error::other)?;
            Conn::Tls(Box::new(StreamOwned::new(conn, tcp)))
        }
        None => Conn::Plain(tcp),
    };
    let mut r = BufReader::new(conn);
    loop {
        match read_request(&mut r, max_body)? {
            Next::End => return Ok(()),
            Next::Refuse(resp) => {
                write_response(r.get_mut(), &resp, true)?;
                return Ok(());
            }
            Next::Request(req) => {
                let close = req.header("Connection").is_some_and(|v| v.eq_ignore_ascii_case("close"));
                let session = store.session(auth(&req));
                let resp = handle(&session, &req);
                write_response(r.get_mut(), &resp, close)?;
                if close {
                    r.get_mut().close();
                    return Ok(());
                }
            }
        }
    }
}

fn respond_error(e: Error) -> Response {
    match e {
        Error::Denied => Response::new(403),
        Error::Invalid(r) => Response::new(400).with("CAS-Reason", wire::invalid_reason(r)),
        Error::Refused { reason, message } => {
            Response::data(message.unwrap_or_default()).status(422).with("CAS-Reason", &reason)
        }
        Error::Conflict => Response::new(409),
        Error::Unavailable(m) => Response::data(m).status(503),
    }
}

/// A body of concatenated 32-byte chunk ids.
fn chunk_ids(body: &[u8]) -> Option<Vec<chunk::Id>> {
    if !body.len().is_multiple_of(32) {
        return None;
    }
    Some(body.chunks(32).map(|c| c.try_into().unwrap()).collect())
}

fn handle(store: &Session, req: &Request) -> Response {
    let (path, query) = req.url.split_once('?').unwrap_or((&req.url, ""));
    let method = req.method.as_str();

    if let Some(seg) = path.strip_prefix("/k/") {
        let key = match wire::decode_key(seg) {
            Ok(k) => k,
            Err(e) => return respond_error(e),
        };
        match method {
            "GET" => match store.read(&key) {
                Ok(Some(v)) => {
                    let cache = match classify(&key) {
                        Some(Parsed::Blob { .. }) => "private, max-age=31536000, immutable",
                        _ => "no-store",
                    };
                    Response::data(v).with("Cache-Control", cache)
                }
                Ok(None) => Response::new(404).with("Cache-Control", "no-store"),
                Err(e) => respond_error(e),
            },
            "PUT" => {
                let Some((expect, value)) = wire::decode_expect(&req.body) else { return bad_request() };
                let by_chunks = req.header("Content-Type") == Some(wire::WRITE_CHUNKS);
                if !by_chunks {
                    return match store.write(&key, value, &expect) {
                        Ok(()) => Response::new(204),
                        Err(e) => respond_error(e),
                    };
                }
                let Some(ids) = chunk_ids(value) else { return bad_request() };
                if !store.chunked() {
                    return Response::new(404);
                }
                match store.write_chunks(&key, &ids, &expect) {
                    Ok(Some(())) => Response::new(204),
                    Ok(None) => respond_error(Error::refused("chunks-missing", "upload the missing chunks first")),
                    Err(e) => respond_error(e),
                }
            }
            _ => Response::new(405),
        }
    } else if let Some(seg) = path.strip_prefix("/remove/") {
        if method != "POST" {
            return Response::new(405);
        }
        let key = match wire::decode_key(seg) {
            Ok(k) => k,
            Err(e) => return respond_error(e),
        };
        match wire::decode_expect(&req.body) {
            Some((expect, [])) => match store.remove(&key, &expect) {
                Ok(()) => Response::new(204),
                Err(e) => respond_error(e),
            },
            _ => bad_request(),
        }
    } else if path == "/chunks/missing" && method == "POST" {
        if !store.chunked() {
            return Response::new(404);
        }
        let Some(ids) = chunk_ids(&req.body) else { return bad_request() };
        match store.missing_chunks(&ids) {
            Ok(missing) => Response::data(missing.concat()),
            Err(e) => respond_error(e),
        }
    } else if let Some(seg) = path.strip_prefix("/chunk/") {
        if method != "PUT" {
            return Response::new(405);
        }
        if !store.chunked() {
            return Response::new(404);
        }
        let Some(id) = hex::decode(seg).ok().and_then(|b| chunk::Id::try_from(b).ok()) else {
            return bad_request();
        };
        match store.put_chunk(&id, &req.body) {
            Ok(()) => Response::new(204),
            Err(e) => respond_error(e),
        }
    } else if let Some(op) = path.strip_prefix("/batch/") {
        if method != "POST" {
            return Response::new(405);
        }
        match op {
            "has" => {
                let Some(keys) = wire::decode_keys(&req.body) else { return bad_request() };
                match store.has_many(&keys) {
                    Ok(has) => Response::data(has.into_iter().map(u8::from).collect::<Vec<u8>>()),
                    Err(e) => respond_error(e),
                }
            }
            "read" => {
                let Some(keys) = wire::decode_keys(&req.body) else { return bad_request() };
                Response::data(wire::encode_reads(&store.read_many(&keys))).with("Cache-Control", "no-store")
            }
            "stored" => {
                let Some(keys) = wire::decode_keys(&req.body) else { return bad_request() };
                Response::data(wire::encode_stored(&store.read_stored_many(&keys))).with("Cache-Control", "no-store")
            }
            "write" => {
                let Some(puts) = wire::decode_puts(&req.body) else { return bad_request() };
                match store.write_many(&puts) {
                    Ok(()) => Response::new(204),
                    Err((i, e)) => respond_error(e).with("CAS-Batch-Index", &i.to_string()),
                }
            }
            _ => Response::new(404),
        }
    } else if path == "/list" && method == "GET" {
        let mut span = Span::all();
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            // `limit` is only a paging hint; the store's policy sets the page size.
            let bound = match k {
                "from" => &mut span.from,
                "to" => &mut span.to,
                _ => continue,
            };
            match hex::decode(v) {
                Ok(b) => *bound = Some(b),
                Err(_) => return bad_request(),
            }
        }
        match store.list(&span) {
            Ok(page) => {
                let mut body = Vec::new();
                for k in &page.keys {
                    body.extend_from_slice(k.as_bytes());
                    body.push(b'\n');
                }
                let r = Response::data(body).with("Cache-Control", "no-store");
                match page.rest {
                    Some(rest) => r.with("CAS-Rest-From", &hex::encode(rest.from.unwrap_or_default())),
                    None => r,
                }
            }
            Err(e) => respond_error(e),
        }
    } else {
        Response::new(404)
    }
}
