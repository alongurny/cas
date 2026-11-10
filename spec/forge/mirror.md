# Forge record: mirror

A project can follow a git repository. Variable
`p/<owner>/<project>/m/<name>`, a [record](store.md) whose `record.uni`
has `url`, `last` (time of the last pull), `commits` (count), `tip`
(the git commit of the default branch last imported) and `packed` (the
commits, space-separated, whose objects the store already holds). The forge fetches
into a bare repository it keeps only as a cache.

Git branches map onto histories: the default branch to `h/dev`; tags to
`h/release`, each tag a release whose changelog is the tag's message; any
other branch `b` to `u/<owner>/b` (a `/` in `b` becomes `-`). A pull
imports each branch whole; since an import gives the same nodes for the
same commits, the new history extends the old one, and a history the
remote rewrote (dropping nodes) is refused. Importing only the commits
added since the last pull is not done yet. Git's own objects are stored
too (those new since `packed`), and the project's links
([git](git.md)) say which commit each imported node is, so the project
served as git has the remote's commit ids.

Export goes the other way, `cas-forge export`: a history becomes a git
branch with one commit per node, the step's note (or changelog) as its
message, through `git fast-import`. Exporting `h/dev` keeps a git mirror,
such as GitHub's, current. Its commits are not the links' commits; a git
client can instead fetch the project itself ([git](git.md)).
