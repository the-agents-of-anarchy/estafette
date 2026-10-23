# CI and local use one definition of "every commit passes": the gate and the records check, commit by commit.

set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := '..'
set positional-arguments := true

[doc("Run just check and just records on every commit of a range, oldest first; fails naming the first bad commit: just ci::each-commit <base>..<tip>")]
[group('verify')]
each-commit range:
    #!/usr/bin/env bash
    set -euo pipefail
    start="$(git symbolic-ref --quiet --short HEAD || git rev-parse HEAD)"
    trap 'git checkout --quiet "$start"' EXIT
    shas="$(git rev-list --reverse "$1")"
    if [ -z "$shas" ]; then
        echo "ci::each-commit: range $1 holds no commit; nothing to check"
        exit 0
    fi
    for sha in $shas; do
        echo "== $(git log -1 --format='%h %s' "$sha")"
        git checkout --quiet --detach "$sha"
        just check || { echo "FAIL: just check failed at commit $sha" >&2; exit 1; }
        just records || { echo "FAIL: just records failed at commit $sha" >&2; exit 1; }
    done
    echo "ci::each-commit: every commit of $1 passed"
