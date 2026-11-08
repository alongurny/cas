//! A [`Store`] that speaks the wire protocol to a remote server.

use crate::store::Store;
use crate::types::{Error, Expect, Invalid, ListPage, Result, Span};
use crate::wire;
use ureq::Agent;
use ureq::http::Response;
use ureq::tls::{PemItem, RootCerts, TlsConfig};

pub struct HttpStore {
    agent: Agent,
    base: String,
    token: Option<String>,
}

fn agent(roots: RootCerts) -> Agent {
    let tls = TlsConfig::builder().root_certs(roots).build();
    Agent::config_builder().http_status_as_error(false).https_only(true).tls_config(tls).build().into()
}

impl HttpStore {
    /// `base` is the server's URL, e.g. `https://cas.example:8443`. Only
    /// `https` is spoken: any other scheme is refused here, and the agent
    /// refuses plain-HTTP redirects. Server certificates are checked
    /// against the Web PKI roots; see [`HttpStore::trusting`].
    pub fn new(base: impl Into<String>) -> std::io::Result<HttpStore> {
        let base = base.into().trim_end_matches('/').to_string();
        if !base.starts_with("https://") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{base}: a store URL must start with https://"),
            ));
        }
        Ok(HttpStore { agent: agent(RootCerts::WebPki), base, token: None })
    }

    /// Trust only the CA certificates in `pem` (e.g. a private CA or a
    /// self-signed server certificate) instead of the Web PKI roots.
    pub fn trusting(mut self, pem: &[u8]) -> std::io::Result<HttpStore> {
        let mut certs = Vec::new();
        for item in ureq::tls::parse_pem(pem) {
            if let PemItem::Certificate(c) = item.map_err(std::io::Error::other)? {
                certs.push(c);
            }
        }
        if certs.is_empty() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "no certificate in the PEM data"));
        }
        self.agent = agent(RootCerts::Specific(std::sync::Arc::new(certs)));
        Ok(self)
    }

    /// Send `token` as `Authorization: Bearer <token>` (spec/access.md).
    pub fn with_token(mut self, token: impl Into<String>) -> HttpStore {
        self.token = Some(token.into());
        self
    }

    fn auth<B>(&self, r: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        match &self.token {
            Some(t) => r.header("Authorization", format!("Bearer {t}")),
            None => r,
        }
    }
}

fn transport(e: ureq::Error) -> Error {
    Error::Unavailable(e.to_string())
}

fn body(resp: &mut Response<ureq::Body>) -> Result<Vec<u8>> {
    resp.body_mut().with_config().limit(u64::MAX).read_to_vec().map_err(transport)
}

fn header(resp: &Response<ureq::Body>, name: &str) -> Option<String> {
    resp.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
}

/// Map a non-success status to its core outcome.
fn outcome(mut resp: Response<ureq::Body>) -> Error {
    let reason = header(&resp, "CAS-Reason");
    match resp.status().as_u16() {
        403 => Error::Denied,
        400 => match reason.as_deref().and_then(wire::parse_invalid_reason) {
            Some(r) => Error::Invalid(r),
            None => Error::Unavailable(format!("server rejected the request ({})", reason.unwrap_or_default())),
        },
        422 => {
            let message =
                body(&mut resp).ok().filter(|b| !b.is_empty()).map(|b| String::from_utf8_lossy(&b).into_owned());
            Error::Refused { reason: reason.unwrap_or_default(), message }
        }
        409 => Error::Conflict,
        // The server read no further than its request size bound (spec/wire.md).
        413 => {
            let message =
                body(&mut resp).ok().filter(|b| !b.is_empty()).map(|b| String::from_utf8_lossy(&b).into_owned());
            Error::Refused { reason: reason.unwrap_or_else(|| "too-large".into()), message }
        }
        503 => {
            Error::Unavailable(body(&mut resp).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default())
        }
        s => Error::Unavailable(format!("unexpected HTTP status {s}")),
    }
}

impl HttpStore {
    /// Upload one chunk (spec/wire.md, chunked transfer).
    pub fn put_chunk(&self, id: &crate::chunk::Id, bytes: &[u8]) -> Result<()> {
        let resp = self
            .auth(self.agent.put(format!("{}/chunk/{}", self.base, hex::encode(id))))
            .header("Content-Type", "application/octet-stream")
            .send(bytes)
            .map_err(transport)?;
        if resp.status().as_u16() == 204 { Ok(()) } else { Err(outcome(resp)) }
    }

    /// Write a big blob by sending only the chunks the server lacks
    /// (spec/wire.md, chunked transfer). `None` when the server does not
    /// take chunks, or a chunk went missing meanwhile: send it whole.
    fn write_chunks(&self, key: &str, value: &[u8], expect: &Expect) -> Result<Option<Result<()>>> {
        let pieces = crate::chunk::split(value);
        let ids: Vec<u8> = pieces.iter().flat_map(|(id, _)| *id).collect();
        let mut resp = self
            .auth(self.agent.post(format!("{}/chunks/missing", self.base)))
            .header("Content-Type", "application/octet-stream")
            .send(&ids[..])
            .map_err(transport)?;
        if resp.status().as_u16() != 200 {
            return Ok(None);
        }
        let missing: std::collections::HashSet<Vec<u8>> = body(&mut resp)?.chunks(32).map(<[u8]>::to_vec).collect();
        let mut sent = std::collections::HashSet::new();
        for (id, piece) in &pieces {
            if !missing.contains(&id[..]) || !sent.insert(*id) {
                continue;
            }
            match self.put_chunk(id, piece) {
                Ok(()) => {}
                Err(Error::Unavailable(e)) => return Err(Error::Unavailable(e)),
                Err(_) => return Ok(None),
            }
        }
        let mut payload = Vec::with_capacity(ids.len() + 9);
        wire::encode_expect(expect, &mut payload);
        payload.extend_from_slice(&ids);
        let resp = self
            .auth(self.agent.put(format!("{}/k/{}", self.base, wire::encode_key(key))))
            .header("Content-Type", wire::WRITE_CHUNKS)
            .send(&payload[..])
            .map_err(transport)?;
        match resp.status().as_u16() {
            204 => Ok(Some(Ok(()))),
            404 => Ok(None),
            _ => match outcome(resp) {
                Error::Refused { reason, .. } if reason == "chunks-missing" => Ok(None),
                e => Ok(Some(Err(e))),
            },
        }
    }
}

impl Store for HttpStore {
    fn read(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let mut resp = self
            .auth(self.agent.get(format!("{}/k/{}", self.base, wire::encode_key(key))))
            .call()
            .map_err(transport)?;
        match resp.status().as_u16() {
            200 => Ok(Some(body(&mut resp)?)),
            404 => Ok(None),
            _ => Err(outcome(resp)),
        }
    }

    fn write(&self, key: &str, value: &[u8], expect: &Expect) -> Result<()> {
        if value.len() > crate::chunk::CHUNK_OVER
            && matches!(crate::key::classify(key), Some(crate::key::Parsed::Blob { .. }))
            && let Some(done) = self.write_chunks(key, value, expect)?
        {
            return done;
        }
        let mut payload = Vec::with_capacity(value.len() + 9);
        wire::encode_expect(expect, &mut payload);
        payload.extend_from_slice(value);
        let resp = self
            .auth(self.agent.put(format!("{}/k/{}", self.base, wire::encode_key(key))))
            .header("Content-Type", "application/vnd.cas.write")
            .send(&payload[..])
            .map_err(transport)?;
        if resp.status().as_u16() == 204 { Ok(()) } else { Err(outcome(resp)) }
    }

    fn remove(&self, key: &str, expect: &Expect) -> Result<()> {
        let mut payload = Vec::new();
        wire::encode_expect(expect, &mut payload);
        let resp = self
            .auth(self.agent.post(format!("{}/remove/{}", self.base, wire::encode_key(key))))
            .header("Content-Type", "application/vnd.cas.remove")
            .send(&payload[..])
            .map_err(transport)?;
        if resp.status().as_u16() == 204 { Ok(()) } else { Err(outcome(resp)) }
    }

    fn list(&self, span: &Span) -> Result<ListPage> {
        let mut query = Vec::new();
        if let Some(f) = &span.from {
            query.push(format!("from={}", hex::encode(f)));
        }
        if let Some(t) = &span.to {
            query.push(format!("to={}", hex::encode(t)));
        }
        let mut resp =
            self.auth(self.agent.get(format!("{}/list?{}", self.base, query.join("&")))).call().map_err(transport)?;
        if resp.status().as_u16() != 200 {
            return Err(outcome(resp));
        }
        let rest = match header(&resp, "CAS-Rest-From") {
            Some(h) => Some(Span {
                from: Some(hex::decode(h).map_err(|_| Error::Unavailable("bad CAS-Rest-From".into()))?),
                to: span.to.clone(),
            }),
            None => None,
        };
        let text = String::from_utf8(body(&mut resp)?).map_err(|_| Error::Invalid(Invalid::Key))?;
        let keys = text.lines().map(str::to_string).collect();
        Ok(ListPage { keys, rest })
    }
}
