# Format: git-commit

A git commit object as git hashes it, before compression:

    commit <n>\0<headers>\n<message>

`<n>` is the length of what follows the NUL, in decimal with no leading zeros. The headers are
lines `<name> <value>`, the first `tree <id>`, then any `parent <id>`, `author`, `committer`
and others, as git writes them; ids are 40 lowercase hex digits. The sha1 key of such a blob is
the commit's git id.

It names its `git-tree` and its parents' `git-commit` objects, at keys `sha1:<id>`.
