# cas-lean, prebuilt

Prove and verify Lean theorems with one succinct proof. x86_64 Linux.

    curl -sSfL https://raw.githubusercontent.com/alongurny/cas/dist/install.sh | sh
    curl -sSfL https://raw.githubusercontent.com/alongurny/cas/dist/install.sh | sh -s -- --source

The first installs `cas-lean-x86_64-linux` (static, needs AVX2) after
checking it against `SHA256SUMS`; the second builds from the cas and flock
sources with cargo. Then:

    cas-lean verify THEOREM.bundle
    cas-lean prove --project LAKE_DIR MODULE THEOREM

Built from cas `main` (lean-cli) and flock `cas`; the formats are in flock
`docs/lean-bundle.md`.

`BUILD` names the exact cas and flock commits and the rustc of this binary.
