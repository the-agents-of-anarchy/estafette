# Account limit figures caught from Claude Code's status line input, and the agents in flight with their token spend and cost.

set shell := ["bash", "-euo", "pipefail", "-c"]

script := source_directory() + '/usage.py'

[doc("Print the last limit figures the status line command wrote and their age; exit 1 when the file is missing or older than max_age_minutes; the file lives under the root checkout's scratch home whichever checkout runs this")]
[group('usage')]
[no-exit-message]
limits max_age_minutes="10":
    python3 -I {{ quote(script) }} limits {{ quote(max_age_minutes) }}

[doc("Print one row per agent whose transcript changed within window_minutes: tokens, context size, dollars by kind at the prices of just/usage-prices.json, the dollars of one cold resume at the cache-write price of the tier each transcript writes at (5m or 1h, shown in the tier column; the lead transcript's tier is printed last), idle minutes, model, name; the root checkout's newest session unless a subagents directory is given")]
[group('usage')]
inflight window_minutes="30" subagents_directory="":
    python3 -I {{ quote(script) }} inflight {{ quote(window_minutes) }} {{ quote(subagents_directory) }}

[doc("Append one line to usage-checkpoints.jsonl beside the limits file: the time, the label, the limit figures last written, and the root checkout's newest session's tokens so far by model and kind (fresh input, cache write, cache read, output) over the lead and every agent")]
[group('usage')]
checkpoint label:
    python3 -I {{ quote(script) }} checkpoint {{ quote(label) }}

[doc("Print the checkpoint lines as a table; each line after the first adds the change in limit percentage points and in tokens by model and kind since the line before; exit 1 when no file exists")]
[group('usage')]
[no-exit-message]
checkpoints:
    python3 -I {{ quote(script) }} checkpoints

[doc("Print per source (the lead apart from the agents), model and cache tier: the assistant turns whose gap to the model message before them exceeds the tier's cache lifetime (5 minutes or 1 hour), their cache-write tokens and share of all that group's cache-write tokens, and the same for turns within the lifetime; the first message of a transcript has no gap and counts in neither; the root checkout's newest session")]
[group('usage')]
coldturns:
    python3 -I {{ quote(script) }} coldturns
