# Format: git-links

Which git commit each node of a history is: a `uni-bin` map from node ids (strings) to commit
ids (strings of 40 lowercase hex digits). A history converted from git, or to git, keeps one, so
converting again in either direction gives the same nodes and the same commits.

It names each commit, as a `git-commit` at key `sha1:<id>`.
