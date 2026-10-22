//! Serve a [`CoreStore`] over HTTP (spec/wire.md).

use crate::gate::Who;
use crate::key::{Parsed, classify};
use crate::store::{CoreStore, Store};
use crate::types::{Error, Span};
use crate::wire;
use std::sync::Arc;
use std::thread;
use tiny_http::{Header, Method, Request, Response};

/// Turns a request into the caller's identity. This is where the outer
/// protocol plugs in.
pub type Authenticate = dyn Fn(&Request) -> Who + Send + Sync;

pub struct Server {
    http: Arc<tiny_http::Server>,
}

impl Server {
    /// Listen on `addr` (e.g. `127.0.0.1:0`) with `workers` handler threads.
    pub fn start(store: Arc<CoreStore>, addr: &str, workers: usize, auth: Arc<Authenticate>) -> std::io::Result<Server> {
        let http = Arc::new(tiny_http::Server::http(addr).map_err(std::io::Error::other)?);
        for _ in 0..workers.max(1) {
            let (http, store, auth) = (Arc::clone(&http), Arc::clone(&store), Arc::clone(&auth));
            thread::spawn(move || {
                while let Ok(req) = http.recv() {
                    let who = auth(&req);
                    handle(&store.session(who), req);
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

fn handle(store: &dyn Store, mut req: Request) {
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
                let mut body = Vec::new();
                if req.as_reader().read_to_end(&mut body).is_err() {
                    return bad_request(req);
                }
                let Some((expect, value)) = wire::decode_expect(&body) else { return bad_request(req) };
                match store.write(&key, value, &expect) {
                    Ok(()) => drop(req.respond(empty(204))),
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
        let mut body = Vec::new();
        if req.as_reader().read_to_end(&mut body).is_err() {
            return bad_request(req);
        }
        match wire::decode_expect(&body) {
            Some((expect, [])) => match store.remove(&key, &expect) {
                Ok(()) => drop(req.respond(empty(204))),
                Err(e) => respond_error(req, e),
            },
            _ => bad_request(req),
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
