# Metaformat 1

A format object describes one format. It is UTF-8 text of exactly these three lines, in this
order:

    name <name>\n
    media <media type>\n
    spec <key>\n

- `<name>` is a short label for people. It is non-empty and has no control characters. Names are
  not unique and mean nothing to a reader.
- `<media type>` is the media type of the spec document, such as `text/markdown` or
  `application/pdf`.
- `<key>` is the blob key of the spec document, which defines the format.

A format's id is the blob key of its format object. A view `(<format>, <key>)` means: the
blob `<key>`, read as the format object with key `<format>` specifies. The spec is a plain key,
not a view, so a format never depends on another format's id.

The metaformat's id is the blake3 key of this document. A format object is always read with this
metaformat, so it needs no view of its own.
