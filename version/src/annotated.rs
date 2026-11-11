//! Annotated text (spec/formats/annotated.md): a note on every line of a
//! blob, in runs, and the line diff of a step (spec/formats/diff.md), whose
//! annotation is a step's notes: what a commit message said, line by line.
//!
//! People write notes in the authoring form: the text's lines, each after
//! `| `, and lines `# NOTE` that set the note of the lines after them (until
//! the next `#` line). A text line that had no newline is followed by a
//! line `\`.

use crate::{Change, Node, Result, View, by_alias, malformed, read_blob, write_blob};
use cas_core::Store;
use cas_core::algo::Algorithm;

const ANNOTATED: &str = "annotated ";
const DIFF: &str = "diff ";
/// Lines of context around a change in a diff.
pub const CONTEXT: usize = 3;

/// The lines of `b`: each piece up to and with its newline, and a last
/// piece without one if `b` does not end in a newline. No lines when empty.
pub fn lines(b: &[u8]) -> Vec<&[u8]> {
    b.split_inclusive(|&c| c == b'\n').collect()
}

pub(crate) fn is_note(s: &str) -> bool {
    !s.chars().any(char::is_control)
}

pub(crate) fn view_text(v: Option<&View>) -> String {
    v.map_or_else(|| "-".to_string(), View::to_string)
}

pub(crate) fn parse_view(s: &str) -> std::result::Result<Option<View>, String> {
    if s == "-" {
        return Ok(None);
    }
    View::parse(s).map(Some).map_err(|e| format!("bad view {s:?}: {e}"))
}

/// An annotated blob: the view it annotates, and its notes as runs of
/// (lines, note), adjacent runs with different notes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Annotated {
    pub target: View,
    pub runs: Vec<(usize, String)>,
}

impl Annotated {
    /// From one note per line (an empty note is no comment).
    pub fn of(target: View, notes: &[String]) -> Annotated {
        let mut runs: Vec<(usize, String)> = Vec::new();
        for n in notes {
            match runs.last_mut() {
                Some((c, last)) if last == n => *c += 1,
                _ => runs.push((1, n.clone())),
            }
        }
        Annotated { target, runs }
    }

    /// One note per line.
    pub fn notes(&self) -> Vec<&str> {
        self.runs.iter().flat_map(|(n, s)| std::iter::repeat_n(s.as_str(), *n)).collect()
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = format!("{ANNOTATED}{}\n", self.target);
        for (n, s) in &self.runs {
            out.push_str(&format!("{n} {s}\n"));
        }
        out.into_bytes()
    }

    pub fn decode(b: &[u8]) -> std::result::Result<Annotated, String> {
        let text = std::str::from_utf8(b).map_err(|_| "not UTF-8")?;
        let mut ls = text.split_inclusive('\n');
        let head = ls.next().and_then(|l| l.strip_suffix('\n')).ok_or("no header line")?;
        let target = parse_view(head.strip_prefix(ANNOTATED).ok_or("the header is not `annotated <view>`")?)?
            .ok_or("an annotation names a view")?;
        let mut runs: Vec<(usize, String)> = Vec::new();
        for l in ls {
            let l = l.strip_suffix('\n').ok_or("the last line has no newline")?;
            let (n, s) = l.split_once(' ').ok_or_else(|| format!("bad run {l:?}"))?;
            if n.starts_with('0') || n.is_empty() || !n.bytes().all(|c| c.is_ascii_digit()) {
                return Err(format!("bad count {n:?}"));
            }
            if !is_note(s) {
                return Err(format!("a note has a control character: {s:?}"));
            }
            if runs.last().is_some_and(|r| r.1 == s) {
                return Err("adjacent runs have the same note".into());
            }
            runs.push((n.parse().map_err(|_| "count too large")?, s.to_string()));
        }
        Ok(Annotated { target, runs })
    }

    /// Lines the notes cover.
    pub fn len(&self) -> usize {
        self.runs.iter().map(|r| r.0).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The `annotated` format's check of a blob's own bytes. That the runs
/// cover the target's lines exactly is checked with the target
/// ([`verify`]).
pub fn check(b: &[u8]) -> std::result::Result<(), String> {
    Annotated::decode(b).map(|_| ())
}

/// Whether the annotation at `key` covers its target's lines exactly.
pub fn verify(store: &dyn Store, key: &str) -> Result<Annotated> {
    let a = Annotated::decode(&read_blob(store, key)?).map_err(|e| malformed(key, e))?;
    let n = lines(&read_blob(store, a.target.key())?).len();
    if a.len() != n {
        return Err(malformed(key, format!("notes for {} lines, the text has {n}", a.len())));
    }
    Ok(a)
}

/// Write the annotation of `target` with one note per line; the view of it.
pub fn write(store: &dyn Store, alg: &'static Algorithm, target: &View, notes: &[String]) -> Result<View> {
    let n = lines(&read_blob(store, target.key())?).len();
    if notes.len() != n {
        return Err(malformed(target.key(), format!("{} notes for {n} lines", notes.len())));
    }
    if let Some(bad) = notes.iter().find(|s| !is_note(s)) {
        return Err(malformed(target.key(), format!("a note has a control character: {bad:?}")));
    }
    let key = write_blob(store, alg, &Annotated::of(target.clone(), notes).encode())?;
    Ok(View { format: by_alias("annotated").unwrap().id().to_string(), key })
}

/// The authoring form of `text` with one note per line.
pub fn render(text: &[u8], notes: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut last: Option<&str> = None;
    for (i, l) in lines(text).into_iter().enumerate() {
        let note = notes.get(i).copied().unwrap_or("");
        if last != Some(note) && (last.is_some() || !note.is_empty()) {
            out.push(b'#');
            if !note.is_empty() {
                out.push(b' ');
                out.extend_from_slice(note.as_bytes());
            }
            out.push(b'\n');
        }
        last = Some(note);
        out.extend_from_slice(b"| ");
        out.extend_from_slice(l);
        if !l.ends_with(b"\n") {
            out.extend_from_slice(b"\n\\\n");
        }
    }
    out
}

/// The text and one note per line from the authoring form.
pub fn parse(form: &[u8]) -> std::result::Result<(Vec<u8>, Vec<String>), String> {
    let mut text = Vec::new();
    let mut notes = Vec::new();
    let mut note = String::new();
    let mut no_newline = false;
    for (i, l) in lines(form).into_iter().enumerate() {
        let body = l.strip_suffix(b"\n").unwrap_or(l);
        if no_newline {
            return Err(format!("line {}: text after a line marked as the last", i + 1));
        }
        if let Some(t) = body.strip_prefix(b"| ").or_else(|| (body == b"|").then_some(&b""[..])) {
            text.extend_from_slice(t);
            text.push(b'\n');
            notes.push(note.clone());
        } else if let Some(n) = body.strip_prefix(b"#") {
            let n = std::str::from_utf8(n).map_err(|_| format!("line {}: a note is not UTF-8", i + 1))?;
            note = n.strip_prefix(' ').unwrap_or(n).trim_end().to_string();
            if !is_note(&note) {
                return Err(format!("line {}: a note has a control character", i + 1));
            }
        } else if body == b"\\" {
            if text.pop() != Some(b'\n') {
                return Err(format!("line {}: `\\` follows no text line", i + 1));
            }
            no_newline = true;
        } else if !body.iter().all(u8::is_ascii_whitespace) {
            return Err(format!("line {}: neither `| text` nor `# note`", i + 1));
        }
    }
    Ok((text, notes))
}

/// The authoring form of the annotation at `key`, with its target's text.
pub fn show(store: &dyn Store, key: &str) -> Result<Vec<u8>> {
    let a = verify(store, key)?;
    Ok(render(&read_blob(store, a.target.key())?, &a.notes()))
}

// The diff format.

/// The line diff from version `a` (none: from nothing) to version `b`.
pub fn diff(store: &dyn Store, a: Option<&View>, b: &View) -> Result<Vec<u8>> {
    diff_with_lens(store, a, b).map(|(d, _)| d)
}

/// The lines of a text file on each side of a diff, by path (none: no text
/// file there).
pub type Lens = std::collections::HashMap<String, (Option<usize>, Option<usize>)>;

/// [`diff`], with the line counts of the text files it compared.
pub fn diff_with_lens(store: &dyn Store, a: Option<&View>, b: &View) -> Result<(Vec<u8>, Lens)> {
    let mut lens = Lens::new();
    let changes: Vec<Change> = match a {
        Some(a) => crate::diff(store, a, b, None)?,
        None => crate::walk(store, b, None)
            .map(|r| r.map(|(path, node)| Change { path, old: None, new: Some(node) }))
            .collect::<Result<_>>()?,
    };
    let mut out = format!("{DIFF}{} {}\n", view_text(a), b).into_bytes();
    let file = |n: &Option<Node>| match n {
        Some(Node::File(v)) => Some(v.clone()),
        _ => None,
    };
    for c in changes {
        let (old, new) = (file(&c.old), file(&c.new));
        let dir = |n: &Option<Node>| matches!(n, Some(Node::Dir));
        if old.is_none() && new.is_none() && !(dir(&c.old) ^ dir(&c.new)) {
            continue;
        }
        let side = |n: &Option<Node>, v: &Option<View>| match (n, v) {
            (Some(Node::Dir), _) => "dir".to_string(),
            (_, v) => view_text(v.as_ref()),
        };
        out.extend_from_slice(
            format!("path {}\nold {}\nnew {}\n", c.path, side(&c.old, &old), side(&c.new, &new)).as_bytes(),
        );
        let text = |v: &Option<View>| -> Result<Option<Vec<u8>>> {
            match v {
                Some(v) if is_text_format(v) => {
                    let b = read_blob(store, v.key())?;
                    Ok((!b.contains(&0)).then_some(b))
                }
                Some(_) => Ok(None),
                None => Ok(Some(Vec::new())),
            }
        };
        if old.is_none() && new.is_none() {
            continue;
        }
        match (text(&old)?, text(&new)?) {
            (Some(x), Some(y)) => {
                let n = |v: &Option<View>, t: &[u8]| v.is_some().then(|| lines(t).len());
                lens.insert(c.path.to_string(), (n(&old, &x), n(&new, &y)));
                hunks(&mut out, &x, &y)
            }
            _ => out.extend_from_slice(b"binary\n"),
        }
    }
    Ok((out, lens))
}

/// Formats whose files a diff shows by lines.
pub(crate) fn is_text_format(v: &View) -> bool {
    v.known_format().is_some_and(|f| matches!(f.alias, "raw" | "exec"))
}

fn hunks(out: &mut Vec<u8>, a: &[u8], b: &[u8]) {
    use similar::{Algorithm, DiffTag, capture_diff_slices, group_diff_ops};
    let (x, y) = (lines(a), lines(b));
    // The common start and end are equal lines as Myers would find them
    // (it strips them first too): compared once here, so that the lines
    // between are the only ones numbered.
    let pre = x.iter().zip(&y).take_while(|(p, q)| p == q).count();
    let suf = x[pre..].iter().rev().zip(y[pre..].iter().rev()).take_while(|(p, q)| p == q).count();
    let (xm, ym) = (&x[pre..x.len() - suf], &y[pre..y.len() - suf]);
    // Myers compares lines many times: compare numbers, one per distinct line.
    let mut ids: std::collections::HashMap<&[u8], u32> = std::collections::HashMap::new();
    let mut id = |l| {
        let n = ids.len() as u32;
        *ids.entry(l).or_insert(n)
    };
    let xi: Vec<u32> = xm.iter().map(|&l| id(l)).collect();
    let yi: Vec<u32> = ym.iter().map(|&l| id(l)).collect();
    let mut ops = Vec::new();
    if pre > 0 {
        ops.push(similar::DiffOp::Equal { old_index: 0, new_index: 0, len: pre });
    }
    for op in capture_diff_slices(Algorithm::Myers, &xi, &yi) {
        let (tag, o, n) = op.as_tag_tuple();
        let (old_index, new_index) = (o.start + pre, n.start + pre);
        ops.push(match tag {
            DiffTag::Equal => similar::DiffOp::Equal { old_index, new_index, len: o.len() },
            DiffTag::Delete => similar::DiffOp::Delete { old_index, old_len: o.len(), new_index },
            DiffTag::Insert => similar::DiffOp::Insert { old_index, new_index, new_len: n.len() },
            DiffTag::Replace => similar::DiffOp::Replace { old_index, old_len: o.len(), new_index, new_len: n.len() },
        });
    }
    if suf > 0 {
        ops.push(similar::DiffOp::Equal { old_index: x.len() - suf, new_index: y.len() - suf, len: suf });
    }
    let mut shift: isize = 0;
    for group in group_diff_ops(ops, CONTEXT) {
        let Some(first) = group.first() else { continue };
        // New positions from old ones and the lengths, not the ops' new
        // indices: similar 2.7 can give a `Delete` after a `Replace` a stale
        // one. Every change is in some group, so `shift` counts them all.
        let os = first.old_range().start;
        let ns = os.checked_add_signed(shift).unwrap_or(0);
        let (ol, nl) = group.iter().fold((0, 0), |(a, b), op| (a + op.old_range().len(), b + op.new_range().len()));
        shift += nl as isize - ol as isize;
        let (o, n) = (os..os + ol, ns..ns + nl);
        let start = |r: &std::ops::Range<usize>| if r.is_empty() { r.start } else { r.start + 1 };
        out.extend_from_slice(format!("@@ -{},{} +{},{} @@\n", start(&o), o.len(), start(&n), n.len()).as_bytes());
        let mut put = |c: u8, l: &[u8]| {
            out.push(c);
            out.extend_from_slice(l);
            if !l.ends_with(b"\n") {
                out.extend_from_slice(b"\n\\\n");
            }
        };
        let mut j = ns;
        for op in group {
            let (tag, or, nr) = op.as_tag_tuple();
            let added = j..j + nr.len();
            j = added.end;
            match tag {
                DiffTag::Equal => or.clone().for_each(|i| put(b' ', x[i])),
                DiffTag::Delete => or.clone().for_each(|i| put(b'-', x[i])),
                DiffTag::Insert => added.for_each(|i| put(b'+', y[i])),
                DiffTag::Replace => {
                    or.clone().for_each(|i| put(b'-', x[i]));
                    added.for_each(|i| put(b'+', y[i]));
                }
            }
        }
    }
}

/// One file of a parsed diff.
#[derive(Debug, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub old: String,
    pub new: String,
    /// The hunks: (old start, old count, new start, new count, lines as
    /// (marker, line with its newline when it had one)).
    pub hunks: Vec<Hunk>,
    pub binary: bool,
}

pub type Hunk = (usize, usize, usize, usize, Vec<(u8, Vec<u8>)>);

/// A diff blob's two versions and files. Checks the structure: counts
/// match the lines, markers, the order of the parts.
pub fn parse_diff(b: &[u8]) -> std::result::Result<(Option<View>, View, Vec<FileDiff>), String> {
    let ls = lines(b);
    let mut it = ls.iter().peekable();
    let text_line = |l: &[u8]| -> std::result::Result<String, String> {
        let l = l.strip_suffix(b"\n").ok_or("the last line has no newline")?;
        String::from_utf8(l.to_vec()).map_err(|_| "a header line is not UTF-8".to_string())
    };
    let head = text_line(it.next().ok_or("empty")?)?;
    let rest = head.strip_prefix(DIFF).ok_or("the header is not `diff <view> <view>`")?;
    let parts: Vec<&str> = rest.split(' ').collect();
    let (a, b) = match parts.as_slice() {
        ["-", f, k] => (None, format!("{f} {k}")),
        [f1, k1, f2, k2] => (parse_view(&format!("{f1} {k1}"))?, format!("{f2} {k2}")),
        _ => return Err("the header is not `diff <view> <view>`".into()),
    };
    let b = parse_view(&b)?.ok_or("the new side is a version")?;
    let mut files: Vec<FileDiff> = Vec::new();
    while let Some(l) = it.next() {
        let l = text_line(l)?;
        let path = l.strip_prefix("path ").ok_or_else(|| format!("expected `path`: {l:?}"))?.to_string();
        if files.last().is_some_and(|f| !path_before(&f.path, &path)) {
            return Err(format!("paths out of order at {path:?}"));
        }
        let mut field = |name: &str| -> std::result::Result<String, String> {
            let l = text_line(it.next().ok_or("truncated")?)?;
            l.strip_prefix(name).map(String::from).ok_or_else(|| format!("expected `{name}`: {l:?}"))
        };
        let (old, new) = (field("old ")?, field("new ")?);
        let mut f = FileDiff { path, old, new, hunks: Vec::new(), binary: false };
        if it.peek().is_some_and(|l| **l == b"binary\n") {
            it.next();
            f.binary = true;
        }
        while it.peek().is_some_and(|l| l.starts_with(b"@@ ")) {
            let h = text_line(it.next().unwrap())?;
            let nums = h
                .strip_prefix("@@ -")
                .and_then(|s| s.strip_suffix(" @@"))
                .and_then(|s| s.split_once(" +"))
                .ok_or_else(|| format!("bad hunk header {h:?}"))?;
            let pair = |s: &str| -> std::result::Result<(usize, usize), String> {
                let (a, b) = s.split_once(',').ok_or("bad hunk range")?;
                Ok((a.parse().map_err(|_| "bad hunk range")?, b.parse().map_err(|_| "bad hunk range")?))
            };
            let ((os, oc), (ns, nc)) = (pair(nums.0)?, pair(nums.1)?);
            let (mut seen_o, mut seen_n) = (0, 0);
            let mut body = Vec::new();
            while seen_o < oc || seen_n < nc {
                let l = it.next().ok_or("a hunk is cut short")?;
                let (&m, rest) = l.split_first().ok_or("empty line")?;
                match m {
                    b' ' => {
                        seen_o += 1;
                        seen_n += 1
                    }
                    b'-' => seen_o += 1,
                    b'+' => seen_n += 1,
                    _ => return Err(format!("bad hunk line marker {:?}", m as char)),
                }
                let mut line = rest.to_vec();
                if it.peek().is_some_and(|l| **l == b"\\\n") {
                    it.next();
                    line.pop();
                }
                body.push((m, line));
            }
            if seen_o != oc || seen_n != nc {
                return Err("a hunk's counts do not match its lines".into());
            }
            f.hunks.push((os, oc, ns, nc, body));
        }
        files.push(f);
    }
    Ok((a, b, files))
}

/// Path order: bytes, with `/` before any other byte.
fn path_before(a: &str, b: &str) -> bool {
    let key = |s: &str| s.bytes().map(|c| if c == b'/' { 0 } else { c }).collect::<Vec<u8>>();
    key(a) < key(b)
}

/// The `diff` format's check of a blob's own bytes.
pub fn check_diff(b: &[u8]) -> std::result::Result<(), String> {
    parse_diff(b).map(|_| ())
}

/// Apply a file's hunks to `old`: the new text, or why they do not apply.
pub fn apply(old: &[u8], f: &FileDiff) -> std::result::Result<Vec<u8>, String> {
    let x = lines(old);
    let mut out = Vec::new();
    let mut at = 0;
    for (os, oc, _, _, body) in &f.hunks {
        let start = if *oc == 0 { *os } else { os - 1 };
        if start < at || start > x.len() {
            return Err("hunks out of order".into());
        }
        x[at..start].iter().for_each(|l| out.extend_from_slice(l));
        at = start;
        for (m, l) in body {
            match m {
                b' ' | b'-' => {
                    if x.get(at).copied() != Some(l.as_slice()) {
                        return Err(format!("line {} differs", at + 1));
                    }
                    if *m == b' ' {
                        out.extend_from_slice(l);
                    }
                    at += 1;
                }
                _ => out.extend_from_slice(l),
            }
        }
    }
    x[at..].iter().for_each(|l| out.extend_from_slice(l));
    Ok(out)
}

/// The children of an annotation (its target) or a diff (its versions).
pub(crate) fn children(alias: &str, b: &[u8]) -> Vec<View> {
    match alias {
        "annotated" => Annotated::decode(b).map(|a| vec![a.target]).unwrap_or_default(),
        "diff" => parse_diff(b).map(|(a, b, _)| a.into_iter().chain([b]).collect()).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Notes for `new`'s lines taken from `old`'s (both line lists): each new
/// line takes the note of an unused old line with the same `key`, in order.
pub(crate) fn carry_notes<'a>(old: &[(Vec<u8>, &'a str)], new: &[Vec<u8>]) -> Vec<String> {
    let mut by: std::collections::HashMap<&[u8], std::collections::VecDeque<&'a str>> = Default::default();
    for (k, n) in old {
        by.entry(k.as_slice()).or_default().push_back(n);
    }
    new.iter().map(|k| by.get_mut(k.as_slice()).and_then(|q| q.pop_front()).unwrap_or("").to_string()).collect()
}

/// Keys of a diff's lines for carrying notes: a line within its path, with
/// what names versions or line numbers reduced to its first word.
fn diff_keys(d: &[u8]) -> Vec<Vec<u8>> {
    let mut path: &[u8] = b"";
    lines(d)
        .into_iter()
        .map(|l| {
            if let Some(p) = l.strip_prefix(b"path ") {
                path = p;
            }
            let word = |w: &[u8]| l.starts_with(w);
            let line: &[u8] =
                if word(b"old ") || word(b"new ") || word(b"@@ ") || word(b"diff ") { &l[..3] } else { l };
            [path, b"\0", line].concat()
        })
        .collect()
}

/// The annotation `old` carried over to `target`, a text much like the one
/// `old` annotates (a rebased diff, a file after a merge): each line of
/// `target` takes the note of a like line of the old text, others none.
/// For a `diff` target, lines match within their path, and version and line
/// numbers are ignored. `None` when no note carries over.
pub fn carry(store: &dyn Store, alg: &'static Algorithm, old: &View, target: &View) -> Result<Option<View>> {
    let a = verify(store, old.key())?;
    let (old_text, new_text) = (read_blob(store, a.target.key())?, read_blob(store, target.key())?);
    let is_diff = |v: &View| v.known_format().is_some_and(|f| f.alias == "diff");
    let keys =
        |v: &View, b: &[u8]| if is_diff(v) { diff_keys(b) } else { lines(b).into_iter().map(<[u8]>::to_vec).collect() };
    let old_lines: Vec<(Vec<u8>, &str)> = keys(&a.target, &old_text).into_iter().zip(a.notes()).collect();
    let notes = carry_notes(&old_lines, &keys(target, &new_text));
    if notes.iter().all(String::is_empty) {
        return Ok(None);
    }
    write(store, alg, target, &notes).map(Some)
}
