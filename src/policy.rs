//! Store policy: what this store is willing to do for a permitted, well-formed
//! op. Breaking it yields `Error::Refused` with a reason.

use std::collections::BTreeSet;

#[derive(Clone, Debug)]
pub struct Policy {
    /// Registry names this store accepts.
    pub algorithms: BTreeSet<String>,
    pub max_key_len: Option<usize>,
    pub max_blob_len: Option<usize>,
    pub max_tag_len: Option<usize>,
    /// Most keys returned in one `list` answer. Paging is an implementation
    /// detail; callers follow `ListPage::rest`.
    pub list_page: usize,
}

impl Policy {
    /// Every registered algorithm, no size limits.
    pub fn permissive() -> Policy {
        Policy {
            algorithms: crate::algo::REGISTRY.iter().map(|a| a.name.to_string()).collect(),
            max_key_len: None,
            max_blob_len: None,
            max_tag_len: None,
            list_page: 1000,
        }
    }

    pub fn with_algorithms(mut self, names: &[&str]) -> Policy {
        self.algorithms = names.iter().map(|n| n.to_string()).collect();
        self
    }
}

impl Default for Policy {
    fn default() -> Policy {
        Policy::permissive()
    }
}
