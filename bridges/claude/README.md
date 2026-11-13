# estafette Claude Code bridge

Delivers estafette inbox messages into a running Claude Code session as notices. It reaches the estafette binary only through its inbox socket.

## Install

1. Build with `cargo build --release -p estafette-bridge-claude` and copy `target/release/estafette-bridge-claude` to a directory on `PATH`.
2. Merge `bridges/claude/hooks/session-start.json` into `~/.claude/settings.json`, or into `.claude/settings.json` of a project. The hook starts `estafette-bridge-claude run "$PPID"` detached at session start and resume. `$PPID` is the Claude Code process, the one the bridge serves and exits with.
3. Leave `crossSessionInbound` unset. `refuse` drops every frame the bridge injects, `hold` parks each one for the user's approval, and `accept` would also admit the native messages of every other local session.

The bridge reads `CLAUDE_CODE_MESSAGING_SOCKET` and `CLAUDE_CODE_MESSAGING_TOKEN` from its environment. Claude Code exports both before any hook runs, and the hook passes them on by inheritance, so the token never appears in a process listing.

## What it does

Every 2 seconds the bridge looks under the session's process for `estafette mcp` processes, follows each one's inbox socket, and writes one notice per frame to the session's messaging socket. Each frame is written as the line Claude Code 2.1.319 itself writes for a peer send: the frame's body inside a `<cross-session-message from-name="<handle>" from-mode="<class>">` envelope, with the frame's message id as `msg_id` and nothing else added to the content. The class is the session's own permission class, re-read for every frame from the latest `permission-mode` entry of its transcript (found through its entry in the session registry), `bypass` for `bypassPermissions` and `prompting` for every other mode, so a switch of the session's permission mode in place is followed from the next frame and the session accepts the line without a hold; when the transcript has no `permission-mode` entry yet or cannot be read, the bridge attests `prompting` until a class is read, a bypass session holds the frame, and the doctor's attested-class line reads `unknown` because no class was recorded. The harness's own rewrite of `</cross-session-message>` to `<\/cross-session-message>`, with the same lookalike brackets, slashes, hyphens and zero-width fill it matches, is applied to the body, and a sender name over 64 characters is written cut to 64 with an ellipsis, exactly as the harness's envelope rebuild reproduces them; a sender name that rebuild would alter (empty, or anything but visible ASCII without `"`, `<`, `>`) is logged on stderr and its frame is dropped and counted. At most 10 notices per sender in 60 seconds, and no repeat of the previous body from a sender within 30 seconds; the frames that limit drops are counted. The bridge exits when the session ends. A second start for the same session, as a `/resume` causes, exits 0 at once and names the bridge already running.

## Debug trace

Until v1.0.0, `run` writes a debug trace by default, as the hook starts it: one file per run, readable by you alone, named `<start>-<run>-bridge.events.jsonl` under `traces/` in the data directory (`$XDG_DATA_HOME/estafette`, or `~/.local/share/estafette`), the directory `estafette` keeps its own traces in. The first line on stderr names the file. It holds the bridge's typed events: its start, each inbox connection opened, closed and lost, each frame received (message id and frame number), each permission class read and where from, each notice written or not, each frame dropped and why, and an event for each line the bridge writes to stderr, by name without its text. It never holds a message body, sender name, token, socket path or error text, so it is safe to post. `estafette-bridge-claude run --help` states its size bounds. `estafette-bridge-claude run <harness-pid> --no-debug` writes no trace and leaves stderr as it is without one; a trace that cannot start refuses the run, naming why.

## Doctor

Run it inside the session (the Bash tool, or `!` at the prompt) so the session's environment is present:

```
estafette-bridge-claude doctor
```

The working directory names the project whose `.claude/settings.json` and `.claude/settings.local.json` are read. The user settings come from `$CLAUDE_CONFIG_DIR`, or `~/.claude`.

Checks, one line each on stdout:

- hook installed: a `SessionStart` hook whose command is `estafette-bridge-claude run`, optionally after `nohup` and with or without a directory in front of the executable name, is in the user, project or local settings; any other prefix, such as `env`, `exec` or a quoted path, fails the check.
- bridge process runs for this session: a bridge started for a parent process of the doctor is running.
- socket path exists: the path in `CLAUDE_CODE_MESSAGING_SOCKET` exists.
- no refuse applies: none of the settings sources that reach the session sets `crossSessionInbound` to `refuse`, to `hold`, or to any value other than `accept`, `hold` and `refuse`. The sources read are the user, project and local settings files, the managed `managed-settings.json` and `managed-settings.d/*.json` (macOS `/Library/Application Support/ClaudeCode/`, Linux `/etc/claude-code/`), and each `--settings` value on the session's command line, inline or a file. A source that exists but cannot be read, or is not valid JSON, makes the check `UNVERIFIED`, which fails it. MDM profiles and server-managed settings are not read.
- binary process found: an `estafette mcp` process runs under the session.

The last two lines are the permission class the bridge attests (`unknown` when it left none) and the count of frames dropped, by the rate limit or for a refused sender name. Exit status is 0 when every check passes, 3 when one fails or cannot be verified, 1 when the doctor cannot run (the message is on stderr), 2 on a malformed command line.
