# CAS wire protocol (Stage 3)

The four core ops over HTTP/1.1. Store, server and cache speak exactly this; a
cache is a store whose `read` falls through. Semantics, outcome order and
guarantees are the core spec's; this file only fixes the encoding.

## Identity

The core carries no credentials. The outer protocol (TLS client certs, a
tailnet identity, an `Authorization` header, ...) tells the server who is
calling; the server turns that into the `who` its gate sees. Nothing below
names a caller.

## Keys in URLs

A key travels as one path segment, percent-encoded: every byte except
`A-Z a-z 0-9 - . _ ~` is written `%XX`. So `refs/heads/main` is
`refs%2Fheads%2Fmain`. A server accepts any valid percent-encoding of the same
bytes. Bytes that do not decode to UTF-8 are `invalid` (`key`).

## Expect frame

`write` and `remove` carry `expect` in the request body, so tag values of any
size fit:

| Bytes | Meaning |
| --- | --- |
| `00` | `absent` |
| `01` + 8-byte big-endian length `n` + `n` bytes | a value |

## Requests

| Op | Request | Body |
| --- | --- | --- |
| read | `GET /k/{key}` | none |
| write | `PUT /k/{key}` | expect frame, then the value (rest of the body) |
| remove | `POST /remove/{key}` | expect frame |
| list | `GET /list?from={hex}&to={hex}` | none |

`from` and `to` are the span bounds as lowercase hex of their bytes; leave one
out for an open bound. A client may add `limit={n}` as a paging hint.

## Responses

| Outcome | Status | Headers | Body |
| --- | --- | --- | --- |
| ok (read) | 200 | `Cache-Control`, see below | the value |
| ok (write, remove) | 204 | | |
| ok (list) | 200 | `CAS-Rest-From: {hex}` when only part of the span was answered | each key followed by `\n` |
| absent | 404 | `Cache-Control: no-store` | |
| denied | 403 | nothing else | empty |
| invalid | 400 | `CAS-Reason: key`, `expect`, `hash-mismatch` or `request` | empty |
| refused | 422 | `CAS-Reason: {reason}` | optional UTF-8 message |
| conflict | 409 | | empty |
| unavailable | 503 | | optional UTF-8 message |

A partial `list` answer means the rest of the span is `[CAS-Rest-From, to)`.
Keys contain no control characters, so `\n` separates them unambiguously.

`invalid` with reason `request` means the HTTP request itself is malformed (bad
framing, bad hex). That is not a core op, so it is the one answer that can
come before `denied`.

### Caching headers

- A blob read: `Cache-Control: private, max-age=31536000, immutable`. The
  bytes never change, but reads are gated, so only caches that run the same
  gate (a CAS cache, the client itself) may keep them. `private` keeps shared
  HTTP caches out.
- A tag read and every `absent`: `Cache-Control: no-store`. Caches forward tag
  reads (spec section 7).
- `list`: `Cache-Control: no-store`.

## Caching store

A caching store holds a local store and an upstream store, and runs the same
gate as upstream for the caller it serves:

- `read` of a blob: gate, then answer from the local copy if there is one;
  otherwise forward, verify the bytes hash to the key, keep a copy, return.
- `read` of a tag, `write`, `remove`, `list`: gate, then forward and return the
  upstream outcome unchanged. A successful blob write also fills the local
  copy; a successful blob remove drops it.
