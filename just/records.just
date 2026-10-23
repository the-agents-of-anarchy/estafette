# Every read and write of the project's records, through the shipped memento tool: just records <verb> ...
# `just records` alone runs the check. The pin is the commit sha of the v0.10.0 tag; bump the sha to take a new version.

set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := '..'
set positional-arguments := true

memento := "uvx --from git+https://github.com/ryzhakar/memento@702292b203b84a32d5d001ee78e085a504545c57 memento"

[default]
[doc("Check every record against .claude/memento.yaml: one line per finding, exit 1 on any FAIL")]
[group('verify')]
check *args:
    {{ memento }} check "$@"

[doc("Commit records after the check is green: just records commit <type>: <subject> <path>...")]
[group('write')]
commit *args:
    {{ memento }} commit "$@"

[doc("Print the baseline, the events tail, the found state and the anomalies")]
[group('read')]
patdown *args:
    {{ memento }} patdown "$@"

[doc("Read or write the record map (.claude/memento-map.md)")]
[group('read')]
map *args:
    {{ memento }} map "$@"

[doc("Events: open a span, add a line to its events file, tail it")]
[group('write')]
events *args:
    {{ memento }} events "$@"

[doc("The span's narrative: add a paragraph, close the span, print the newest close block")]
[group('write')]
digest *args:
    {{ memento }} digest "$@"

[doc("Failures: add an entry with its events line")]
[group('write')]
failure *args:
    {{ memento }} failure "$@"

[doc("The change log (docs/architecture_log.md): add, supersede, set a field, repair, age out")]
[group('write')]
change *args:
    {{ memento }} change "$@"

[doc("Conventions (docs/conventions.md): add, replace, set a label of, or drop a rule")]
[group('write')]
directive *args:
    {{ memento }} directive "$@"

[doc("Capabilities (docs/capabilities.md): replace one exact passage")]
[group('write')]
fact *args:
    {{ memento }} fact "$@"

[doc("Specifications: add one, replace one exact passage, archive one")]
[group('write')]
spec *args:
    {{ memento }} spec "$@"

[doc("The measurement log (docs/measurements.yaml): add an entry, void one")]
[group('write')]
measurement *args:
    {{ memento }} measurement "$@"

[doc("Results records: the verb the tool offers for them")]
[group('write')]
results *args:
    {{ memento }} results "$@"
