# The gate: what must pass before every commit. CI, pre-commit and `just check` run this one definition.

set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := '..'

[default]
[doc("The whole gate: format check, clippy, tests; run before every commit")]
[group('verify')]
check: fmt-check clippy test

[doc("Fail when any file is not formatted")]
[group('verify')]
fmt-check:
    cargo fmt --all -- --check

[doc("Clippy over every target, warnings denied")]
[group('verify')]
clippy:
    cargo clippy --all-targets -- -D warnings

[doc("Run every test of the workspace")]
[group('verify')]
test:
    cargo test

[doc("Format every file in place")]
[group('fix')]
fmt:
    cargo fmt --all
