# Forge: everything in the store

The forge (`cas-forge`, design: "A forge for cas") keeps no state outside
the store it serves: no database, no files beside it. Accounts, project
settings, proposals, reviews, issues and the event log are variables of
that one store, each holding a view, and so they sync, back up and are
collected like any other data.

## Variables

| Variable | Holds |
| --- | --- |
| `forge/a/<name>` | An account: a user or an organization ([account](account.md)) |
| `forge/events` | The newest segment of the event log ([events](events.md)) |
| `p/<owner>/<project>/meta` | The project's settings ([project](project.md)) |
| `p/<owner>/<project>/h/dev` | The development history (the default) |
| `p/<owner>/<project>/h/release` | The release history ([release-history](../release-history.md)) |
| `p/<owner>/<project>/h/<name>` | Other shared histories |
| `p/<owner>/<project>/u/<user>/<name>` | A user's private histories, the sources of proposals |
| `p/<owner>/<project>/r/<n>` | Proposal `n` ([proposal](proposal.md)) |
| `p/<owner>/<project>/i` | The issues history ([issues](issues.md)) |
| `p/<owner>/<project>/m/<name>` | A mirror's state ([mirror](mirror.md)) |
| `p/<owner>/<project>/git/<history>` | Which git commit each node of a history is ([git](git.md)) |
| `p/<owner>/<project>/git/tags` | Git's annotated tags ([git](git.md)) |
| `derived/<F>/<X>` | Derived results: blame, check results ([checks](checks.md)) |

Owner, project and user names are lowercase letters, digits and `-`,
starting with a letter or digit, at most 64 bytes. `forge`, `api`, `p`
and `static` are not owner names.

## Records

Accounts, settings, proposals and mirror states are *records*: a version
(a `dir` tree) holding a file `record.uni`, a `uni-txt` document with the
record's fields, and, beside it, the views the record refers to, at the
paths its spec names. Refering by a view in the tree (not by a key written
in the document) is what makes those objects reachable: gc keeps them and
sync copies them.

A record's fields are strings and maps. In these specs:

- a *number* is a decimal string without leading zeros;
- a *time* is seconds since 1970-01-01 UTC, a number;
- a *set* is a map whose values are empty strings;
- a *list* is a map from numbers written with twelve digits (`000000000001`)
  to the items, so its order is the order of the map.

A record changes by compare-and-set of its variable: read, build the new
record, write expecting the old view, retry on conflict.

## Who may touch what on the wire

The forge serves the cas wire for each project at
`https://<host>/p/<owner>/<project>`: variable names there are under the
project's prefix, as if the project were a store of its own. Every
variable outside a project, and `meta`, `r/`, `i`, `m/` and `git/` inside one,
change only through the forge's API, which checks the rules of each
record. On the wire, by role ([project](project.md)):

| Variables | read, list | write, remove |
| --- | --- | --- |
| `h/<name>` | read | maintain, when not protected; never when protected |
| `u/<user>/...` | read | write, for the user's own |
| `r/`, `i`, `git/` | read | never |
| `meta`, `m/` | never | never |

Blob reads are scoped by reachability: a caller may read a blob only when a
variable they may read in some project reaches it, or it is the object or
spec of a format the forge knows. Asking whether the store holds another
blob answers "absent", so a guessed key reveals nothing. Anyone signed in
may write blobs.
