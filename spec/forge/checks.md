# Forge: checks

A check is a command run on a version. Checks are defined in the version
itself, in `.cas/checks.uni` (`uni-txt`): a map from check name to
`{run}`, the shell command, run in a checkout of the version.

A check is a derived function: its function object is the `uni-bin` map
`{name: check, check: <name>, run: <run>}`, and its result over version
`X` is variable `derived/<F>/<key of X>`, a [record](store.md) with
`status` (`pass` or `fail`), `runner`, `time`, and a `raw` file `log`.
A version whose tree and checks are unchanged keeps its results, so a
rebase that changes nothing reruns nothing, and forks share results.

Runners are accounts listed in the project's `runners`; only their
results are written. A runner asks the forge for jobs
(`GET /api/v1/runner/jobs`): the checks of the tips of open proposals,
of `h/dev` and of `h/release` without results, in projects listing it.
It posts results to `POST /api/v1/runner/results`.

Blame is derived the same way: `derived/<blame>/<X>` for a `uni-bin`
query `{after, node, path}` at `X` is an annotated file of the path's
text at commit `node`, each line noting the key of the commit that last
changed it, computed from the result for the commit before it along first
parents.
