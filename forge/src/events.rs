//! The event log (spec/forge/events.md): segments of at most 256 events
//! under `forge/events`, served as server-sent events and sent to webhooks.

use crate::names::{EVENTS, ProjectId};
use crate::record::{self, Doc, Record};
use crate::{Forge, Result, now};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

pub const SEGMENT: usize = 256;

/// Wakes the streams waiting for events.
#[derive(Default)]
pub struct Hub {
    last: Mutex<u64>,
    cond: Condvar,
}

impl Hub {
    fn bump(&self, n: u64) {
        let mut l = self.last.lock().unwrap();
        if n > *l {
            *l = n;
        }
        self.cond.notify_all();
    }

    /// Wait until an event after `after` exists, up to `timeout`; the last number.
    pub fn wait(&self, after: u64, timeout: Duration) -> u64 {
        let l = self.last.lock().unwrap();
        let (l, _) = self.cond.wait_timeout_while(l, timeout, |l| *l <= after).unwrap();
        *l
    }
}

impl Forge {
    /// Append an event of `kind` by `actor`; `f` fills in its fields. A
    /// failure to log is reported, not returned: what happened stands.
    pub fn event(&self, kind: &str, actor: Option<&str>, f: impl Fn(&mut Doc)) {
        let mut e = Doc::new();
        e.set_num("time", now()).set("kind", kind).set("actor", actor.unwrap_or(""));
        f(&mut e);
        match self.append(&e) {
            Ok(n) => {
                self.events.bump(n);
                e.set_num("number", n);
                self.deliver(&e);
            }
            Err(err) => eprintln!("event log: {err}"),
        }
    }

    fn append(&self, e: &Doc) -> Result<u64> {
        record::update(&*self.store, self.alg, EVENTS, |cur| {
            let (n, r) = match cur {
                None => {
                    let mut d = Doc::new();
                    d.set_num("first", 1);
                    d.push("events", e.clone());
                    (1, Record::new(d))
                }
                Some(r) if r.doc.doc("events").0.len() >= SEGMENT => {
                    let first = r.doc.num("first") + SEGMENT as u64;
                    let prev = r.write(&*self.store, self.alg)?;
                    let mut d = Doc::new();
                    d.set_num("first", first);
                    d.push("events", e.clone());
                    let mut nr = Record::new(d);
                    nr.views.insert("prev".into(), prev);
                    (first, nr)
                }
                Some(mut r) => {
                    let k = r.doc.push("events", e.clone());
                    (r.doc.num("first") + k - 1, r)
                }
            };
            Ok((Some(r), n))
        })
    }

    /// The events numbered above `after`, oldest first, at most `limit`.
    pub fn events_after(&self, after: u64, limit: usize) -> Result<Vec<Doc>> {
        let mut segs = Vec::new();
        let mut cur = record::load(&*self.store, EVENTS)?.map(|(_, r)| r);
        while let Some(r) = cur {
            let first = r.doc.num("first");
            let prev = r.views.get("prev").cloned();
            let reach_back = first > after + 1;
            segs.push(r);
            cur = match (reach_back, prev) {
                (true, Some(p)) => Some(Record::read(&*self.store, &p)?),
                _ => None,
            };
        }
        let mut out = Vec::new();
        for r in segs.iter().rev() {
            let first = r.doc.num("first");
            for (k, mut e) in r.doc.entries("events") {
                let n = first + k.parse::<u64>().unwrap_or(1) - 1;
                if n > after {
                    e.set_num("number", n);
                    out.push(e);
                    if out.len() >= limit {
                        return Ok(out);
                    }
                }
            }
        }
        Ok(out)
    }

    /// The number of the newest event.
    pub fn last_event(&self) -> Result<u64> {
        Ok(match record::load(&*self.store, EVENTS)? {
            None => 0,
            Some((_, r)) => r.doc.num("first") + r.doc.doc("events").0.len() as u64 - 1,
        })
    }

    /// Send `e` to the webhooks of its project that take its kind, on a
    /// thread of its own: three tries, a second and then four apart.
    fn deliver(&self, e: &Doc) {
        let Some(p) = e.opt("project").and_then(|p| ProjectId::parse(p).ok()) else { return };
        let Ok(Some(s)) = self.settings(&p) else { return };
        let body = serde_json::to_vec(&e.json()).unwrap_or_default();
        for (_, h) in s.entries("webhooks") {
            let events = h.s("events");
            if !(events.is_empty() || events == "*" || events.split(',').any(|k| k.trim() == e.s("kind"))) {
                continue;
            }
            let (url, sig, body) = (h.s("url").to_string(), signature(h.s("secret").as_bytes(), &body), body.clone());
            std::thread::spawn(move || {
                for wait in [0, 1, 4] {
                    std::thread::sleep(Duration::from_secs(wait));
                    let r = ureq::post(&url)
                        .header("Content-Type", "application/json")
                        .header("X-Cas-Signature", &format!("sha256={sig}"))
                        .send(&body[..]);
                    if r.is_ok() {
                        return;
                    }
                }
                eprintln!("webhook {url}: gave up after three tries");
            });
        }
    }
}

/// HMAC-SHA256 of `body` under `key`, as hex.
pub fn signature(key: &[u8], body: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let pad = |b: u8| k.iter().map(|x| x ^ b).collect::<Vec<u8>>();
    let inner = Sha256::new().chain_update(pad(0x36)).chain_update(body).finalize();
    hex::encode(Sha256::new().chain_update(pad(0x5c)).chain_update(inner).finalize())
}

#[cfg(test)]
mod tests {
    #[test]
    fn hmac_rfc4231_case_2() {
        assert_eq!(
            super::signature(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }
}
