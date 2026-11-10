# The inner loop of one engineer: watch, run, and keep commit messages to the rule.

set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := '..'

subject_word_limit := "10"

[doc("Rerun clippy on every save (bacon); the standard watch loop of docs/reference/rust-bootstrap.md")]
[group('loop')]
watch:
    bacon clippy

[doc("Run the estafette binary; arguments pass through, e.g. just dev::run --help")]
[group('loop')]
[positional-arguments]
run *args:
    cargo run -p estafette -- "$@"

[doc("Refuse a commit message that is not exactly one line `type(scope): subject` of at most ten words, trailers included; the commit-msg hook passes the message file")]
[group('verify')]
check-message message_file:
    #!/usr/bin/env bash
    set -euo pipefail
    message="$(grep -v '^#' {{ quote(message_file) }} | sed 's/[[:space:]]*$//' || true)"
    refuse() { printf 'commit message refused: %s\n' "$1" >&2; exit 1; }
    [[ -n "$message" ]] || refuse "the message is empty; write one line: type(scope): subject"
    [[ "$message" != *$'\n'* ]] || refuse "the message has a second line or a trailer; write exactly one line: type(scope): subject"
    read -ra words <<<"$message"
    (( ${#words[@]} <= {{ subject_word_limit }} )) || refuse "'$message' has ${#words[@]} words; the limit is {{ subject_word_limit }}, the type(scope): token counted"
    [[ "$message" =~ ^[a-z]+\([^\(\)[:space:]]+\):[[:space:]]+[^[:space:]] ]] || refuse "'$message' is not of the form type(scope): subject"

[doc("Print each commit of a git range whose message breaks the rule, then the count; always exits 0, history is never rewritten: just dev::check-range origin/main")]
[group('verify')]
check-range range:
    #!/usr/bin/env bash
    set -euo pipefail
    message_file="$(mktemp)"
    trap 'rm -f "$message_file"' EXIT
    total=0
    broken=0
    for commit in $(git rev-list {{ quote(range) }}); do
        total=$((total + 1))
        git log -1 --format=%B "$commit" > "$message_file"
        if ! reason="$({{ just_executable() }} --quiet dev::check-message "$message_file" 2>&1)"; then
            broken=$((broken + 1))
            printf '%s %s\n' "$(git rev-parse --short "$commit")" "$reason"
        fi
    done
    printf '%d of %d commits break the message rule\n' "$broken" "$total"

[doc("Install the git hooks of .pre-commit-config.yaml, the commit-msg stage included; one install serves every worktree of the repository")]
[group('fix')]
install-hooks:
    pre-commit install
