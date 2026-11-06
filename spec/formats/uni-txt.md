# Format: uni-txt

The values of the `uni-bin` format, stored as their text notation instead of their binary
serialization. The blob is the canonical text of exactly one value, in UTF-8, with nothing before
or after it:

    value  = string | map
    string = "'" { char } "'"
    map    = "{" [ entry { ", " entry } ] "}"
    entry  = value ": " value

- In a string, `\` is written `\\` and `'` is written `\'`. Every other character is written as
  itself, control characters included.
- A map's entries are in strictly ascending bytewise order of their keys' `uni-bin`
  serializations, the same order as in `uni-bin`.
- Separators are exactly `, ` and `: `. There is no other whitespace outside strings.

A value has one `uni-bin` blob and one `uni-txt` blob, with different keys. Converting between
them is lossless. A view says which of the two it points to.
