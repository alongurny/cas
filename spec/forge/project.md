# Forge record: project settings

Variable `p/<owner>/<project>/meta`, a [record](store.md) whose `record.uni` has:

| Field | Value |
| --- | --- |
| `description` | optional |
| `visibility` | `public` (everyone may read, signed in or not) or `private` |
| `created` | time |
| `members` | map from user or organization name to role |
| `protected` | set of history names (`h/dev`, `h/release` by default) |
| `gates` | map from history name to its gates (below) |
| `labels` | map from label name to `{color, set}` |
| `sets` | map from set name to `one` (exactly one label of it) or `any` |
| `webhooks` | map from id to `{url, secret, events}` |
| `runners` | set of account names whose check results count |
| `next` | the next issue or proposal number |
| `readme` | path of the readme in a version, optional (`README.md`) |

## Roles

`read`, `write`, `maintain`, `admin`, each holding the ones before it:

- read: read the project's histories, proposals and issues; open issues and comment;
- write: also own private histories `u/<user>/`; open proposals and review;
- maintain: also merge proposals, publish releases, manage issues and labels;
- admin: also change settings and members, and delete the project.

A user's role is the highest of: their own entry; the entries of the
organizations they belong to; `admin` for the owner (a user) or for an
owner of the owning organization; `admin` for a site administrator; and
`read` for everyone when the project is public.

## Protected histories

A protected history moves only by merging a proposal into it or
publishing a release, through the forge, once its gates hold; never on
the wire, not even for admins. An admin may change `protected` itself.

## Gates

A history's gates: `approvals` (a number: approvals needed from members
with maintain or above, not counting the author), `checks` (set of check
names that must pass, see [checks](checks.md)), and `resolved` (`true`:
every review thread resolved).

## Numbers

Issues and proposals share `next`: taking a number is a compare-and-set
of `meta` raising it by one, so `#n` names one of them.
