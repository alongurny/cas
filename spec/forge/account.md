# Forge record: account

Variable `forge/a/<name>`, a [record](store.md) whose `record.uni` has:

| Field | Value |
| --- | --- |
| `kind` | `user` or `org` |
| `name` | the account name (the variable's last part) |
| `display` | the full name shown, optional |
| `email` | optional |
| `created` | time |
| `admin` | `true` for a site administrator, optional (users only) |
| `password` | an Argon2id hash in PHC string form, optional (users only) |
| `tokens` | map from the `blake3` key of a token to its fields below |
| `members` | organizations: map from user name to `owner` or `member` |

A token's fields: `label` (what it is for), `created` (time), `expires`
(time, optional), and `scope`: `all`, `read`, or `project <owner>/<name>`.
Only an `all` token changes the account itself: its password, display
name, tokens and organizations, and making projects.
A token is 32 random bytes written as 64 hex digits; only its key is
kept, so it is shown once, when made. Signing in with a password makes a
token with label `session` that expires after 30 days.

Signing in follows Gitea: an account name and password give a session; a
token (`Authorization: Bearer <token>`, or `token <token>` as Gitea's API
takes it) stands for its user.

The first user made becomes an administrator. Organizations have no
password and no tokens; their `owner` members act as admins of the
organization's projects.
