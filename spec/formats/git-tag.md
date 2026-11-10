# Format: git-tag

A git annotated tag object as git hashes it, before compression:

    tag <n>\0<headers>\n<message>

`<n>` is the length of what follows the NUL, in decimal with no leading zeros. The headers are
lines `<name> <value>`, the first `object <id>` (40 lowercase hex digits), then `type`
(`commit`, `tree`, `blob` or `tag`), `tag` and `tagger`. The sha1 key of such a blob is the
tag's git id.

It names its object, in the format its `type` says, at key `sha1:<id>`.
