//! In-memory backend. Volatile, so conformant only as a test or cache store.

use crate::backend::Backend;
use crate::types::{Expect, Span};
use std::collections::BTreeMap;
use std::io;
use std::ops::Bound;
use std::sync::Mutex;

#[derive(Default)]
pub struct MemBackend {
    slots: Mutex<BTreeMap<Vec<u8>, Vec<u8>>>,
}

impl MemBackend {
    pub fn new() -> MemBackend {
        MemBackend::default()
    }
}

impl Backend for MemBackend {
    fn get(&self, key: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(self.slots.lock().unwrap().get(key.as_bytes()).cloned())
    }

    fn put_blob(&self, key: &str, value: &[u8]) -> io::Result<()> {
        self.slots.lock().unwrap().entry(key.as_bytes().to_vec()).or_insert_with(|| value.to_vec());
        Ok(())
    }

    fn remove_blob(&self, key: &str) -> io::Result<()> {
        self.slots.lock().unwrap().remove(key.as_bytes());
        Ok(())
    }

    fn cas_var(&self, name: &str, expect: &Expect, new: Option<&[u8]>) -> io::Result<bool> {
        let mut slots = self.slots.lock().unwrap();
        if !expect.matches(slots.get(name.as_bytes()).map(Vec::as_slice)) {
            return Ok(false);
        }
        match new {
            Some(v) => slots.insert(name.as_bytes().to_vec(), v.to_vec()),
            None => slots.remove(name.as_bytes()),
        };
        Ok(true)
    }

    fn list(&self, span: &Span, limit: usize) -> io::Result<Vec<String>> {
        let slots = self.slots.lock().unwrap();
        let lo = span.from.as_ref().map_or(Bound::Unbounded, |f| Bound::Included(f.clone()));
        let hi = span.to.as_ref().map_or(Bound::Unbounded, |t| Bound::Excluded(t.clone()));
        if let (Bound::Included(f), Bound::Excluded(t)) = (&lo, &hi)
            && f >= t
        {
            return Ok(Vec::new());
        }
        Ok(slots
            .range((lo, hi))
            .take(limit)
            .map(|(k, _)| String::from_utf8(k.clone()).expect("keys are UTF-8"))
            .collect())
    }
}
