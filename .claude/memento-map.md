# estafette map

| path | kind | load | purpose |
|---|---|---|---|
| CLAUDE.md | charter | self | goals, session start, prime directives, pointers |
| docs/ground-truth.md | frame | self | the problem as given |
| docs/conventions.md | directive | self | how to work: rules with grounds |
| docs/architecture_log.md | change | self | how the system's core changed |
| .claude/memento.yaml | schema | self | record kinds, homes, formats, admission |
| .claude/memento-map.md | map | start | index of records |
| docs/orchestration_log/history/{date}/events*.md | events | start (tail) | one line per event |
| docs/orchestration_log/history/{date}/session*.md | digest | start (close block) | a span's narrative and close block |
| docs/capabilities.md | fact | need | what the system is and does, dated |
| docs/orchestration_log/history/{date}/failures*.md | failure | latent | diagnosed failures |
| docs/orchestration_log/archive/ | archived | latent | out of force, kept |
| docs/orchestration_log/recon/{date}/ | scratch | latent | drafts, claims, worker returns |
