# Account limit figures caught from Claude Code's status line input, and the agents in flight with their token spend and cost.

set shell := ["bash", "-euo", "pipefail", "-c"]

script := source_directory() + '/usage.py'

[doc("Print the last limit figures the status line command wrote and their age; exit 1 when the file is missing or older than max_age_minutes; the file lives under the root checkout's scratch home whichever checkout runs this")]
[group('usage')]
[no-exit-message]
limits max_age_minutes="10":
    python3 -I {{ quote(script) }} limits {{ quote(max_age_minutes) }}

[doc("Print one row per agent whose transcript changed within window_minutes: tokens, context size, dollars by kind at the prices of just/usage-prices.json, the dollars of one cold resume at the cache-write price of the tier each transcript writes at (5m or 1h, shown in the tier column; the lead transcript's tier is printed last), idle minutes, model, effort, name; the root checkout's newest session unless a subagents directory is given")]
[group('usage')]
inflight window_minutes="30" subagents_directory="":
    python3 -I {{ quote(script) }} inflight {{ quote(window_minutes) }} {{ quote(subagents_directory) }}

[doc("Append one line to usage-checkpoints.jsonl beside the limits file: the time, the label, the limit figures last written, and the root checkout's newest session's tokens so far by model with effort and kind (fresh input, cache write, cache read, output) over the lead and every agent")]
[group('usage')]
checkpoint label:
    python3 -I {{ quote(script) }} checkpoint {{ quote(label) }}

[doc("Print the checkpoint lines as a table; each line after the first adds the change in limit percentage points and in tokens by model with effort and kind since the line before; exit 1 when no file exists")]
[group('usage')]
[no-exit-message]
checkpoints:
    python3 -I {{ quote(script) }} checkpoints

[doc("Print per source (the lead apart from the agents), model with effort and cache tier: the assistant turns whose gap to the model message before them exceeds the tier's cache lifetime (5 minutes or 1 hour), their cache-write tokens and share of all that group's cache-write tokens, and the same for turns within the lifetime; the first message of a transcript has no gap and counts in neither; the root checkout's newest session")]
[group('usage')]
coldturns:
    python3 -I {{ quote(script) }} coldturns

[doc("Compute one attempt, arguments: model effort context_k rounds cold_restarts tool_min agent|lead [--generated-k N]; model is a key of usage-prices.json, effort is low, high or max, context_k the thousands of tokens carried on average, tool_min the minutes of tools; prints dollars for generation, re-reads and re-writes at the prices of usage-prices.json (the re-write at the 5m write price for an agent, the 1h price for the lead) and their sum, the limit share of the same three where usage-params.json knows the model's shares and ? where not, minutes of generation at the model's speed, of tools and their sum, and the cache life of the role; reads usage-prices.json and usage-params.json; refuses an unknown model or effort naming the accepted set, tokens generated null in usage-params.json and not passed with --generated-k, and a null speed")]
[group('usage')]
[no-exit-message]
attempt *arguments:
    python3 -I {{ quote(script) }} attempt {{ arguments }}

[doc("Compute the cost of one delivered unit, arguments: the arguments of attempt, then check_usd check_min miss_usd miss_min [--reach R] [--generated-k N]; prints the attempt table and then dollars and minutes per delivered unit = (attempt + check + (1 - reach) x miss) / reach, where the attempt's minutes are its total and reach is the share of attempts that deliver; reads usage-prices.json and usage-params.json; refuses everything attempt refuses, reach null in usage-params.json and not passed with --reach, and a reach outside 0 to 1 with 0 excluded")]
[group('usage')]
[no-exit-message]
unit *arguments:
    python3 -I {{ quote(script) }} unit {{ arguments }}
