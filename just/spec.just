# Read-only helpers over the specification and the experiments.

set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := '..'
set positional-arguments := true

[doc("Search the specification, case-insensitive, with line numbers: just spec::search <pattern>")]
[group('read')]
search pattern:
    grep -n -i -e "$1" spec/blueprint.md

[doc("Print one numbered subsection of Part 3, e.g. just spec::section 3.15")]
[group('read')]
section number:
    awk -v n="$1" '/^#{2,3} / { on = ($2 == n) } on' spec/blueprint.md

[doc("List the experiment files with their first line")]
[group('read')]
experiments:
    awk 'FNR == 1 { print FILENAME ": " $0 }' research/stack/experiments/*.md
