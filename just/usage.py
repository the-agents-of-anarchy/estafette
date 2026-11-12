"""Catch Claude Code's account limit figures from the status line input and report them, and list the agents in flight with their cost."""
import argparse
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

SCRIPT_DIRECTORY = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIRECTORY.parent
PRICES_PATH = SCRIPT_DIRECTORY / 'usage-prices.json'
SCRATCH_ROOT_VARIABLE = 'ESTAFETTE_SCRATCH_ROOT'
SCRATCH_ROOT_BELOW_CHECKOUT = Path('docs') / 'orchestration_log' / 'recon'
CONFIG_DIRECTORY_VARIABLE = 'CLAUDE_CONFIG_DIR'
DEFAULT_CONFIG_DIRECTORY_NAME = '.claude'
LIMITS_FILE_NAME = 'usage-limits.json'
LIMIT_WINDOWS = ('five_hour', 'seven_day')
LIMIT_FIELDS = ('used_percentage', 'resets_at')
GIT_FILE_POINTER_PREFIX = 'gitdir:'
LEVELS_FROM_WORKTREE_GIT_DIRECTORY_TO_CHECKOUT = 3
SECONDS_PER_MINUTE = 60
DEFAULT_WINDOW_MINUTES = 30
NAME_COLUMN_WIDTH = 60
TOKENS_PER_THOUSAND = 1000
TOKENS_PER_PRICE_UNIT = 1_000_000
LONG_PROMPT_TOKENS = 100_000
LONG_PROMPT_RATES_KEY = 'over_100k_prompt'
PROMPT_KINDS = ('input_tokens', 'cache_creation_input_tokens', 'cache_read_input_tokens')
TOKEN_KINDS = ('input_tokens', 'output_tokens', 'cache_creation_input_tokens', 'cache_read_input_tokens')
COST_KINDS = ('fresh', 'cache_write', 'cache_read', 'output')
UNPRICED_COLUMN = '?'


def login_directory(environment, home_directory):
    """The directory of the Claude Code login this process runs under."""
    configured = environment.get(CONFIG_DIRECTORY_VARIABLE)
    return Path(configured) if configured else home_directory / DEFAULT_CONFIG_DIRECTORY_NAME


def root_checkout(repository_root):
    """The root checkout of the repository, found through the pointer file a worktree holds in place of a git directory."""
    git_entry = repository_root / '.git'
    if not git_entry.is_file():
        return repository_root
    worktree_git_directory = Path(git_entry.read_text().removeprefix(GIT_FILE_POINTER_PREFIX).strip())
    return worktree_git_directory.parents[LEVELS_FROM_WORKTREE_GIT_DIRECTORY_TO_CHECKOUT - 1]


def limits_file_path(environment, repository_root):
    """The one file the limit figures are written to, the same whichever checkout runs."""
    scratch_root = environment.get(SCRATCH_ROOT_VARIABLE)
    return (Path(scratch_root) if scratch_root else root_checkout(repository_root) / SCRATCH_ROOT_BELOW_CHECKOUT) / LIMITS_FILE_NAME


def window_figures(window):
    """The used percentage and reset time of one window, or None when the window is absent."""
    if not isinstance(window, dict):
        return None
    return {field: window.get(field) for field in LIMIT_FIELDS}


def limit_figures(status_input):
    """Both windows' figures from a status line input, or None when the input carries no rate_limits."""
    rate_limits = status_input.get('rate_limits') if isinstance(status_input, dict) else None
    if not isinstance(rate_limits, dict):
        return None
    return {name: window_figures(rate_limits.get(name)) for name in LIMIT_WINDOWS}


def write_limits_file(path, figures, now):
    """Write the figures and the time of writing by a temporary file renamed over the final name."""
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary_path = path.with_name(f'{path.name}.tmp-{os.getpid()}')
    temporary_path.write_text(json.dumps({'written_at': int(now), 'rate_limits': figures}))
    os.replace(temporary_path, path)


def login_status_line_command(login):
    """The status line command the login already sets, or None when it sets none or sets this script."""
    try:
        settings = json.loads((login / 'settings.json').read_text())
    except (OSError, ValueError):
        return None
    command = (settings.get('statusLine') or {}).get('command')
    if not isinstance(command, str) or Path(__file__).name in command:
        return None
    return command


def own_status_line(figures):
    """One line naming the used percentage of each window, for a login with no status line command."""
    if figures is None:
        return 'usage: no rate limits'
    parts = [f'{name} {window["used_percentage"]}%' for name, window in figures.items() if window]
    return 'usage: ' + (' '.join(parts) or 'no rate limits')


def capture(raw_input, now, limits_path, login):
    """Write the limit figures from the status line input, then print the login's own status line unchanged."""
    figures = None
    try:
        figures = limit_figures(json.loads(raw_input))
        write_limits_file(limits_path, figures, now)
    except (OSError, ValueError) as failure:
        print(f'usage capture failed: {failure}', file=sys.stderr)
    command = login_status_line_command(login)
    if command is None:
        print(own_status_line(figures))
        return 0
    forwarded = subprocess.run(command, shell=True, input=raw_input, capture_output=True, check=False)
    sys.stdout.buffer.write(forwarded.stdout)
    sys.stdout.buffer.flush()
    return forwarded.returncode


def limits(max_age_minutes, now, limits_path):
    """Print the last written figures and their age; return 1 when the file is missing, unreadable or older than the bound."""
    if not limits_path.exists():
        print(f'no limit figures written: {limits_path} is missing', file=sys.stderr)
        return 1
    try:
        written = json.loads(limits_path.read_text())
        age_minutes = (now - written['written_at']) / SECONDS_PER_MINUTE
        windows = written['rate_limits']
    except (ValueError, KeyError, TypeError) as failure:
        print(f'limit figures unreadable: {limits_path}: {failure!r}', file=sys.stderr)
        return 1
    print(f'written {age_minutes:.1f} min ago')
    if windows is None:
        print('rate_limits absent from the status line input')
    for name, window in (windows or {}).items():
        print(f'{name}: ' + ('absent' if window is None else f'{window["used_percentage"]}% resets_at {window["resets_at"]}'))
    return 1 if age_minutes > max_age_minutes else 0


def newest_session_subagents_directory(login, checkout):
    """The subagents directory of the checkout's most recently written session."""
    project_directory = login / 'projects' / re.sub(r'[^A-Za-z0-9]', '-', str(checkout))
    sessions = list(project_directory.glob('*.jsonl'))
    if not sessions:
        raise SystemExit(f'no session transcript under {project_directory}')
    return project_directory / max(sessions, key=lambda session: session.stat().st_mtime).stem / 'subagents'


def usage_by_message(transcript_path):
    """Each model message's model and usage, once per message id."""
    seen = {}
    with open(transcript_path, errors='replace') as lines:
        for line in lines:
            if '"usage"' not in line:
                continue
            try:
                message = json.loads(line).get('message') or {}
            except ValueError:
                continue
            usage = message.get('usage')
            if usage and message.get('model') != '<synthetic>':
                seen[message.get('id') or len(seen)] = (message.get('model', '?'), usage)
    return seen


def agent_name(transcript_path):
    """The agent's name from its meta file, empty when the file is unreadable."""
    meta_path = transcript_path.with_suffix('.meta.json')
    try:
        meta = json.loads(meta_path.read_text())
    except (OSError, ValueError):
        return ''
    return str(meta.get('name') or meta.get('description') or meta.get('agentType') or '')


def token_count(usage, kind):
    """The tokens of one kind in a usage record, zero when absent."""
    return int(usage.get(kind) or 0)


def rates_for(prices, model, prompt_tokens):
    """The price per million tokens of the model at this prompt length, or None when the model is not listed."""
    rates = prices['models'].get(prices['alias'].get(model, model))
    if rates is None:
        return None
    return rates.get(LONG_PROMPT_RATES_KEY, rates) if prompt_tokens > LONG_PROMPT_TOKENS else rates


def message_cost(prices, model, usage):
    """Dollars by kind for one message, or None when the model is not listed."""
    fresh = token_count(usage, 'input_tokens')
    cache_write = token_count(usage, 'cache_creation_input_tokens')
    cache_read = token_count(usage, 'cache_read_input_tokens')
    rates = rates_for(prices, model, fresh + cache_write + cache_read)
    if rates is None:
        return None
    write_one_hour = token_count(usage.get('cache_creation') or {}, 'ephemeral_1h_input_tokens')
    write_five_minutes = cache_write - write_one_hour
    dollars = {
        'fresh': fresh * rates['input'],
        'cache_write': write_five_minutes * rates['cache_write_5m'] + write_one_hour * rates['cache_write_1h'],
        'cache_read': cache_read * rates['cache_read'],
        'output': token_count(usage, 'output_tokens') * rates['output'],
    }
    return {kind: amount / TOKENS_PER_PRICE_UNIT for kind, amount in dollars.items()}


def cost_by_kind(prices, messages):
    """Dollars by kind summed over the messages, or None when any message's model is not listed."""
    costs = [message_cost(prices, model, usage) for model, usage in messages.values()]
    if None in costs:
        return None
    return {kind: sum(cost[kind] for cost in costs) for kind in COST_KINDS}


def cold_resume_cost(prices, messages):
    """Dollars to write the agent's last context into the cache again, or None when the model is not listed."""
    if not messages:
        return 0.0
    model, usage = list(messages.values())[-1]
    context = sum(token_count(usage, kind) for kind in PROMPT_KINDS)
    rates = rates_for(prices, model, context)
    return None if rates is None else context * rates['cache_write_1h'] / TOKENS_PER_PRICE_UNIT


def dollars_column(amount):
    """One dollar figure for a table column, or the unpriced mark."""
    return f'{UNPRICED_COLUMN:>9}' if amount is None else f'{amount:9.2f}'


def inflight(window_minutes, subagents_directory, now, prices):
    """Print one row per agent whose transcript changed within the window."""
    rows = []
    for transcript_path in subagents_directory.glob('agent-*.jsonl'):
        idle_minutes = (now - transcript_path.stat().st_mtime) / SECONDS_PER_MINUTE
        if idle_minutes > window_minutes:
            continue
        messages = usage_by_message(transcript_path)
        totals = {kind: sum(token_count(usage, kind) for _, usage in messages.values()) for kind in TOKEN_KINDS}
        models = sorted({model for model, _ in messages.values()})
        fresh = totals['input_tokens'] + totals['output_tokens'] + totals['cache_creation_input_tokens']
        last_usage = list(messages.values())[-1][1] if messages else {}
        context = sum(token_count(last_usage, kind) for kind in PROMPT_KINDS)
        costs = cost_by_kind(prices, messages)
        rows.append((idle_minutes, fresh, totals['cache_read_input_tokens'], context, costs, cold_resume_cost(prices, messages), ','.join(models), agent_name(transcript_path)[:NAME_COLUMN_WIDTH]))
    print('idle_min | fresh_k | cache_read_k | context_k | usd_fresh | usd_write | usd_read | usd_out | usd_total | usd_resume_1h | model | agent')
    for idle_minutes, fresh, cache_read, context, costs, resume, models, name in sorted(rows, key=lambda row: row[0]):
        by_kind = [dollars_column(None if costs is None else costs[kind]) for kind in COST_KINDS]
        total = dollars_column(None if costs is None else sum(costs.values()))
        print(f'{idle_minutes:7.0f} | {fresh // TOKENS_PER_THOUSAND:7d} | {cache_read // TOKENS_PER_THOUSAND:12d} | {context // TOKENS_PER_THOUSAND:9d} | ' + ' | '.join(by_kind) + f' | {total} | {dollars_column(resume)} | {models} | {name}')
    print(f'context_k of all rows: {sum(row[3] for row in rows) // TOKENS_PER_THOUSAND}')
    return 0


def main(arguments, standard_input, now, environment, home_directory):
    """Run the subcommand named in the arguments and return the process exit code."""
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest='subcommand', required=True)
    subcommands.add_parser('capture', help='status line command: write the limit figures from standard input, print the login status line')
    limits_parser = subcommands.add_parser('limits', help='print the last written limit figures; exit 1 when missing or too old')
    limits_parser.add_argument('max_age_minutes', type=float)
    inflight_parser = subcommands.add_parser('inflight', help='print the agents whose transcript changed within the window')
    inflight_parser.add_argument('window_minutes', type=int, nargs='?', default=DEFAULT_WINDOW_MINUTES)
    inflight_parser.add_argument('subagents_directory', nargs='?', default='')
    parsed = parser.parse_args(arguments)
    login = login_directory(environment, home_directory)
    limits_path = limits_file_path(environment, REPOSITORY_ROOT)
    if parsed.subcommand == 'capture':
        return capture(standard_input.buffer.read(), now, limits_path, login)
    if parsed.subcommand == 'limits':
        return limits(parsed.max_age_minutes, now, limits_path)
    subagents_directory = Path(parsed.subagents_directory) if parsed.subagents_directory else newest_session_subagents_directory(login, root_checkout(REPOSITORY_ROOT))
    return inflight(parsed.window_minutes, subagents_directory, now, json.loads(PRICES_PATH.read_text()))


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:], sys.stdin, time.time(), os.environ, Path.home()))
