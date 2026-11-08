//! Serve a [`CoreStore`] over HTTP (spec/wire.md).

use crate::chunk;
use crate::gate::Who;
use crate::key::{Parsed, classify};
use crate::store::{CoreStore, Session, Store};
use crate::types::{Error, Span};
use crate::wire;
use std::io::Read;
use std::sync::Arc;
use std::thread;
use tiny_http::{Header, Method, Request, Response};

/// Turns a request into the caller's identity. This is where the outer
/// protocol plugs in.
pub type Authenticate = dyn Fn(&Request) -> Who + Send + Sync;

/// The bearer token of a request, from `Authorization: Bearer <token>`.
pub fn bearer(req: &Request) -> Option<&str> {
    req.headers()
        .iter()
        .find(|h| h.field.equiv("Authorization"))
        .and_then(|h| h.value.as_str().strip_prefix("Bearer "))
        .map(str::trim)
}

/// The largest request body [`Server::start`] reads: an expect frame and a
/// value. A body is read before the gate can see it (the gate may look at the
/// value), so without a bound any caller could make each worker buffer
/// without end. Larger requests are answered 413 before they are buffered.
pub const DEFAULT_MAX_BODY: usize = 64 << 20;

pub struct Server {
    http: Arc<tiny_http::Server>,
}

impl Server {
    /// Listen on `addr` (e.g. `127.0.0.1:0`) with `workers` handler threads,
    /// reading request bodies of at most [`DEFAULT_MAX_BODY`] bytes.
    pub fn start(
        store: Arc<CoreStore>,
        addr: &str,
        workers: usize,
        auth: Arc<Authenticate>,
    ) -> std::io::Result<Server> {
        Server::start_with_max_body(store, addr, workers, auth, DEFAULT_MAX_BODY)
    }

    /// [`Server::start`], reading request bodies of at most `max_body` bytes.
    pub fn start_with_max_body(
        store: Arc<CoreStore>,
        addr: &str,
        workers: usize,
        auth: Arc<Authenticate>,
        max_body: usize,
    ) -> std::io::Result<Server> {
        let listener = std::net::TcpListener::bind(addr)?;
        no_delay(&listener)?;
        let http = Arc::new(tiny_http::Server::from_listener(listener, None).map_err(std::io::Error::other)?);
        for _ in 0..workers.max(1) {
            let (http, store, auth) = (Arc::clone(&http), Arc::clone(&store), Arc::clone(&auth));
            thread::spawn(move || {
                while let Ok(req) = http.recv() {
                    let who = auth(&req);
                    handle(&store.session(who), req, max_body);
                }
            });
        }
        Ok(Server { http })
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.http.server_addr().to_ip().expect("listening on TCP"))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.http.unblock();
    }
}

/// Turn off Nagle's algorithm on the listener, which accepted connections
/// inherit on Linux. tiny_http writes a response's head, body and chunk ends
/// separately; with Nagle each small write after the first waits for the
/// client's delayed ACK, about 40 ms per request on a kept-alive connection.
#[cfg(target_os = "linux")]
fn no_delay(l: &std::net::TcpListener) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let on: libc::c_int = 1;
    // SAFETY: a valid socket and a c_int option value of the given size.
    let r = unsafe {
        libc::setsockopt(
            l.as_raw_fd(),
            libc::IPPROTO_TCP,
            libc::TCP_NODELAY,
            (&on as *const libc::c_int).cast(),
            size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if r == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

#[cfg(not(target_os = "linux"))]
fn no_delay(_: &std::net::TcpListener) -> std::io::Result<()> {
    Ok(())
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

fn empty(status: u16) -> Response<std::io::Empty> {
    Response::empty(status)
}

fn respond_error(req: Request, e: Error) {
    let r = match e {
        Error::Denied => empty(403).boxed(),
        Error::Invalid(r) => empty(400).with_header(header("CAS-Reason", wire::invalid_reason(r))).boxed(),
        Error::Refused { reason, message } => Response::from_data(message.unwrap_or_default())
            .with_status_code(422)
            .with_header(header("CAS-Reason", &reason))
            .boxed(),
        Error::Conflict => empty(409).boxed(),
        Error::Unavailable(m) => Response::from_data(m).with_status_code(503).boxed(),
    };
    let _ = req.respond(r);
}

fn bad_request(req: Request) {
    let _ = req.respond(empty(400).with_header(header("CAS-Reason", "request")));
}

enum Body {
    Read(Vec<u8>),
    TooLarge,
    Broken,
}

/// The request body, refused unread when its declared length is over `max`,
/// and never buffered past `max + 1` bytes when it declares none.
fn read_body(req: &mut Request, max: usize) -> Body {
    if req.body_length().is_some_and(|n| n > max) {
        return Body::TooLarge;
    }
    let mut body = Vec::new();
    match req.as_reader().take(max as u64 + 1).read_to_end(&mut body) {
        Ok(_) if body.len() > max => Body::TooLarge,
        Ok(_) => Body::Read(body),
        Err(_) => Body::Broken,
    }
}

fn too_large(req: Request, max: usize) {
    let _ = req.respond(
        Response::from_data(format!("request bodies are limited to {max} bytes"))
            .with_status_code(413)
            .with_header(header("CAS-Reason", "too-large")),
    );
}

/// A body of concatenated 32-byte chunk ids.
fn chunk_ids(body: &[u8]) -> Option<Vec<chunk::Id>> {
    if !body.len().is_multiple_of(32) {
        return None;
    }
    Some(body.chunks(32).map(|c| c.try_into().unwrap()).collect())
}

fn handle(store: &Session, mut req: Request, max_body: usize) {
    let url = req.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let method = req.method().clone();

    if let Some(seg) = path.strip_prefix("/k/") {
        let key = match wire::decode_key(seg) {
            Ok(k) => k,
            Err(e) => return respond_error(req, e),
        };
        match method {
            Method::Get => match store.read(&key) {
                Ok(Some(v)) => {
                    let cache = match classify(&key) {
                        Some(Parsed::Blob { .. }) => "private, max-age=31536000, immutable",
                        _ => "no-store",
                    };
                    let _ = req.respond(Response::from_data(v).with_header(header("Cache-Control", cache)));
                }
                Ok(None) => {
                    let _ = req.respond(empty(404).with_header(header("Cache-Control", "no-store")));
                }
                Err(e) => respond_error(req, e),
            },
            Method::Put => {
                let body = match read_body(&mut req, max_body) {
                    Body::Read(body) => body,
                    Body::TooLarge => return too_large(req, max_body),
                    Body::Broken => return bad_request(req),
                };
                let Some((expect, value)) = wire::decode_expect(&body) else { return bad_request(req) };
                let by_chunks = req
                    .headers()
                    .iter()
                    .any(|h| h.field.equiv("Content-Type") && h.value.as_str() == wire::WRITE_CHUNKS);
                if !by_chunks {
                    return match store.write(&key, value, &expect) {
                        Ok(()) => drop(req.respond(empty(204))),
                        Err(e) => respond_error(req, e),
                    };
                }
                let Some(ids) = chunk_ids(value) else { return bad_request(req) };
                if !store.chunked() {
                    return drop(req.respond(empty(404)));
                }
                match store.write_chunks(&key, &ids, &expect) {
                    Ok(Some(())) => drop(req.respond(empty(204))),
                    Ok(None) => respond_error(req, Error::refused("chunks-missing", "upload the missing chunks first")),
                    Err(e) => respond_error(req, e),
                }
            }
            _ => drop(req.respond(empty(405))),
        }
    } else if let Some(seg) = path.strip_prefix("/remove/") {
        if method != Method::Post {
            return drop(req.respond(empty(405)));
        }
        let key = match wire::decode_key(seg) {
            Ok(k) => k,
            Err(e) => return respond_error(req, e),
        };
        let body = match read_body(&mut req, max_body) {
            Body::Read(body) => body,
            Body::TooLarge => return too_large(req, max_body),
            Body::Broken => return bad_request(req),
        };
        match wire::decode_expect(&body) {
            Some((expect, [])) => match store.remove(&key, &expect) {
                Ok(()) => drop(req.respond(empty(204))),
                Err(e) => respond_error(req, e),
            },
            _ => bad_request(req),
        }
    } else if path == "/chunks/missing" && method == Method::Post {
        if !store.chunked() {
            return drop(req.respond(empty(404)));
        }
        let body = match read_body(&mut req, max_body) {
            Body::Read(body) => body,
            Body::TooLarge => return too_large(req, max_body),
            Body::Broken => return bad_request(req),
        };
        let Some(ids) = chunk_ids(&body) else { return bad_request(req) };
        match store.missing_chunks(&ids) {
            Ok(missing) => drop(req.respond(Response::from_data(missing.concat()))),
            Err(e) => respond_error(req, e),
        }
    } else if let Some(seg) = path.strip_prefix("/chunk/") {
        if method != Method::Put {
            return drop(req.respond(empty(405)));
        }
        if !store.chunked() {
            return drop(req.respond(empty(404)));
        }
        let Some(id) = hex::decode(seg).ok().and_then(|b| chunk::Id::try_from(b).ok()) else {
            return bad_request(req);
        };
        let body = match read_body(&mut req, max_body) {
            Body::Read(body) => body,
            Body::TooLarge => return too_large(req, max_body),
            Body::Broken => return bad_request(req),
        };
        match store.put_chunk(&id, &body) {
            Ok(()) => drop(req.respond(empty(204))),
            Err(e) => respond_error(req, e),
        }
    } else if path == "/list" && method == Method::Get {
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
                Err(_) => return bad_request(req),
            }
        }
        match store.list(&span) {
            Ok(page) => {
                let mut body = Vec::new();
                for k in &page.keys {
                    body.extend_from_slice(k.as_bytes());
                    body.push(b'\n');
                }
                let mut r = Response::from_data(body).with_header(header("Cache-Control", "no-store"));
                if let Some(rest) = page.rest {
                    r.add_header(header("CAS-Rest-From", &hex::encode(rest.from.unwrap_or_default())));
                }
                let _ = req.respond(r);
            }
            Err(e) => respond_error(req, e),
        }
    } else {
        let _ = req.respond(empty(404));
    }
}
