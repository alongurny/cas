# Format: dir

A directory: a list of named views of other blobs.

The blob is UTF-8 text, made of zero or more lines:

    <format> <key> <name>\n

- `<format>` and `<key>` are blob keys. Together they are a view: `<key>` holds the
  child's bytes, and `<format>` is the id of the format to read them with.
- `<name>` is the rest of the line. It is non-empty and is not `.` or `..`. It contains no `/` and
  no control character.
- Exactly one space separates the fields. Every line, including the last, ends in `\n`.
- Lines are in strictly ascending bytewise order of `<name>`.

The empty blob is the empty directory. Any other byte string is not a `dir` blob.
