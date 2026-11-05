# Format: uni

General data, after the primitive values of the Uniform language. A value is a string (UTF-8
text), or a map from values to values. Nothing else: a list is a map keyed `'0'`, `'1'`, …, and a
set is a map whose values are all `{}`.

## Serialization

The blob is the serialization of exactly one value, with nothing after it. Its key is the hash of
these bytes.

    string = 0x00 len bytes          len: number of bytes, then the UTF-8 bytes
    map    = 0x01 len entries        len: number of entries
    entry  = key value               each a serialized value

- `len` is an unsigned LEB128 integer in its shortest form: 7 bits per byte, low bits first, the
  high bit set on every byte but the last, and no trailing `0x80`-continued zero bytes.
- A map's entries are in strictly ascending bytewise order of their keys' serializations, so keys
  are unique.

Any byte string that is not the serialization of a value is not a `uni` blob.

## Text notation

For people only; it is never hashed or stored. A string is `'…'` with `\` written `\\` and `'`
written `\'`. A map is `{k: v, k: v}`, with entries in serialization order and exactly `, ` and
`: ` as separators.

Example: `{'a': 'x', 'b': {}}` serializes to

    01 02  00 01 61  00 01 78  00 01 62  01 00
