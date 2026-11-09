# CAS wire protocol (Stage 3)

The four core ops over HTTP/1.1 inside TLS (HTTPS). Store, server and cache
speak exactly this; a cache is a store whose `read` falls through. Semantics,
outcome order and guarantees are the core spec's; this file only fixes the
encoding.

## Transport

HTTPS is the only transport, per Alon (2026-11-08): "it should only speak
plain https, no other choice". A server listens for TLS and nothing else, so
a plain-HTTP request gets no HTTP answer; a client takes only `https://`
URLs and follows no redirect to another scheme. TLS 1.2 or 1.3 (rustls'
safe defaults). The server has a certificate chain and key (PEM); a client
checks it against the Web PKI roots, or against CA certificates it is given
(a private CA, or a self-signed certificate in tests). Bearer tokens
(access.md) therefore never travel in clear.

One exception, for benchmarks only (Alon, 2026-11-08: "for benchmark only
allow insecure without https ... So we compare Apple's to Apple"): a build
with the `bench-insecure` feature also serves plain HTTP (`serve
--insecure-http`) and takes `http://` URLs. A normal build has neither and
refuses both; `bench/perf/run.sh` builds the variant into `target/bench`.

Requests carry a `Content-Length` body; a chunked request body is answered
411 and the connection closed. A connection carries requests one after
another (keep-alive) until either side closes it or it idles 30 seconds.

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

## Batches

The same ops on many keys in one request, each decided exactly as the single op
would be (same gate, checks and outcome). A frame is a u64 big-endian length and
that many bytes.

| Request | Body | Answer |
| --- | --- | --- |
| `POST /batch/has` | key frames | 200, one byte per key: 1 present, 0 absent |
| `POST /batch/read` | key frames | 200, per key: `0` absent, `1` + value frame, or `2` + outcome frame |
| `POST /batch/write` | per write: key frame, expect frame, value frame | 204, or the first failure's answer with `CAS-Batch-Index: {i}` |

- `has` of a key is gated as `list` of the span `[key, key + 0x00)`; one denied key
  answers the whole request `denied`.
- A read outcome frame is a letter and details: `D` denied, `I{reason}`
  invalid, `R` + reason frame + message refused, `C` conflict, `U{message}`
  unavailable.
- Writes apply in order and stop at the first failure: the writes before index
  `i` are done, `i` and the rest are not (outcome unknown only for
  `unavailable`). A writer that sends children before parents keeps the closure
  invariant. A store may make a run of blob writes durable together (the disk
  store syncs twice per batch, not twice per blob); none is acknowledged before
  all of the run is durable.
- A client keeps write bodies under the server's bound and splits on 413;
  reads and has-checks go in groups of 512 keys, four requests in flight.

## Stored forms

An optimization for copying many blobs; a server may leave it out (it
answers 404), and a client then reads values with `batch/read`. A store
keeps most blobs compressed, many as deltas on another blob; sending that
form spares the server decoding it and the receiver compressing it again,
as git sends its pack's deltas.

| Request | Body | Answer |
| --- | --- | --- |
| `POST /batch/stored` | key frames | 200, per key: as `batch/read`, or `3` + length + zstd frame, or `4` + base key frame + length + delta frame |

- The length is the value's, a u64 big-endian, at most 1 MiB (larger
  values are sent as values). A zstd frame decodes to the value; a delta
  frame is zstd with the value of blob `base` as a raw-content dictionary.
- Gated and decided as `read` of each key. The answer is not checked by the
  server: the reader decodes each form (getting a delta's base from what it
  holds, or reading it as a value) and checks the value against its key, as
  it would a read's.
- A writer may keep a form it received as it is, once it has checked that
  the form decodes to the value. A delta is kept only if its base is kept
  in a form that can be a base and the chain stays within the store's
  limit; otherwise the value is compressed afresh.

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
