# The gate: what must pass before every commit. CI, pre-commit and `just check` run this one definition.

set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := '..'

audit_pin := "0.22.2"
killable_server := "estafette-lab-2"
built_binary := "target/debug/estafette"
claude_session_variables := "ESTAFETTE_ACCEPTANCE_CLAUDE_SOCKET ESTAFETTE_ACCEPTANCE_CLAUDE_TOKEN ESTAFETTE_ACCEPTANCE_CLAUDE_EXPECTED_PERMISSION_MODE ESTAFETTE_ACCEPTANCE_CLAUDE_DEBUG_LOG ESTAFETTE_ACCEPTANCE_CLAUDE_TRANSCRIPT"

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
    [[ "$(cargo audit --version)" == *" {{ audit_pin }}" ]] || { echo "cargo-audit must be {{ audit_pin }}: cargo binstall cargo-audit@{{ audit_pin }} (or just gate::install-audit)" >&2; exit 1; }

[doc("Install the pinned cargo-audit; the one place CI and developers take the version from")]
[group('fix')]
install-audit:
    cargo binstall --no-confirm cargo-audit@{{ audit_pin }}

[doc("Run every test of the workspace with the relay feature on")]
[group('verify')]
test:
    cargo test --workspace --features estafette/relay

[doc("Format every file in place")]
[group('fix')]
fmt:
    cargo fmt --all

[doc("Run the acceptance tests of spec 3.15: every core acceptance target, the one-machine ones and the lab target against the live lab host and a second host it may kill, brought up first by just remote::infrastructure; the lab target starts members on both servers and on this machine, stops, starts and kills the second host, and drops UDP between the second host's server and this machine while test 3 runs, deleting the drops when it ends; then the bridge lab target, skipped with a line naming the unset ESTAFETTE_ACCEPTANCE_CLAUDE_* variables; exits with the first non-zero code; both servers are torn down by the run-hetzner-lab skill once it exits; set ESTAFETTE_ACCEPTANCE_RUN_RECORDS to a directory to keep every member run record there under the test name, secrets replaced by <redacted>")]
[group('verify')]
acceptance:
    #!/usr/bin/env bash
    set -uo pipefail
    cargo build -p estafette || exit
    invitation="$(just remote::invitation)" || exit
    killable_host_line="$(LAB_SERVER={{ killable_server }} LAB_BINARY={{ built_binary }} just remote::infrastructure)" || exit
    export ESTAFETTE_ACCEPTANCE_INVITATION="$invitation"
    export ESTAFETTE_ACCEPTANCE_KILLABLE_SERVER={{ killable_server }}
    export ESTAFETTE_ACCEPTANCE_KILLABLE_HOST_LINE="$killable_host_line"
    first_failure=0
    cargo test -p estafette --features lab --no-fail-fast --test 'acceptance_*' || first_failure=$?
    unset_variables=()
    for variable in {{ claude_session_variables }}; do
        [[ -n "${!variable:-}" ]] || unset_variables+=("$variable")
    done
    if (( ${#unset_variables[@]} > 0 )); then
        printf 'bridge lab target skipped: %s unset\n' "${unset_variables[*]}" >&2
    else
        cargo test -p estafette-bridge-claude --features lab --test 'acceptance_*' || bridge_failure=$?
        (( first_failure != 0 )) || first_failure=${bridge_failure:-0}
    fi
    exit "$first_failure"
