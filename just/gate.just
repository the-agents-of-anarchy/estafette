# The gate: what must pass before every commit. CI, pre-commit and `just check` run this one definition.

set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := '..'

audit_pin := "0.22.2"

[default]
[doc("The whole gate: format check, clippy with and without relay, tests, advisory audit; run before every commit")]
[group('verify')]
check: fmt-check clippy clippy-relay test audit

[doc("Fail when any file is not formatted")]
[group('verify')]
fmt-check:
    cargo fmt --all -- --check

[doc("Clippy over every target, warnings denied")]
[group('verify')]
clippy:
    cargo clippy --all-targets -- -D warnings

[doc("Clippy over every target with the relay feature, warnings denied; skipped while no package declares it")]
[group('verify')]
clippy-relay:
    metadata="$(cargo metadata --no-deps --format-version 1)"; \
    if grep -q '"relay":\[' <<<"$metadata"; then \
        cargo clippy --all-targets --features relay -- -D warnings; \
    else \
        echo 'relay feature absent; skipped'; \
    fi

[doc("Fail on any RustSec advisory against Cargo.lock, with cargo-audit held at its pinned version")]
[group('verify')]
audit: audit-pin-held
    cargo audit

[doc("Fail unless the installed cargo-audit is the pinned version")]
[private]
audit-pin-held:
    [[ "$(cargo audit --version)" == *" {{ audit_pin }}" ]] || { echo "cargo-audit must be {{ audit_pin }}: just gate::install-audit" >&2; exit 1; }

[doc("Install the pinned cargo-audit; the one place CI and developers take the version from")]
[group('fix')]
install-audit:
    cargo binstall --no-confirm cargo-audit@{{ audit_pin }}

[doc("Run every test of the workspace")]
[group('verify')]
test:
    cargo test

[doc("Format every file in place")]
[group('fix')]
fmt:
    cargo fmt --all
