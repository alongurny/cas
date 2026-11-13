# Format: annotated

A note on every line of a blob. It says, in short, why a line is there or
was changed, or anything else worth knowing about it. When the blob is a
[diff](diff.md) of a step, the annotation is the step's notes and takes the
place of a changelog: each changed line carries its own reason (the
`step` of a parent in a [commit](commit.md)). An annotation of a
plain file serves where there is only one side, the before or the after.

The blob is UTF-8 text:

    annotated <format> <key>\n
    <count> <note>\n
    ...

- The first line names the annotated blob by its view.
- Every further line is a run: `<count>` lines in a row share `<note>`.
  `<count>` is a decimal number from 1 without leading zeros, then one
  space, then the note: any text without control characters, possibly
  empty (an empty note says nothing about those lines).
- Adjacent runs have different notes, so the blob is a function of the
  view and the note of each line.
- The counts add up to the number of lines of the annotated blob. Its lines
  are the pieces up to and including each newline, and a last piece without
  one if the blob does not end in a newline; an empty blob has no lines.

The annotated blob is a child of the annotation (it is copied and kept
with it).

## Authoring form

People read and write notes in this form, never stored: each line of the
text after `| ` (or `|` alone for an empty line), and lines `# <note>`
that set the note for the text lines after them, up to the next `#` line.
Lines start with no note. A line `\` after a text line means that line had
no newline. Blank lines are ignored. `cas` prints this form (`status DIR
--diff`, `diff --notes`, `annotated show`) and reads it (`commit --notes
FILE`, `annotated make FILE`).
