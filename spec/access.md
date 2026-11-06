# Outer protocol: identity and access

The core carries no credentials and its gate sees only `who`, the op and the
op's arguments, never store contents. This document fixes the layer around
it: how a caller proves who they are, and the allow-list a gate applies.
Code: `version/src/access.rs`; `cas_core::client::HttpStore::with_token`
and `cas_core::server::bearer` on the wire.

## Credentials and identity

A caller sends a bearer token in every request:

    Authorization: Bearer <token>

A token is 32 random bytes written as 64 lowercase hex digits. The server
hashes the presented token with blake3 and looks for a user whose stored
token key matches; that user's name is the `who`. No token, or a token that
matches nobody, is the anonymous caller. A wrong token is not an error of
its own: the request simply runs with anonymous rights, and denials come
back as `403` as usual.

Only token hashes are stored. A token is shown once, when the user is
created; losing it means issuing a new one. Tokens are meant to travel as
files or network secrets, not environment variables: the CLI reads one with
`--token-file`.

Run the server behind TLS (or a tailnet) so tokens are not sent in clear.
Other identities (tailnet node names, TLS client certificates) plug in the
same way: anything that maps a request to a `who` is an authenticator.

## The meta-store and the policy document

Users and grants live in a store of their own, the meta-store, never in
the store they govern, so a gate decides from configuration and the op
alone. The meta-store's variable `policy` holds a view of a `uni-bin`
document:

    {'grants': {'*': {'list blobs': {}, 'read blobs': {}, 'read var:main': {}},
                'alice': {'write blobs': {}, 'write var:main': {}}},
     'users': {'alice': {'token': 'blake3:9f2c...'}}}

- `users` maps a user name (lowercase letters, digits, `-`, `_`) to
  `{'token': <blake3 key of the token>}`.
- `grants` maps a grantee to a set of grants. A grantee is a user name,
  `*` (every caller, signed in or not), or `+` (every signed-in caller).

Changing the policy is a compare-and-set of the `policy` variable, so two
administrators cannot silently overwrite each other. A server re-reads the
variable on each request and re-parses the document only when the variable moved, so
changes apply at once, without a restart.

If the variable is absent, nothing is allowed. If the variable or document cannot be
read or parsed, everything is denied (fail closed).

## Grants

A grant is `<action> <scope>`.

| Action | Allows |
| --- | --- |
| `read` | `read` of a key in scope |
| `write` | `write` of a key in scope |
| `remove` | `remove` of a key in scope |
| `list` | `list` of a span all of whose keys are in scope |

| Scope | Keys |
| --- | --- |
| `*` | every key |
| `blobs` | every blob key |
| `var:<prefix>` | variables starting with `<prefix>` (`var:` is every variable) |

An op is allowed when any grant to the caller (by name, `+` or `*`) covers
it. There are no deny rules; access is the union of grants.

A `list` grant covers a span only if no key outside the scope could fall in
it, judged from the span bounds alone:

- `list blobs` covers spans inside one algorithm's range, from `<alg>:` up
  to the first string after all keys starting with it. `has()`, which
  lists a one-key span, needs this grant.
- `list var:<p>` covers spans whose `from` starts with `p` and whose `to`
  is at most the first string after `p`. A prefix that a blob key could
  share (one that is a prefix of `<alg>:` or starts with it, including the
  empty prefix) never authorizes a list.
- `list *` covers every span.

### Typical policies

Public read, one writer:

    *      read blobs, list blobs, read var:main
    alice  write blobs, write var:main

A team where everyone signed in can push branches under `team/` and run
collection (spec/gc.md needs `remove blobs` and `write var:gc/`):

    +      read *, list *, write blobs, write var:team/
    admin  remove blobs, write var:gc/

## Limits

- Write access to blobs cannot be narrowed by content: a blob is named by
  its hash, so the gate cannot tell which version it belongs to.
- Read access to blobs is all or nothing. A blob is shared by every
  version that contains it, so per-branch read secrecy needs separate
  stores.
- A reader who may read variables in scope learns the keys they point to; with
  `read blobs` they can then fetch the content.
