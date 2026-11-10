//! Step notes (spec/formats/step-notes.md): why each line of a step
//! changed, kept on the lines of the two versions' files, not on a stored
//! diff. Showing them computes the diff ([`annotated::diff`]) and puts each
//! note on its line; notes written on that form go back the same way.

use crate::annotated::{self, Lens, is_note, lines, parse_view, view_text};
use crate::{Result, View, by_alias, malformed, read_blob, write_blob};
use cas_core::Store;
use cas_core::algo::Algorithm;
use std::collections::BTreeMap;

const HEAD: &str = "step-notes ";

/// Runs over a file's lines: (count, note), adjacent notes different.
pub type Runs = Vec<(usize, String)>;

/// The notes on one path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PathNotes {
    pub note: String,
    pub old: Option<Runs>,
    pub new: Option<Runs>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepNotes {
    pub before: Option<View>,
    pub after: View,
    pub note: String,
    pub paths: BTreeMap<String, PathNotes>,
}

fn runs_of(notes: &[String]) -> Option<Runs> {
    if notes.iter().all(String::is_empty) {
        return None;
    }
    let mut runs: Runs = Vec::new();
    for n in notes {
        match runs.last_mut() {
            Some((c, last)) if last == n => *c += 1,
            _ => runs.push((1, n.clone())),
        }
    }
    Some(runs)
}

/// One note per line from runs.
pub fn expand(runs: &Runs) -> Vec<&str> {
    runs.iter().flat_map(|(n, s)| std::iter::repeat_n(s.as_str(), *n)).collect()
}

fn len(runs: &Runs) -> usize {
    runs.iter().map(|r| r.0).sum()
}

/// Path order: bytes, with `/` before any other byte.
fn path_key(s: &str) -> Vec<u8> {
    s.bytes().map(|c| if c == b'/' { 0 } else { c }).collect()
}

impl StepNotes {
    pub fn is_empty(&self) -> bool {
        self.note.is_empty() && self.paths.is_empty()
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = format!("{HEAD}{} {}\n", view_text(self.before.as_ref()), self.after);
        if !self.note.is_empty() {
            out.push_str(&format!("note {}\n", self.note));
        }
        let mut paths: Vec<_> = self.paths.iter().collect();
        paths.sort_by_key(|(p, _)| path_key(p));
        for (p, n) in paths {
            out.push_str(&format!("path {p}\n"));
            if !n.note.is_empty() {
                out.push_str(&format!("note {}\n", n.note));
            }
            for (side, runs) in [("old", &n.old), ("new", &n.new)] {
                if let Some(runs) = runs {
                    out.push_str(side);
                    out.push('\n');
                    for (c, s) in runs {
                        out.push_str(&format!("{c} {s}\n"));
                    }
                }
            }
        }
        out.into_bytes()
    }

    pub fn decode(b: &[u8]) -> std::result::Result<StepNotes, String> {
        let text = std::str::from_utf8(b).map_err(|_| "not UTF-8")?;
        let mut ls = text.split_inclusive('\n').map(|l| l.strip_suffix('\n').ok_or("the last line has no newline"));
        let head = ls.next().ok_or("empty")??;
        let rest = head.strip_prefix(HEAD).ok_or("the header is not `step-notes <view> <view>`")?;
        let parts: Vec<&str> = rest.split(' ').collect();
        let (before, after) = match parts.as_slice() {
            ["-", f, k] => (None, format!("{f} {k}")),
            [f1, k1, f2, k2] => (parse_view(&format!("{f1} {k1}"))?, format!("{f2} {k2}")),
            _ => return Err("the header is not `step-notes <view> <view>`".into()),
        };
        let after = parse_view(&after)?.ok_or("the new side is a version")?;
        let mut sn = StepNotes { before, after, note: String::new(), paths: BTreeMap::new() };
        let note = |s: &str| -> std::result::Result<String, String> {
            if s.is_empty() || !is_note(s) {
                return Err(format!("bad note {s:?}"));
            }
            Ok(s.to_string())
        };
        let mut ls = ls.peekable();
        if let Some(Ok(l)) = ls.peek()
            && let Some(n) = l.strip_prefix("note ")
        {
            sn.note = note(n)?;
            ls.next();
        }
        let mut last: Option<Vec<u8>> = None;
        while let Some(l) = ls.next() {
            let l = l?;
            let p = l.strip_prefix("path ").ok_or_else(|| format!("expected `path`: {l:?}"))?;
            crate::Path::new(p).map_err(|e| e.to_string())?;
            let k = path_key(p);
            if last.as_ref().is_some_and(|x| *x >= k) {
                return Err(format!("paths out of order at {p:?}"));
            }
            last = Some(k);
            let mut pn = PathNotes::default();
            if let Some(Ok(l)) = ls.peek()
                && let Some(n) = l.strip_prefix("note ")
            {
                pn.note = note(n)?;
                ls.next();
            }
            for side in ["old", "new"] {
                if ls.peek().is_some_and(|l| l.as_deref() == Ok(side)) {
                    ls.next();
                    let mut runs: Runs = Vec::new();
                    while let Some(Ok(l)) = ls.peek() {
                        let Some((c, s)) = l.split_once(' ') else { break };
                        if c.is_empty() || !c.bytes().all(|b| b.is_ascii_digit()) {
                            break;
                        }
                        if c.starts_with('0') {
                            return Err(format!("bad count {c:?}"));
                        }
                        if !is_note(s) {
                            return Err(format!("a note has a control character: {s:?}"));
                        }
                        if runs.last().is_some_and(|r| r.1 == s) {
                            return Err("adjacent runs have the same note".into());
                        }
                        runs.push((c.parse().map_err(|_| "count too large")?, s.to_string()));
                        ls.next();
                    }
                    if runs.iter().all(|r| r.1.is_empty()) {
                        return Err(format!("{p}: {side} has no note"));
                    }
                    if side == "old" { pn.old = Some(runs) } else { pn.new = Some(runs) }
                }
            }
            if pn == PathNotes::default() {
                return Err(format!("{p} has no note"));
            }
            sn.paths.insert(p.to_string(), pn);
        }
        if sn.is_empty() {
            return Err("no notes".into());
        }
        Ok(sn)
    }
}

/// The format's check of a blob's own bytes; [`verify`] checks the runs
/// against the files.
pub fn check(b: &[u8]) -> std::result::Result<(), String> {
    StepNotes::decode(b).map(|_| ())
}

/// The versions a step-notes blob names.
pub(crate) fn children(b: &[u8]) -> Vec<View> {
    StepNotes::decode(b).map(|s| s.before.into_iter().chain([s.after]).collect()).unwrap_or_default()
}

/// The text file at `path` in `version`: its lines' count.
fn file_lines(store: &dyn Store, version: Option<&View>, path: &str) -> Result<Option<usize>> {
    let Some(v) = version else { return Ok(None) };
    let Some(f) = crate::object_at(store, v, &crate::Path::new(path)?)? else { return Ok(None) };
    if f.is_dir_view() || !annotated::is_text_format(&f) {
        return Ok(None);
    }
    let b = read_blob(store, f.key())?;
    Ok((!b.contains(&0)).then(|| lines(&b).len()))
}

/// [`verify`] for many: files found through `memo`, their lines counted
/// by `count` (none: not a text file).
pub(crate) fn verify_with(
    store: &dyn Store,
    key: &str,
    memo: &mut crate::dir::FindMemo,
    count: &dyn Fn(&View) -> Result<Option<usize>>,
) -> Result<()> {
    let sn = StepNotes::decode(&read_blob(store, key)?).map_err(|e| malformed(key, e))?;
    for (p, n) in &sn.paths {
        for (version, runs) in [(sn.before.as_ref(), &n.old), (Some(&sn.after), &n.new)] {
            let (Some(runs), Some(version)) = (runs, version) else {
                if runs.is_some() {
                    return Err(malformed(key, format!("{p}: old runs in a step from nothing")));
                }
                continue;
            };
            let file = crate::tree::object_at_memo(store, version, &crate::Path::new(p.as_str())?, memo)?;
            let have = match file {
                Some(f) if !f.is_dir_view() && annotated::is_text_format(&f) => count(&f)?,
                _ => None,
            };
            if have != Some(len(runs)) {
                return Err(malformed(key, format!("{p}: runs for {} lines, the file has {have:?}", len(runs))));
            }
        }
    }
    Ok(())
}

/// The notes on the new files' lines as annotated files (path to view):
/// for a first node, which has no step to hold step notes.
pub fn file_notes(store: &dyn Store, alg: &'static Algorithm, sn: &StepNotes) -> Result<BTreeMap<String, View>> {
    let mut out = BTreeMap::new();
    for (p, n) in &sn.paths {
        let Some(runs) = &n.new else { continue };
        let Some(f) = crate::object_at(store, &sn.after, &crate::Path::new(p.as_str())?)? else { continue };
        let notes: Vec<String> = expand(runs).into_iter().map(String::from).collect();
        out.insert(p.clone(), annotated::write(store, alg, &f, &notes)?);
    }
    Ok(out)
}

/// Whether every path's runs cover its files' lines; the notes.
pub fn verify(store: &dyn Store, key: &str) -> Result<StepNotes> {
    let sn = StepNotes::decode(&read_blob(store, key)?).map_err(|e| malformed(key, e))?;
    for (p, n) in &sn.paths {
        for (version, runs) in [(sn.before.as_ref(), &n.old), (Some(&sn.after), &n.new)] {
            if let Some(runs) = runs {
                let have = file_lines(store, version, p)?;
                if have != Some(len(runs)) {
                    return Err(malformed(key, format!("{p}: runs for {} lines, the file has {have:?}", len(runs))));
                }
            }
        }
    }
    Ok(sn)
}

pub fn write(store: &dyn Store, alg: &'static Algorithm, sn: &StepNotes) -> Result<View> {
    let key = write_blob(store, alg, &sn.encode())?;
    Ok(View::new(by_alias("step-notes").unwrap().id(), key).unwrap())
}

/// The diff's lines, each as (path, side, index): what a note on it belongs
/// to. `side` is `'s'` for the step (the header), `'p'` for the path, `'o'`
/// and `'n'` for a line of the old and new file.
fn places(d: &[u8]) -> Vec<(String, u8, usize)> {
    let mut out = Vec::new();
    let (mut path, mut o, mut n) = (String::new(), 0, 0);
    let start = |a: usize, b: usize| if b == 0 { a } else { a - 1 };
    for (i, l) in lines(d).into_iter().enumerate() {
        let place = if i == 0 {
            (String::new(), b's', 0)
        } else if let Some(p) = l.strip_prefix(b"path ") {
            path = String::from_utf8_lossy(p.strip_suffix(b"\n").unwrap_or(p)).into_owned();
            (path.clone(), b'p', 0)
        } else if l.starts_with(b"@@ ") {
            let h = String::from_utf8_lossy(l);
            let nums: Vec<usize> = h
                .split(|c: char| !c.is_ascii_digit())
                .filter(|x| !x.is_empty())
                .filter_map(|x| x.parse().ok())
                .collect();
            if let [a, b, c, d] = nums[..] {
                (o, n) = (start(a, b), start(c, d));
            }
            (path.clone(), b'p', 0)
        } else if l == b"\\\n" {
            out.last().cloned().unwrap_or((path.clone(), b'p', 0))
        } else {
            match l.first() {
                Some(b' ') => {
                    o += 1;
                    n += 1;
                    (path.clone(), b'n', n - 1)
                }
                Some(b'-') => {
                    o += 1;
                    (path.clone(), b'o', o - 1)
                }
                Some(b'+') => {
                    n += 1;
                    (path.clone(), b'n', n - 1)
                }
                _ => (path.clone(), b'p', 0),
            }
        };
        out.push(place);
    }
    out
}

/// Step notes from a note on each line of the diff `d` from `before` to
/// `after` (`lens`: its text files' line counts). `None` when every note is
/// empty.
pub fn from_lines(
    before: Option<&View>,
    after: &View,
    d: &[u8],
    lens: &Lens,
    notes: &[String],
) -> std::result::Result<Option<StepNotes>, String> {
    let mut sn =
        StepNotes { before: before.cloned(), after: after.clone(), note: String::new(), paths: BTreeMap::new() };
    let mut sides: BTreeMap<(String, u8), Vec<String>> = BTreeMap::new();
    let mut path_notes: BTreeMap<String, String> = BTreeMap::new();
    for ((path, side, i), note) in places(d).into_iter().zip(notes) {
        if note.is_empty() {
            continue;
        }
        if !is_note(note) {
            return Err(format!("a note has a control character: {note:?}"));
        }
        match side {
            b's' => sn.note.clone_from(note),
            b'p' => {
                let p = path_notes.entry(path.clone()).or_default();
                if p.is_empty() {
                    p.clone_from(note);
                } else if p != note {
                    return Err(format!("{path}: two notes for the path: {p:?} and {note:?}"));
                }
            }
            _ => {
                let (o, n) = lens.get(&path).copied().unwrap_or_default();
                let total =
                    if side == b'o' { o } else { n }.ok_or_else(|| format!("{path}: a note on a line of no text"))?;
                let v = sides.entry((path.clone(), side)).or_insert_with(|| vec![String::new(); total]);
                *v.get_mut(i).ok_or_else(|| format!("{path}: line {} past the file's end", i + 1))? = note.clone();
            }
        }
    }
    for (p, n) in path_notes {
        sn.paths.entry(p).or_default().note = n;
    }
    for ((p, side), notes) in sides {
        let e = sn.paths.entry(p).or_default();
        if side == b'o' { e.old = runs_of(&notes) } else { e.new = runs_of(&notes) }
    }
    Ok((!sn.is_empty()).then_some(sn))
}

/// A note for each line of the diff from `sn`'s notes.
fn line_notes(sn: &StepNotes, d: &[u8]) -> Vec<String> {
    let expanded: BTreeMap<&str, (Vec<&str>, Vec<&str>)> = sn
        .paths
        .iter()
        .map(|(p, n)| {
            (
                p.as_str(),
                (n.old.as_ref().map(expand).unwrap_or_default(), n.new.as_ref().map(expand).unwrap_or_default()),
            )
        })
        .collect();
    places(d)
        .into_iter()
        .enumerate()
        .map(|(k, (path, side, i))| {
            let line = lines(d)[k];
            let e = expanded.get(path.as_str());
            match side {
                b's' => sn.note.clone(),
                // The path's note on its `path` line only.
                b'p' if line.starts_with(b"path ") => sn.paths.get(&path).map(|n| n.note.clone()).unwrap_or_default(),
                b'p' => String::new(),
                b'o' => e.and_then(|e| e.0.get(i)).unwrap_or(&"").to_string(),
                _ => e.and_then(|e| e.1.get(i)).unwrap_or(&"").to_string(),
            }
        })
        .collect()
}

/// The notes shown on their diff, in the authoring form.
pub fn show(store: &dyn Store, key: &str) -> Result<Vec<u8>> {
    let sn = verify(store, key)?;
    let d = annotated::diff(store, sn.before.as_ref(), &sn.after)?;
    let notes = line_notes(&sn, &d);
    Ok(annotated::render(&d, &notes.iter().map(String::as_str).collect::<Vec<_>>()))
}

/// Step notes from the authoring form of the diff from `before` to
/// `after`, which must be exactly that diff.
pub fn from_form(store: &dyn Store, before: Option<&View>, after: &View, form: &[u8]) -> Result<Option<StepNotes>> {
    let (text, notes) = annotated::parse(form).map_err(|e| malformed("notes", e))?;
    let (d, lens) = annotated::diff_with_lens(store, before, after)?;
    if text != d {
        return Err(malformed("notes", "its text is not the diff to commit; run status --diff again"));
    }
    from_lines(before, after, &d, &lens, &notes).map_err(|e| malformed("notes", e))
}

/// The notes `old` carried to a step from `before` to `after` (a rebased
/// step): each file line takes the note of a like line of the same file on
/// the same side, the path and step notes stay. `None` when none carries.
pub fn carry(
    store: &dyn Store,
    alg: &'static Algorithm,
    old: &View,
    before: Option<&View>,
    after: &View,
) -> Result<Option<View>> {
    let sn = verify(store, old.key())?;
    let text = |v: Option<&View>, p: &str| -> Result<Option<Vec<u8>>> {
        let Some(v) = v else { return Ok(None) };
        match crate::object_at(store, v, &crate::Path::new(p)?)? {
            Some(f) if !f.is_dir_view() && annotated::is_text_format(&f) => Ok(Some(read_blob(store, f.key())?)),
            _ => Ok(None),
        }
    };
    let mut out =
        StepNotes { before: before.cloned(), after: after.clone(), note: sn.note.clone(), paths: BTreeMap::new() };
    for (p, n) in &sn.paths {
        let mut pn = PathNotes { note: n.note.clone(), ..PathNotes::default() };
        for (from, to, runs, slot) in
            [(sn.before.as_ref(), before, &n.old, &mut pn.old), (Some(&sn.after), Some(after), &n.new, &mut pn.new)]
        {
            let (Some(runs), Some(was), Some(now)) = (runs, text(from, p)?, text(to, p)?) else { continue };
            let old_lines: Vec<(Vec<u8>, &str)> =
                lines(&was).into_iter().map(<[u8]>::to_vec).zip(expand(runs)).collect();
            let new_lines: Vec<Vec<u8>> = lines(&now).into_iter().map(<[u8]>::to_vec).collect();
            *slot = runs_of(&annotated::carry_notes(&old_lines, &new_lines));
        }
        if pn != PathNotes::default() {
            out.paths.insert(p.clone(), pn);
        }
    }
    if out.is_empty() { Ok(None) } else { write(store, alg, &out).map(Some) }
}
