//! A [`Store`] that speaks the wire protocol to a remote server.

use crate::store::Store;
use crate::types::{Error, Expect, Invalid, ListPage, Result, Span};
use crate::wire;
use ureq::Agent;
use ureq::http::Response;

pub struct HttpStore {
    agent: Agent,
    base: String,
    token: Option<String>,
}

impl HttpStore {
    /// `base` is the server's URL, e.g. `http://127.0.0.1:8080`.
    pub fn new(base: impl Into<String>) -> HttpStore {
        let agent = Agent::config_builder().http_status_as_error(false).build().into();
        HttpStore { agent, base: base.into().trim_end_matches('/').to_string(), token: None }
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
        503 => {
            Error::Unavailable(body(&mut resp).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default())
        }
        s => Error::Unavailable(format!("unexpected HTTP status {s}")),
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
