//! What the tests share: a commit on a history in one call.
#![allow(dead_code)]

use cas_core::Store;
use cas_core::algo::Algorithm;
use cas_version::View;
use cas_version::commits::{self, Commit, Converter};

pub fn b3() -> &'static Algorithm {
    cas_core::algo::lookup("blake3").unwrap()
}

/// Commit `version` (a directory of any directory format) on history
/// `name`, after its tip, a second later; the new commit's key.
pub fn commit(s: &dyn Store, name: &str, version: &View) -> String {
    let parent = commits::tip(s, name).unwrap();
    let time = parent.as_ref().map_or(1, |p| commits::read(s, p).unwrap().time + 1);
    let c = Commit {
        version: Converter::new(s, b3()).directory(version).unwrap(),
        author: commits::put_text(s, b3(), "author", "Dana").unwrap(),
        notes: None,
        time,
        parents: parent.iter().map(|p| (p.clone(), None)).collect(),
    };
    let key = commits::write(s, b3(), &c).unwrap();
    commits::advance(s, name, parent.as_deref(), &key).unwrap();
    key
}

/// The version of history `name`'s newest commit.
pub fn tip_version(s: &dyn Store, name: &str) -> View {
    commits::read(s, &commits::tip(s, name).unwrap().unwrap()).unwrap().version()
}
