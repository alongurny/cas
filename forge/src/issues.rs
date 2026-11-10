//! Issues (spec/forge/issues.md): a history, one step per change.

use crate::accounts::Caller;
use crate::gate::as_actor;
use crate::names::{ProjectId, Role};
use crate::record::Doc;
use crate::{Error, Forge, Result, now};
use cas_version::history::{self, HistNode, Step};
use cas_version::uni::Value;
use cas_version::{Edit, Kind, Node, Path, View, build, by_alias, object_at, put, read_blob, update};
use serde_json::{Value as J, json};
use std::collections::BTreeMap;

fn read_doc(f: &Forge, v: &View) -> Result<Doc> {
    let text = String::from_utf8(read_blob(&*f.store, v.key())?).map_err(|_| Error::new(500, "not UTF-8"))?;
    Ok(Doc::of(&Value::from_text(&text).map_err(|e| Error::new(500, e))?))
}

/// Fields a change to an issue may set.
#[derive(Default, Debug)]
pub struct Change {
    pub title: Option<String>,
    pub body: Option<String>,
    pub state: Option<String>,
    pub add_labels: Vec<String>,
    pub remove_labels: Vec<String>,
    pub assignees: Option<Vec<String>>,
}

impl Forge {
    /// The tip version of the issues history (none yet: `None`).
    fn issues_version(&self, p: &ProjectId) -> Result<Option<View>> {
        let Some(v) = history::head(&*self.store, &p.issues())? else { return Ok(None) };
        let heads = cas_version::derived::heads(&*self.store, &v)?;
        Ok(heads.tips.keys().next().map(|t| t.version()))
    }

    /// Commit `edits` (made by `f` from the current version) to the issues
    /// history, retried while others commit first.
    pub(crate) fn commit_issues(
        &self,
        user: &str,
        p: &ProjectId,
        mut f: impl FnMut(Option<&View>) -> Result<BTreeMap<Path, Edit>>,
    ) -> Result<()> {
        let alg = self.alg;
        as_actor(Some(user), || {
            history::edit(&*self.store, alg, &p.issues(), |st, root| {
                let tip = match root.tips.len() {
                    0 => None,
                    1 => root.tips.first().cloned(),
                    _ => return Err(cas_version::Error::Diverged("the issues have several tips".into())),
                };
                let base = match &tip {
                    Some(t) => t.version(),
                    None => build(&*self.store, alg, by_alias("dir").unwrap(), &BTreeMap::new())?,
                };
                let edits = f(tip.as_ref().map(|_| &base)).map_err(|e| cas_version::Error::Io(e.message))?;
                let version = update(&*self.store, alg, &base, &edits)?;
                let id = st.new_id(&version)?;
                let after = tip.iter().map(|t| (t.clone(), Step::default())).collect();
                if let Some(t) = &tip {
                    root.tips.remove(t);
                }
                root.tips.insert(id.clone());
                Ok((BTreeMap::from([(id, Some(HistNode { after, doc: None, notes: BTreeMap::new() }))]), ()))
            })
        })
        .map_err(|e| match e {
            cas_version::Error::Io(m) => Error::bad(m),
            e => e.into(),
        })?;
        Ok(())
    }

    pub(crate) fn put_doc(&self, d: &Doc) -> Result<Node> {
        Ok(Node::File(put(&*self.store, self.alg, "uni-txt", d.text().as_bytes())?))
    }

    /// The issue's document at `version`.
    fn issue_at(&self, version: &View, n: u64) -> Result<Option<Doc>> {
        match object_at(&*self.store, version, &Path::new(format!("{n}/issue.uni"))?)? {
            Some(v) => Ok(Some(read_doc(self, &v)?)),
            None => Ok(None),
        }
    }

    /// Labels `labels` with the exclusive sets kept: of a set with rule
    /// `one`, the last named wins.
    fn labels_after(&self, p: &ProjectId, cur: &Doc, add: &[String], remove: &[String]) -> Result<Doc> {
        let s = self.settings(p)?.ok_or_else(|| Error::missing("no project"))?;
        let defs = s.doc("labels");
        let sets = s.doc("sets");
        let mut out = cur.clone();
        for l in remove {
            out.remove(l);
        }
        for l in add {
            let def = defs.doc(l);
            if def.0.is_empty() {
                return Err(Error::bad(format!("no label {l} in {p}")));
            }
            let set = def.s("set");
            if !set.is_empty() && sets.s(set) == "one" {
                let others: Vec<String> = out.0.keys().filter(|k| defs.doc(k).s("set") == set).cloned().collect();
                for o in others {
                    out.remove(&o);
                }
            }
            out.set(l, "");
        }
        Ok(out)
    }

    /// Open an issue; its number.
    #[allow(clippy::too_many_arguments)]
    pub fn open_issue(
        &self,
        c: &Caller,
        p: &ProjectId,
        title: &str,
        body: &str,
        labels: &[String],
        anchor: Option<Doc>,
        suggestion: Option<&str>,
    ) -> Result<u64> {
        let role = self.require(Some(c), p, Role::Read)?;
        if !c.may_write(p) && c.scope != crate::accounts::Scope::All {
            return Err(Error::denied("this token cannot change the project"));
        }
        if title.trim().is_empty() {
            return Err(Error::bad("an issue needs a title"));
        }
        if !labels.is_empty() && role < Role::Write {
            return Err(Error::denied("labels need the write role"));
        }
        let labels = self.labels_after(p, &Doc::new(), labels, &[])?;
        let n = self.take_number(p)?;
        let mut d = Doc::new();
        d.set("title", title.trim()).set("state", "open").set("author", &c.user);
        d.set_num("created", now()).set_num("updated", now()).set_opt("body", Some(body));
        d.set_doc("labels", labels);
        if let Some(a) = anchor {
            d.set_doc("anchor", a);
        }
        d.set_opt("suggestion", suggestion);
        let node = self.put_doc(&d)?;
        self.commit_issues(&c.user, p, |_| {
            Ok(BTreeMap::from([(Path::new(format!("{n}/issue.uni")).unwrap(), Edit::Set(node.clone()))]))
        })?;
        self.event("issue", Some(&c.user), |e| {
            e.set("project", p.to_string()).set_num("number", n).set("text", format!("opened: {}", title.trim()));
        });
        Ok(n)
    }

    /// Change issue `n`.
    pub fn edit_issue(&self, c: &Caller, p: &ProjectId, n: u64, ch: &Change) -> Result<()> {
        let role = self.require(Some(c), p, Role::Read)?;
        let mut what = Vec::new();
        self.commit_issues(&c.user, p, |cur| {
            let cur = cur.ok_or_else(|| Error::missing(format!("no issue {n}")))?;
            let mut d = self.issue_at(cur, n)?.ok_or_else(|| Error::missing(format!("no issue {n}")))?;
            let author = d.s("author") == c.user;
            if (ch.title.is_some() || ch.body.is_some() || ch.state.is_some()) && !author && role < Role::Maintain {
                return Err(Error::denied("only the author or a maintainer changes an issue"));
            }
            if (!ch.add_labels.is_empty() || !ch.remove_labels.is_empty() || ch.assignees.is_some())
                && role < Role::Write
            {
                return Err(Error::denied("labels and assignees need the write role"));
            }
            what.clear();
            if let Some(t) = &ch.title {
                d.set("title", t.trim());
                what.push("title".to_string());
            }
            if let Some(b) = &ch.body {
                d.set_opt("body", Some(b));
                what.push("body".to_string());
            }
            if let Some(s) = &ch.state {
                if !matches!(s.as_str(), "open" | "closed") {
                    return Err(Error::bad("state is open or closed"));
                }
                d.set("state", s.as_str());
                what.push(s.clone());
            }
            if !ch.add_labels.is_empty() || !ch.remove_labels.is_empty() {
                let l = self.labels_after(p, &d.doc("labels"), &ch.add_labels, &ch.remove_labels)?;
                d.set_doc("labels", l);
                what.push("labels".to_string());
            }
            if let Some(a) = &ch.assignees {
                let mut s = Doc::new();
                for u in a {
                    s.set(u, "");
                }
                d.set_doc("assignees", s);
                what.push("assignees".to_string());
            }
            d.set_num("updated", now());
            Ok(BTreeMap::from([(Path::new(format!("{n}/issue.uni"))?, Edit::Set(self.put_doc(&d)?))]))
        })?;
        self.event("issue", Some(&c.user), |e| {
            e.set("project", p.to_string()).set_num("number", n).set("text", what.join(", "));
        });
        Ok(())
    }

    pub fn comment_issue(&self, c: &Caller, p: &ProjectId, n: u64, text: &str) -> Result<u64> {
        self.require(Some(c), p, Role::Read)?;
        if text.trim().is_empty() {
            return Err(Error::bad("a comment needs text"));
        }
        let mut k = 0;
        self.commit_issues(&c.user, p, |cur| {
            let cur = cur.ok_or_else(|| Error::missing(format!("no issue {n}")))?;
            if self.issue_at(cur, n)?.is_none() {
                return Err(Error::missing(format!("no issue {n}")));
            }
            k = self.comments(cur, n)?.len() as u64 + 1;
            let mut d = Doc::new();
            d.set("author", &c.user).set_num("time", now()).set("text", text);
            Ok(BTreeMap::from([(Path::new(format!("{n}/c/{k}.uni"))?, Edit::Set(self.put_doc(&d)?))]))
        })?;
        self.event("comment", Some(&c.user), |e| {
            e.set("project", p.to_string()).set_num("number", n).set("text", text);
        });
        Ok(k)
    }

    fn comments(&self, version: &View, n: u64) -> Result<Vec<Doc>> {
        let Some(dir) = object_at(&*self.store, version, &Path::new(format!("{n}/c"))?)? else { return Ok(Vec::new()) };
        let Some(Kind::Dir(d)) = dir.known_format().map(|f| &f.kind) else { return Ok(Vec::new()) };
        let mut items = d.load(&*self.store, dir.key(), &read_blob(&*self.store, dir.key())?)?;
        items.sort_by_key(|it| it.name.trim_end_matches(".uni").parse::<u64>().unwrap_or(0));
        items.iter().map(|it| read_doc(self, &it.child)).collect()
    }

    /// The issues, newest first, in `state` (empty: all), with `label` (empty: any).
    pub fn issues(&self, p: &ProjectId, state: &str, label: &str) -> Result<Vec<J>> {
        let Some(v) = self.issues_version(p)? else { return Ok(Vec::new()) };
        let Some(Kind::Dir(d)) = v.known_format().map(|f| &f.kind) else { return Ok(Vec::new()) };
        let mut out = Vec::new();
        for it in d.load(&*self.store, v.key(), &read_blob(&*self.store, v.key())?)? {
            let Ok(n) = it.name.parse::<u64>() else { continue };
            let Some(doc) = self.issue_at(&v, n)? else { continue };
            if (!state.is_empty() && doc.s("state") != state)
                || (!label.is_empty() && doc.doc("labels").opt(label).is_none())
            {
                continue;
            }
            out.push((n, issue_json(n, &doc)));
        }
        out.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
        Ok(out.into_iter().map(|(_, j)| j).collect())
    }

    pub fn issue(&self, p: &ProjectId, n: u64) -> Result<J> {
        let v = self.issues_version(p)?.ok_or_else(|| Error::missing(format!("no issue {n}")))?;
        let doc = self.issue_at(&v, n)?.ok_or_else(|| Error::missing(format!("no issue {n}")))?;
        let mut j = issue_json(n, &doc);
        j["comments"] = json!(self.comments(&v, n)?.iter().map(Doc::json).collect::<Vec<_>>());
        Ok(j)
    }
}

fn issue_json(n: u64, d: &Doc) -> J {
    let mut j = d.json();
    j["number"] = json!(n.to_string());
    j["labels"] = json!(d.keys("labels"));
    j["assignees"] = json!(d.keys("assignees"));
    j
}
