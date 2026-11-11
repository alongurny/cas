# Format: git-commits

Which git commit each commit of a history is: a `uni-bin` map from the keys
of commits (strings, `blake3:` and 64 lowercase hex digits) to git commit
ids (strings of 40 lowercase hex digits). A history that came from git, or
is served to git, keeps one, so converting again in either direction gives
the same commits on both sides.

It names each git commit, as a `git-commit` at key `sha1:<id>`.
