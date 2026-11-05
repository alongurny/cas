# Format: record

A history record: one step of history, naming a version and the records it came from. Versions
never refer to records; records refer to versions and to each other.

The blob is a `uni` value (see the `uni` format): a map with exactly these three keys.

- `'meta'`: a map of free metadata. By convention, `'author'`, `'message'` and `'time'` (RFC 3339)
  are strings. It may be empty.
- `'parents'`: a set of references to the records this one follows, as a map whose keys are
  references and whose values are all `{}`. It is empty for a first record and has two or more
  entries for a merge.
- `'version'`: a reference to the version this record names.

A reference is a string `<format> <key>`: two blob keys separated by one space. `<key>` holds the
bytes, and `<format>` is the id of the format they are in. The version is in a directory format,
and each parent is in this format.
