# Forge record: proposal

A proposal offers the steps of a private history to a target history.
Variable `p/<owner>/<project>/r/<n>`, a [record](store.md) whose
`record.uni` has:

| Field | Value |
| --- | --- |
| `number` | `n` |
| `title` | one line (proposals have no description: steps carry their notes) |
| `author` | user name |
| `source` | variable name in the project, `u/<author>/<name>` |
| `target` | variable name in the project, `h/<name>` |
| `state` | `open`, `merged` or `closed` |
| `created`, `updated` | time |
| `revisions` | list of revisions |
| `approvals` | map from user name to the revision number they approved |
| `threads` | map from thread id to a review thread |
| `merged` | the target's tip after the merge, a node id |

A revision's fields: `time`, `tip` (the source's tip, a node id) and
`base` (the target's tip it was offered against, or empty). The record
keeps the histories themselves at `rev/<i>/source` and `rev/<i>/target`
(views), so a reviewed revision stays readable after its source moves.

## Review threads

A thread's fields: `revision`, `path`, `side` (`old` or `new`), `line`
(1-based), `text` (the line's text when the thread began), `author`, `time`, `resolved` (`true`, optional),
`promote` (`true` when the author accepted the thread's note as a step
note), `comments` (list of `{author, time, text, suggestion}`; a
suggestion replaces the anchored line). An anchor of a thread on an
earlier revision moves to the line of the latest one with the same text
nearest to it; a thread whose line is gone is outdated.

## Merging

Merge is a rebase only: the source's steps beyond the target are replayed
on the target's tip, and the target moves by one compare-and-set to the
result. The gates of the target ([project](project.md)) are checked
first. A promoted thread's first comment becomes the note of its line in
the newest replayed step that added a line with the thread's text. Merging sets `state` to `merged`
and `merged` to the new tip.
