# Forge: moving from Gitea

`cas-forge migrate-gitea <store> <gitea-url> --as <admin>` moves Gitea
repositories into new projects of the forge: every repository the Gitea
token shows, or each `--repo <owner>/<name>`. Gitea is only read: API
`GET` requests, and `git fetch` of each repository into the forge's
mirror cache. `--dry-run` reads everything and reports what would move
without writing the store. A project that exists is left alone (delete it
to migrate again); the others go on.

## What moves

| Gitea | Forge |
| --- | --- |
| owner, repository name | owner and project names, lowercased, with `.`, `_` and spaces as `-`; a reserved name gets `gitea-` in front |
| organization (when the token may read it) | organization, owned by `--as`, with its members |
| visibility, description | the project's `visibility` and `description`; `from` in its settings is the repository's Gitea URL |
| collaborators | members: Gitea's `admin` and `owner` as admin, `write` as write, the rest read |
| labels | labels with their colors and descriptions; an exclusive scoped label `scope/name` joins set `scope` with rule `one` |
| code | mirror `gitea` ([mirror](mirror.md)): the default branch to `h/dev`, other branches to `u/<owner>/<branch>`, tags to `h/release` |
| issues | issues with their numbers, titles, bodies, states, authors, times (`created`, `updated`, `closed`), labels, assignees, comments, and `from`, the issue's Gitea URL; all in one step of the issues history |
| pull requests | proposals with their numbers ([proposal](proposal.md)); see below |
| users met on the way | accounts without passwords (`cas-forge password` sets one); a Gitea email is kept |

The project's counter continues after the highest number moved, so new
issues and proposals do not reuse Gitea's.

A pull request becomes a proposal with its title (the first line), author,
state (`merged`, `closed` or `open`), times, source `u/<owner>/<branch>`
and target (`h/dev` for the default branch). A proposal has no
description, so what Gitea had beyond that is kept under `imported` in the
record: `from` (its URL), `body`, `head` and `base` (branch and commit),
`merge_commit`, `comments` (list of `{author, time, text}`) and `reviews`
(list of `{author, time, state, text, comments}`, a review's comments being
`{author, path, line, text}`, `line` as Gitea gave it). An open pull request
whose branch came with the code gets its first revision and can be reviewed
and merged here; one whose branch did not stays open with no revision.

## What does not move

Passwords (Gitea's hashes are not the forge's), two-factor settings,
release notes beyond tag messages, attachments, reactions, milestones,
projects boards, wikis, webhooks, deploy keys, branch protection (the forge
protects `h/dev` and `h/release` itself) and Actions. The run says when a
repository has release notes or a wiki that stay behind.
