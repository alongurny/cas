//! Step notes as they are written and shown: why each line of a step
//! changed, kept on the lines of the two versions' files (a commit stores
//! them as an annotation, `commits::write_notes`). Showing them computes the diff ([`annotated::diff`]) and puts each
//! note on its line; notes written on that form go back the same way.

use crate::annotated::{self, Lens, is_note, lines};
use crate::{Result, View, malformed};
use cas_core::Store;
use std::collections::BTreeMap;

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

pub fn runs_of<N: AsRef<str>>(notes: &[N]) -> Option<Runs> {
    if notes.iter().all(|n| n.as_ref().is_empty()) {
        return None;
    }
    let mut runs: Runs = Vec::new();
    for n in notes {
        let n = n.as_ref();
        match runs.last_mut() {
            Some((c, last)) if last == n => *c += 1,
            _ => runs.push((1, n.to_string())),
        }
    }
    Some(runs)
}

/// One note per line from runs.
pub fn expand(runs: &Runs) -> Vec<&str> {
    runs.iter().flat_map(|(n, s)| std::iter::repeat_n(s.as_str(), *n)).collect()
}

impl StepNotes {
    pub fn is_empty(&self) -> bool {
        self.note.is_empty() && self.paths.is_empty()
    }
}

/// The diff's lines, each as (path, side, index): what a note on it belongs
/// to. `path` indexes the returned paths (0: none). `side` is `'s'` for the
/// step (the header), `'P'` for a `path` line, `'p'` for the path's other
/// header lines, `'o'` and `'n'` for a line of the old and new file.
fn places(d: &[u8]) -> (Vec<String>, Vec<(u32, u8, usize)>) {
    let mut paths = vec![String::new()];
    let mut out: Vec<(u32, u8, usize)> = Vec::new();
    let (mut o, mut n) = (0, 0);
    let start = |a: usize, b: usize| if b == 0 { a } else { a - 1 };
    for (i, l) in lines(d).into_iter().enumerate() {
        let path = paths.len() as u32 - 1;
        let place = if i == 0 {
            (0, b's', 0)
        } else if let Some(p) = l.strip_prefix(b"path ") {
            paths.push(String::from_utf8_lossy(p.strip_suffix(b"\n").unwrap_or(p)).into_owned());
            (path + 1, b'P', 0)
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
            (path, b'p', 0)
        } else if l == b"\\\n" {
            out.last().copied().unwrap_or((path, b'p', 0))
        } else {
            match l.first() {
                Some(b' ') => {
                    o += 1;
                    n += 1;
                    (path, b'n', n - 1)
                }
                Some(b'-') => {
                    o += 1;
                    (path, b'o', o - 1)
                }
                Some(b'+') => {
                    n += 1;
                    (path, b'n', n - 1)
                }
                _ => (path, b'p', 0),
            }
        };
        out.push(place);
    }
    (paths, out)
}

/// Step notes from a note on each line of the diff `d` from `before` to
/// `after` (`lens`: its text files' line counts). `None` when every note is
/// empty.
pub fn from_lines<N: AsRef<str>>(
    before: Option<&View>,
    after: &View,
    d: &[u8],
    lens: &Lens,
    notes: &[N],
) -> std::result::Result<Option<StepNotes>, String> {
    let mut sn =
        StepNotes { before: before.cloned(), after: after.clone(), note: String::new(), paths: BTreeMap::new() };
    let mut sides: BTreeMap<(u32, u8), Vec<&str>> = BTreeMap::new();
    let mut path_notes: BTreeMap<u32, &str> = BTreeMap::new();
    let (paths, places) = places(d);
    for ((path, side, i), note) in places.into_iter().zip(notes) {
        let note = note.as_ref();
        if note.is_empty() {
            continue;
        }
        if !is_note(note) {
            return Err(format!("a note has a control character: {note:?}"));
        }
        let name = &paths[path as usize];
        match side {
            b's' => note.clone_into(&mut sn.note),
            b'p' | b'P' => {
                let p = path_notes.entry(path).or_default();
                if p.is_empty() {
                    *p = note;
                } else if *p != note {
                    return Err(format!("{name}: two notes for the path: {p:?} and {note:?}"));
                }
            }
            _ => {
                let (o, n) = lens.get(name).copied().unwrap_or_default();
                let total =
                    if side == b'o' { o } else { n }.ok_or_else(|| format!("{name}: a note on a line of no text"))?;
                let v = sides.entry((path, side)).or_insert_with(|| vec![""; total]);
                *v.get_mut(i).ok_or_else(|| format!("{name}: line {} past the file's end", i + 1))? = note;
            }
        }
    }
    for (p, n) in path_notes {
        sn.paths.entry(paths[p as usize].clone()).or_default().note = n.to_string();
    }
    for ((p, side), notes) in sides {
        let e = sn.paths.entry(paths[p as usize].clone()).or_default();
        if side == b'o' { e.old = runs_of(&notes) } else { e.new = runs_of(&notes) }
    }
    Ok((!sn.is_empty()).then_some(sn))
}

/// A path's notes with its runs spelled out, a note per line.
type Expanded<'a> = (&'a PathNotes, Vec<&'a str>, Vec<&'a str>);

/// A note for each line of the diff `d` from `sn`'s notes.
pub fn line_notes<'a>(sn: &'a StepNotes, d: &[u8]) -> Vec<&'a str> {
    let (paths, places) = places(d);
    let expanded: Vec<Option<Expanded>> = paths
        .iter()
        .map(|p| {
            sn.paths.get(p).map(|n| {
                (n, n.old.as_ref().map(expand).unwrap_or_default(), n.new.as_ref().map(expand).unwrap_or_default())
            })
        })
        .collect();
    places
        .into_iter()
        .map(|(path, side, i)| {
            let e = expanded[path as usize].as_ref();
            match side {
                b's' => sn.note.as_str(),
                // The path's note on its `path` line only.
                b'P' => e.map_or("", |e| e.0.note.as_str()),
                b'p' => "",
                b'o' => e.and_then(|e| e.1.get(i).copied()).unwrap_or(""),
                _ => e.and_then(|e| e.2.get(i).copied()).unwrap_or(""),
            }
        })
        .collect()
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
