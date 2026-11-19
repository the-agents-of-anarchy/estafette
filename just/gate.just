set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := '..'

heavy_evidence_script := source_directory() + '/heavy-evidence.py'
no_test_sleeps_script := source_directory() + '/no-test-sleeps.py'
audit_pin := "0.22.2"
killable_server := "estafette-lab-2"
built_binary := "target/debug/estafette"
claude_session_variables := "ESTAFETTE_ACCEPTANCE_CLAUDE_SOCKET ESTAFETTE_ACCEPTANCE_CLAUDE_TOKEN ESTAFETTE_ACCEPTANCE_CLAUDE_EXPECTED_PERMISSION_MODE ESTAFETTE_ACCEPTANCE_CLAUDE_DEBUG_LOG ESTAFETTE_ACCEPTANCE_CLAUDE_TRANSCRIPT"

[default]
[doc("The whole gate: the light gate, then the heavy gate; CI runs it")]
[group('verify')]
check: check-light check-heavy

[doc("The light gate, run by the commit hook on every source commit: format check, then the refusal of a sleep in test code, then clippy over every target with and without relay, warnings denied; stops at the first failure")]
[group('verify')]
check-light: fmt-check no-test-sleeps clippy clippy-relay

[doc("The heavy gate, run once at a head: takes the machine-wide heavy gate lock first, printing the holder and waiting while another heavy gate holds it, then every test of the workspace, then the pinned advisory audit, both always run; writes one evidence file (commit, tree, worktree status, seconds waited for the lock, times, each exit code, every test result line with its binary, ignored tests, the full log's path) under target/heavy-evidence/ or at evidence_path, whatever the outcome, prints its absolute path as the last line of standard output and exits nonzero when either command failed")]
[group('verify')]
check-heavy evidence_path="":
    python3 -I {{ quote(heavy_evidence_script) }} {{ quote(evidence_path) }}

[doc("Fail when any file is not formatted")]
[group('verify')]
fmt-check:
    cargo fmt --all -- --check

[doc("Fail when test code under core/ or bridges/claude/ sleeps. Reads every file git lists there, tracked or not ignored. Test code is every file under a tests directory; every file a test module line names, with every file beneath it; and every item, statement or expression under a test attribute (test, or any path ending in ::test), under a cfg whose predicate, read as a formula of all, any and not over its names, holds with test set and fails with test unset for some setting of the other names, decided by trying every setting (not(not(test)) and any(test, unix) are test code, not(test) and any(not(test), unix) are not), or under a cfg_attr whose predicate can hold with test set and that carries a test attribute or such a cfg. An item runs to the close of its body or to its semicolon; a statement runs whole to its own semicolon, whatever brackets stand inside it, or to the bracket that closes around it; a block-like statement (a block, an if with its else branches, a match, a loop, a while, a for) ends at its last block unless a method call or ? follows it; a match arm ends at its comma or at the close of its block body, a field or an argument at its comma. A sleep is std::thread::sleep, std::thread::sleep_ms, tokio::time::sleep or sleep_until named in code, called or passed, by any path ending in its name, bare (a variable of that name included), or bare under an alias a use ... as of the same file gives. A string literal is read by its value, escapes and line continuations decoded, and holds a shell sleep wherever its value holds sleep, whatever stands before it (a path ending in /sleep included), followed by a blank, a line continuation or the value's end, whatever comes after, whether or not a shell runs it; every file under a tests directory that is not Rust is read as a shell script the same way, a # that starts a word outside quotes starting a comment to the end of its line. Prints each as file:line: line, then the count, on standard error, exiting 1, and nothing, exiting 0, when there is none; exits 2 on a test module line that names no file. Reports beyond need: the sleeps of a product file a test module pulls in by #[path]; sleep then a blank in prose, in an echoed string or at the end of a longer word (usleep 1) in a literal or in a non-Rust file under a tests directory; a cfg whose names cannot hold together (two target_os values) read as if they could; every line of a file in which an inner test cfg stands, even inside an inline module. Does not see: a wait whose running out is the pass (a timeout around a receive, recv_timeout, park_timeout, a wait on a condition variable, a spin on the clock); a sleep reached through a function or macro product code defines and a test calls; a method named sleep; an alias imported from another file; a timer of another name (tokio::time::interval, a Delay future); a shell command built from pieces; a doc-comment example run as a doctest; a script outside a tests directory that a test runs; a sleep in a script of another language than shell (time.sleep in Python); in a match arm, a field or an argument under a test cfg, a sleep after a comma of generic arguments or closure parameters standing at the arm's own level")]
[group('verify')]
no-test-sleeps:
    @python3 -I {{ quote(no_test_sleeps_script) }}

[doc("Clippy over every target, warnings denied")]
[group('verify')]
clippy:
    cargo clippy --all-targets -- -D warnings

[doc("Clippy over every target with the relay feature, warnings denied; fails when no package declares the feature")]
[group('verify')]
clippy-relay:
    cargo clippy --all-targets --features relay -- -D warnings

[doc("Fail on any RustSec advisory against Cargo.lock, with cargo-audit held at its pinned version")]
[group('verify')]
audit: audit-pin-held
    cargo audit

[doc("Fail unless the installed cargo-audit is the pinned version")]
[private]
audit-pin-held:
    [[ "$(cargo audit --version)" == *" {{ audit_pin }}" ]] || { echo "cargo-audit must be {{ audit_pin }}: run just gate::install-audit" >&2; exit 1; }

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

[doc("Run the acceptance tests of spec 3.15: every core acceptance target, the one-machine ones and the lab target against the live lab host and a second host it may kill, brought up first by just remote::infrastructure; the lab target starts members on both servers and on this machine, stops, starts and kills the second host, and drops UDP between the second host's server and this machine while test 3 runs, deleting the drops when it ends; then the bridge lab target, skipped with a line naming the unset ESTAFETTE_ACCEPTANCE_CLAUDE_* variables; exits with the first non-zero code; both servers are torn down by the run-hetzner-lab skill once it exits; set ESTAFETTE_ACCEPTANCE_RUN_RECORDS to a directory to keep there, under the test name, every member run record with secrets replaced by <redacted> and every machine's data directory but its secret key files")]
[group('verify')]
acceptance:
    #!/usr/bin/env bash
    set -uo pipefail
    cargo build -p estafette || exit
    just remote::invitation || exit
    invitation="$(sed -n 's/^invitation: //p' "$(just remote::host-record-path)")"
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
