# CAS wire protocol (Stage 3)

The four core ops over HTTP/1.1. Store, server and cache speak exactly this; a
cache is a store whose `read` falls through. Semantics, outcome order and
guarantees are the core spec's; this file only fixes the encoding.

## Identity

The core carries no credentials. The outer protocol (TLS client certs, a
tailnet identity, an `Authorization` header, ...) tells the server who is
calling; the server turns that into the `who` its gate sees. Nothing below
names a caller.

The outer protocol this implementation ships, a bearer token in an
`Authorization` header checked against a policy in a separate meta-store,
is in [access.md](access.md).

## Keys in URLs

A key travels as one path segment, percent-encoded: every byte except
`A-Z a-z 0-9 - . _ ~` is written `%XX`. So `refs/heads/main` is
`refs%2Fheads%2Fmain`. A server accepts any valid percent-encoding of the same
bytes. Bytes that do not decode to UTF-8 are `invalid` (`key`).

## Expect frame

`write` and `remove` carry `expect` in the request body, so variable values of any
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
| invalid | 400 | `CAS-Reason: key`, `expect`, `hash-mismatch`, `view` or `request` | empty |
| refused | 422 | `CAS-Reason: {reason}` | optional UTF-8 message |
| conflict | 409 | | empty |
| request too large | 413 | `CAS-Reason: too-large` | optional UTF-8 message |
| unavailable | 503 | | optional UTF-8 message |

A partial `list` answer means the rest of the span is `[CAS-Rest-From, to)`.
Keys contain no control characters, so `\n` separates them unambiguously.

`invalid` with reason `request` means the HTTP request itself is malformed (bad
framing, bad hex). That is not a core op, so it is the one answer that can
come before `denied`. So can 413: a server bounds the request bodies it reads
(the gate may look at the value, so the body is read before the gate) and
answers 413 to a larger one without buffering it. A client reports it as
`refused` with reason `too-large`.

## Chunked transfer

An optimization for writing big blobs; a server may leave it out (it answers
404 to these requests), and a client then sends the value whole. Reads are
unchanged: `GET /k/{key}` returns the whole value, so fetching one object
stays one request.

A chunk is a piece of a blob value, named by the blake3 hash of its bytes (32
bytes, lowercase hex in URLs). Chunks are cut by FastCDC (minimum 4 KiB,
average 16 KiB, maximum 64 KiB), so both ends agree on the cuts and an edit
moves only the chunks around it. A client uses this for blob values over
64 KiB:

| Request | Body | Answer |
| --- | --- | --- |
| `POST /chunks/missing` | chunk ids, concatenated | 200, the ids of those the server lacks, concatenated |
| `PUT /chunk/{id}` | the chunk's bytes, at most 64 KiB | 204; 400 `hash-mismatch` if they do not hash to `id` |
| `PUT /k/{key}` with `Content-Type: application/vnd.cas.write-chunks` | expect frame, then the value's chunk ids in order | as a write of the concatenation |

Chunks are not keys: they are not listed, read or removed over the wire. The
gate decides asking about and uploading chunk `id` as a write of blob key
`blake3:{id}`, so only callers who may write blobs learn which chunks a
server holds or add any. The last request assembles the value and then is an
ordinary write of it, with the same gate, checks and answers (its key is
checked against the whole value). If a chunk named there is gone, the answer
is 422 `chunks-missing`, and the client sends the value whole.

### Caching headers

- A blob read: `Cache-Control: private, max-age=31536000, immutable`. The
  bytes never change, but reads are gated, so only caches that run the same
  gate (a CAS cache, the client itself) may keep them. `private` keeps shared
  HTTP caches out.
- A variable read and every `absent`: `Cache-Control: no-store`. Caches forward variable
  reads (spec section 7).
- `list`: `Cache-Control: no-store`.

## Caching store

A caching store holds a local store and an upstream store, and runs the same
gate as upstream for the caller it serves:

- `read` of a blob: gate, then answer from the local copy if there is one;
  otherwise forward, verify the bytes hash to the key, keep a copy, return.
- `read` of a variable, `write`, `remove`, `list`: gate, then forward and return the
  upstream outcome unchanged. A successful blob write also fills the local
  copy; a successful blob remove drops it.
