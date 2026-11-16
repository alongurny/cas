# Format: lean-decl

One Lean declaration, as the kernel sees it: UTF-8 text, one item per line, fields separated by
single spaces, each line ended by a newline. The lines are those of `lean/measure/Export.lean`:

- `N i p s <hex>` or `N i p n <nat>`: name i is name p (0 is the anonymous name) with a string
  (the hex of its UTF-8 bytes) or a number appended;
- `L i z`, `L i s a`, `L i m a b`, `L i i a b`, `L i p <name>`: level i is zero, a successor, a
  maximum, an impredicative maximum, or the level parameter of that name;
- `E i v n` (bound variable n), `E i s l` (sort), `E i c <name> <level>...` (constant),
  `E i a f x` (application), `E i l t b` (lambda), `E i f t b` (pi), `E i z t v b` (let),
  `E i n <nat>` (natural-number literal, decimal), `E i t <hex>` (string literal),
  `E i j <struct name> <field> e` (projection);
- last, `D <name> <type> <nlevels> <level names>... [u] | <rest>`, where `u` marks an unsafe
  declaration and the rest is by kind: `axiom`; `def <value> <hint>` (hint `o` opaque, `a`
  abbreviation, or a height); `thm <value>`; `opaque <value>`; `quot <type|ctor|lift|ind>`;
  `induct <params> <indices> <rec> <nnested> all <names> ctors <names>`;
  `ctor <induct> <cidx> <params> <fields>`;
  `rec <params> <indices> <motives> <minors> <k> all <names> rules (<ctor> <fields> <rhs>)...`.

Other declarations are named, not included: a constant is its name.

The text is canonical. There is exactly one `D` line. The items are those the `D` line reaches, each
once (two items with the same line after its number are one item), numbered from 1 in the order of a
walk that takes the `D` line's nodes in the order it prints them, and gives each item its number
after the items its own line names, in that line's order. So one declaration has one text and one
key, whichever export it came from.

A blob is valid when parsing it and printing it back gives the same bytes.
