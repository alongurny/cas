# History records (Stage 5)

History is metadata, kept apart from versions. A **record** (format `record`,
[formats/record.md](formats/record.md)) names a version, the records it follows, and free
metadata. Versions never refer to records, so the same tree reached by different histories has
one key. Records chain only among themselves.

Records are stored in `uni` ([formats/uni.md](formats/uni.md)), a general data format modeled on
Uniform's primitive values. A value is a string or a map from values to values. The blob is a
canonical binary serialization, and the key is the hash of those bytes. A text notation such as
`{'a': 'x', 'b': {}}` is only for display. The format says which fields hold references, written
as strings `<format> <key>`. `uni` has no separate reference kind; one can come later as a new
format if tools need to find links without knowing a format.

## Branches

A branch is a tag whose value is the text of a reference to a record. `commit(tag, version,
meta)` reads the tag, writes a record whose parent is the current head (none for a new branch),
and moves the tag by compare-and-swap from the old value. If someone moved the tag in between,
the swap fails with `conflict` and nothing is lost: the new record exists, just unreferenced.
Merging after a conflict is deferred (handoff).

## Walking history

- `log(head)` returns `head` and everything it follows, each once. A record always comes before
  its parents, and ties go to the smallest reference, so the order is deterministic.
- `merge_bases(a, b)` returns the best common ancestors: records both reach (each reaches itself)
  that no other common ancestor follows. There is more than one after a criss-cross merge, and
  none for unrelated histories.

## Conformance

`version/tests/history.rs` checks:

- `uni`: round trips through bytes and text; that no other bytes decode to the same value; and
  rejection of non-shortest lengths, unsorted or duplicate keys, invalid UTF-8 and trailing bytes.
- Records: round trips, content addressing, and rejection of malformed records.
- Commits: they move the branch, and a concurrent move gives `conflict`.
- `log` order, and merge bases for a linear history, a merge, a criss-cross and unrelated
  histories.
