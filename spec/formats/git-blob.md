# Format: git-blob

A regular file, stored as a git loose blob object before compression:

    blob <n>\0<contents>

`<n>` is the length of `<contents>` in decimal, with no leading zeros. The file's contents are
`<contents>`. The sha1 key of such a blob is the file's git object id.
