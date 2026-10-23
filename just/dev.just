# The inner loop of one engineer: watch and run.

set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := '..'

[doc("Rerun clippy on every save (bacon); the standard watch loop of docs/reference/rust-bootstrap.md")]
[group('loop')]
watch:
    bacon clippy

[doc("Run the estafette binary; arguments pass through, e.g. just dev::run --help")]
[group('loop')]
[positional-arguments]
run *args:
    cargo run -p estafette -- "$@"
