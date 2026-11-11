//! Notes for an imported git history (spec/git-import.md): what a git
//! commit message says, put where cas keeps it. Each step gets step notes
//! (spec/formats/step-notes.md): the message on the step, its subject on
//! the lines it changed; and the
//! tips get annotated files whose every line names the commit that last
//! changed it (a forward blame, from the same diffs).

use crate::annotated::{self, FileDiff};
use crate::history::{History, NodeId};
use crate::{Node, Result, View, walk};
use cas_core::Store;
use cas_core::algo::Algorithm;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// Nodes whose diffs are computed together, in parallel.
const WINDOW: usize = 4096;

/// A note: one line, no control characters.
fn clean(s: &str) -> String {
    s.trim().chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// A commit message as one note: its non-blank lines joined by ` / `.
fn whole(message: &str) -> String {
    message.lines().map(clean).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" / ")
}

/// One note per line of a diff: the whole message on the header line (the
/// step's note), the subject on every `+` and `-` line, and on the `path`
/// line of a path with no such lines (binary, a directory, an empty file).
fn diff_notes<'a>(d: &[u8], subject: &'a str, message: &'a str) -> Vec<&'a str> {
    let ls = annotated::lines(d);
    let mut out = vec![""; ls.len()];
    out[0] = message;
    let mut path_line: Option<usize> = None;
    let mut lines_noted = false;
    let mut last = false;
    for (i, l) in ls.iter().enumerate().skip(1) {
        if l.starts_with(b"path ") {
            if let Some(p) = path_line.filter(|_| !lines_noted) {
                out[p] = subject;
            }
            (path_line, lines_noted) = (Some(i), false);
        }
        let changed = if *l == b"\\\n" { last } else { l.starts_with(b"+") || l.starts_with(b"-") };
        if changed {
            out[i] = subject;
            lines_noted = true;
        }
        last = changed;
    }
    if let Some(p) = path_line.filter(|_| !lines_noted) {
        out[p] = subject;
    }
    out
}

/// Per line of a file: the index of the node that last changed it.
type Blame = Arc<Vec<u32>>;
type State = HashMap<String, Blame>;

/// The new file's lines through one edge: `Some(node)` for a line the edge
/// keeps from the predecessor (whose blame is `old`), `None` for one it
/// changed.
fn through(old: Option<&Blame>, f: &FileDiff) -> Vec<Option<u32>> {
    let old: &[u32] = old.map_or(&[], |b| b.as_slice());
    let mut out = Vec::with_capacity(old.len());
    let mut i = 0;
    for (os, oc, _, _, ls) in &f.hunks {
        let start = if *oc == 0 { *os } else { os - 1 };
        while i < start {
            out.push(old.get(i).copied());
            i += 1;
        }
        for (m, _) in ls {
            match m {
                b' ' => {
                    out.push(old.get(i).copied());
                    i += 1;
                }
                b'-' => i += 1,
                _ => out.push(None),
            }
        }
    }
    out.extend(old.get(i..).unwrap_or(&[]).iter().map(|&n| Some(n)));
    out
}

/// What one node's diffs from its predecessors were: per predecessor, its
/// index and the parsed diff (none for a first node).
struct Edges {
    preds: Vec<(usize, Vec<FileDiff>)>,
    root: Vec<FileDiff>,
}

/// Annotate the steps of `h` and its tips. `order` lists the nodes with
/// predecessors first; `subject` is each node's commit message, `first` its
/// first git parent.
pub fn annotate(
    store: &dyn Store,
    alg: &'static Algorithm,
    h: &mut History,
    order: &[NodeId],
    subject: &HashMap<NodeId, String>,
    first: &HashMap<NodeId, NodeId>,
) -> Result<()> {
    let index: HashMap<&NodeId, usize> = order.iter().enumerate().map(|(i, n)| (n, i)).collect();
    let msg: Vec<&str> = order.iter().map(|n| subject.get(n).map_or("", String::as_str)).collect();
    let subj: Vec<String> = msg.iter().map(|m| clean(m.lines().next().unwrap_or(""))).collect();
    let msg: Vec<String> = msg.iter().map(|m| whole(m)).collect();
    // How many successors still need each node's blame.
    let mut waiting = vec![0usize; order.len()];
    for n in order {
        for p in h.nodes[n].after.keys() {
            waiting[index[p]] += 1;
        }
    }
    let mut states: HashMap<usize, State> = HashMap::new();
    let tips: Vec<usize> = (0..order.len()).filter(|&i| waiting[i] == 0).collect();
    let mut tip_states: HashMap<usize, State> = HashMap::new();
    for (w, window) in order.chunks(WINDOW).enumerate() {
        // The diffs of the window, in parallel; written as annotated
        // diffs, kept parsed for the blame.
        // Nodes are taken one at a time, as threads come free: a few
        // merges of translations cost seconds each.
        let next = std::sync::atomic::AtomicUsize::new(0);
        let slots: Vec<std::sync::Mutex<Option<Result<Done>>>> = window.iter().map(|_| Default::default()).collect();
        let h_ref = &*h;
        let threads = cas_core::threads::get().clamp(1, window.len());
        std::thread::scope(|sc| {
            for _ in 0..threads {
                sc.spawn(|| {
                    loop {
                        let k = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(n) = window.get(k) else { break };
                        let r = node_edges(store, alg, h_ref, n, &index, &subj, &msg, first);
                        *slots[k].lock().unwrap() = Some(r);
                    }
                });
            }
        });
        let done = slots.into_iter().map(|m| m.into_inner().unwrap().unwrap());
        let mut i = w * WINDOW;
        let mut puts = Vec::new();
        {
            for r in done {
                let (edges, notes, files, p) = r?;
                puts.extend(p);
                let n = &order[i];
                if !files.is_empty()
                    && let Some(x) = h.nodes.get_mut(n)
                {
                    x.notes = files;
                }
                for (p, v) in notes {
                    if let Some(step) = h.nodes.get_mut(n).and_then(|x| x.after.get_mut(&p)) {
                        step.notes = Some(v);
                    }
                }
                let state = blame(i, &edges, &mut states, &mut waiting);
                if waiting[i] == 0 {
                    tip_states.insert(i, state);
                } else {
                    states.insert(i, state);
                }
                i += 1;
            }
        }
        store.write_many(&puts).map_err(|(_, e)| crate::Error::from(e))?;
    }
    // The tips' annotated files: each line names the node that last
    // changed it and what its commit said.
    let note = |i: u32| {
        let i = i as usize;
        let k = order[i].version();
        let k = k.key();
        let short = match k.split_once(':') {
            Some((alg, d)) => format!("{alg}:{}", &d[..12.min(d.len())]),
            None => k.to_string(),
        };
        if subj[i].is_empty() { short } else { format!("{short} {}", subj[i]) }
    };
    for t in tips {
        let Some(state) = tip_states.remove(&t) else { continue };
        let id = order[t].clone();
        let mut files = std::collections::BTreeMap::new();
        for item in walk(store, &id.version(), None) {
            let (path, node) = item?;
            let (Node::File(v), Some(b)) = (node, state.get(path.as_str())) else { continue };
            let n = annotated::lines(&crate::read_blob(store, v.key())?).len();
            if n != b.len() || n == 0 {
                continue;
            }
            let notes: Vec<String> = b.iter().map(|&i| note(i)).collect();
            files.insert(path.to_string(), annotated::write(store, alg, &v, &notes)?);
        }
        if let Some(x) = h.nodes.get_mut(&id) {
            x.notes = files;
        }
    }
    Ok(())
}

fn parse(d: &[u8]) -> Result<Vec<FileDiff>> {
    annotated::parse_diff(d).map(|(_, _, f)| f).map_err(|e| crate::malformed("diff", e))
}

/// Node `i`'s blame, from its first predecessor's with what each edge
/// changed. A changed line keeps the blame of a predecessor that has it
/// unchanged (the first that does), and is otherwise `i`'s.
fn blame(i: usize, e: &Edges, states: &mut HashMap<usize, State>, waiting: &mut [usize]) -> State {
    let me = i as u32;
    let Some((first, files)) = e.preds.first() else {
        let mut s = State::new();
        for f in &e.root {
            if !f.binary && f.new != "-" && f.new != "dir" {
                let n = f.hunks.iter().map(|h| h.3).sum();
                s.insert(f.path.clone(), Arc::new(vec![me; n]));
            }
        }
        return s;
    };
    // Take the first predecessor's state, or a copy while others need it.
    waiting[*first] -= 1;
    let mut s = if waiting[*first] == 0 {
        states.remove(first).unwrap_or_default()
    } else {
        states.get(first).cloned().unwrap_or_default()
    };
    for f in files {
        if f.binary || f.new == "-" || f.new == "dir" {
            s.remove(&f.path);
            continue;
        }
        let mut lines = through(s.get(&f.path), f);
        for (p, others) in &e.preds[1..] {
            let Some(o) = others.iter().find(|g| g.path == f.path) else {
                // Unchanged from this predecessor: its blame, line for line.
                if let Some(b) = states.get(p).and_then(|st| st.get(&f.path))
                    && b.len() == lines.len()
                {
                    for (l, &x) in lines.iter_mut().zip(b.iter()) {
                        l.get_or_insert(x);
                    }
                }
                continue;
            };
            if o.binary {
                continue;
            }
            let theirs = through(states.get(p).and_then(|st| st.get(&f.path)), o);
            if theirs.len() == lines.len() {
                for (l, x) in lines.iter_mut().zip(theirs) {
                    if l.is_none() {
                        *l = x;
                    }
                }
            }
        }
        s.insert(f.path.clone(), Arc::new(lines.into_iter().map(|l| l.unwrap_or(me)).collect()));
    }
    for (p, _) in &e.preds[1..] {
        waiting[*p] -= 1;
        if waiting[*p] == 0 {
            states.remove(p);
        }
    }
    s
}

/// A node's edges, its steps' notes, and (a first node) its files' notes.
type Done = (Edges, Vec<(NodeId, View)>, BTreeMap<String, View>, Vec<cas_core::store::Put>);

/// Node `n`'s diffs from its predecessors, and the annotated diff of its
/// step from its first parent.
#[allow(clippy::too_many_arguments)]
fn node_edges(
    store: &dyn Store,
    alg: &'static Algorithm,
    h: &History,
    n: &NodeId,
    index: &HashMap<&NodeId, usize>,
    subj: &[String],
    msg: &[String],
    first: &HashMap<NodeId, NodeId>,
) -> Result<Done> {
    let node = &h.nodes[n];
    let (s, m) = (&subj[index[n]], &msg[index[n]]);
    let mut edges = Edges { preds: Vec::new(), root: Vec::new() };
    let mut notes = Vec::new();
    let mut files = BTreeMap::new();
    let mut puts = Vec::new();
    if node.after.is_empty() {
        let (d, lens) = annotated::diff_with_lens(store, None, &n.version())?;
        edges.root = parse(&d)?;
        // A first commit has no step: its subject goes on its files' lines.
        if !s.is_empty()
            && let Some(sn) = crate::stepnotes::from_lines(None, &n.version(), &d, &lens, &diff_notes(&d, s, m))
                .map_err(|e| crate::malformed("diff", e))?
        {
            files = crate::stepnotes::file_notes(store, alg, &sn)?;
        }
    }
    // The step from the first parent is the
    // commit's (git's own view of a merge); the
    // others count only for the blame of what
    // that step changed.
    if let Some(p) = first.get(n) {
        let (d, lens) = annotated::diff_with_lens(store, Some(&p.version()), &n.version())?;
        let files = parse(&d)?;
        for q in node.after.keys().filter(|q| *q != p) {
            let mut theirs = Vec::new();
            for f in &files {
                let Ok(new) = View::parse(&f.new) else { continue };
                let path = crate::Path::new(f.path.as_str())?;
                let old = crate::object_at(store, &q.version(), &path)?;
                if old.as_ref() == Some(&new) {
                    continue;
                }
                let old = old.filter(|v| !v.is_dir_view());
                theirs.push(annotated::file_diff(store, &f.path, old.as_ref(), &new)?.unwrap_or(FileDiff {
                    path: f.path.clone(),
                    old: String::new(),
                    new: f.new.clone(),
                    hunks: Vec::new(),
                    binary: true,
                }));
            }
            edges.preds.push((index[q], theirs));
        }
        edges.preds.insert(0, (index[p], files));
        if !s.is_empty() {
            let sn = crate::stepnotes::from_lines(Some(&p.version()), &n.version(), &d, &lens, &diff_notes(&d, s, m))
                .map_err(|e| crate::malformed("diff", e))?;
            // Written with the window's others in one batch: one write
            // each waits on the store's lock.
            if let Some(sn) = sn {
                let bytes = sn.encode();
                let key = alg.key(&bytes);
                notes.push((p.clone(), View::new(crate::by_alias("step-notes").unwrap().id(), key.clone())?));
                puts.push(cas_core::store::Put::blob(key, bytes));
            }
        }
    }
    Ok((edges, notes, files, puts))
}
