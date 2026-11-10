# Format: git-tree

A git tree object as git hashes it, before compression:

    tree <n>\0<entries>

`<n>` is the length of `<entries>` in decimal, with no leading zeros. Each entry is
`<mode> <name>\0<id>`: an octal mode (`40000` a tree, `100644` a file, `100755` an
executable file, `120000` a symlink, `160000` a submodule's commit), a name without `/`,
and the 20-byte id of the object it names. The sha1 key of such a blob is the tree's git id.

It names the `git-tree` and `git-blob` objects of its entries, at keys `sha1:<id>`; a
submodule's commit is not part of the repository and is not named.
