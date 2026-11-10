# Format: diff

The line diff of a step: what changed from one version to another, file by
file, with the changed lines of text files. It is the text a step's notes
annotate ([annotated.md](annotated.md)).

The blob is bytes in lines (each ended by a newline):

    diff <before> <after>\n

where `<after>` is the view of the new version and `<before>` the view of
the old one, or `-` for a step from nothing (a root node: everything is
added). Then, for each path that changed, in path order (bytes, `/` first):

    path <path>\n
    old <view>\n
    new <view>\n

`old` and `new` are the file's view on each side, `-` where there is none,
or `dir` where the path is a directory (a directory added or removed, or
replacing a file; what is under it is listed as paths of its own).
Then, if either side is a file:

- `binary\n` when a side is not text: a format other than `raw` or `exec`,
  or a file holding a NUL byte;
- otherwise the hunks: a header `@@ -<a>,<b> +<c>,<d> @@\n` (the old lines
  `a` to `a+b-1` became the new lines `c` to `c+d-1`, counted from 1; a
  start of an empty range is the line before it), then `b` old and `d` new
  lines, each line marked `' '` (in both), `-` (old only) or `+` (new only)
  and followed by its newline. A line that had no newline (the last of its
  file) is followed by a line `\`.

`cas` writes three lines of context around changes and finds the lines with
Myers' algorithm, but any diff that applies is valid: applying the hunks to
the old file gives the new one. The two versions are children of the diff.
