# Format: step-notes

The notes of a step: why each line changed, said on the lines themselves.
They replace commit messages. A step-notes blob names the two versions
and notes the lines of their files: removed lines on the old file, added
and kept lines on the new one. The diff between the versions is not
stored; whoever shows the notes computes it ([diff.md](diff.md)) and puts
each note on its line, so the notes do not depend on how a diff matches
lines.

The blob is UTF-8 text in lines (each ended by a newline):

    step-notes <before> <after>\n

where `<after>` is the view of the new version and `<before>` the view of
the old one, or `-` for a step from nothing. Then, optionally, the step's
own note (what the step as a whole is for):

    note <note>\n

Then, for each path that has a note, in path order (bytes, `/` first):

    path <path>\n

followed by, each optional and in this order:

- `note <note>\n`: a note on the path as a whole (a file added, removed or
  made binary, a directory);
- `old\n` and runs over the lines of the old version's file at the path;
- `new\n` and runs over the lines of the new version's file at the path.

A run is `<count> <note>\n`: the next `<count>` lines (at least one,
written without leading zeros) carry `<note>`, which may be empty (no note).
Adjacent runs of one file have different notes, and the runs of a file
cover its lines exactly, as [annotated](annotated.md) runs do; a file's
lines are as there: pieces ended by a newline, and a last piece without
one. `old` and `new` need a text file at the path on their side. A note
is any text without control characters. A path entry has at least one
non-empty note, and the blob has at least one note.

The two versions are children of the blob.

## Showing

Shown as a diff (the authoring form of [annotated.md](annotated.md) over
the diff's lines), a line takes its note from its file: a `-` line from
the old file, a `+` or ` ` line from the new one; a `path` line takes the
path's note and the `diff` line the step's note. When someone writes notes
on that form, each goes back the same way. A note on an `old`, `new`,
`binary` or `@@` line is the path's note when the `path` line has none,
and otherwise must be empty or the same; a `\` line has the note of the
line before it.
