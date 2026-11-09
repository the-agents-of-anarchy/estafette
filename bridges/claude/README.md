# estafette Claude Code bridge

Delivers estafette inbox messages into a running Claude Code session as notices. It reaches the estafette binary only through its inbox socket.

## Install

1. Build with `cargo build --release -p estafette-bridge-claude` and copy `target/release/estafette-bridge-claude` to a directory on `PATH`.
2. Merge `bridges/claude/hooks/session-start.json` into `~/.claude/settings.json`, or into `.claude/settings.json` of a project. The hook starts `estafette-bridge-claude run "$PPID"` detached at session start and resume, handing it the hook's input on standard input so the bridge reads the session's `permission_mode`. `$PPID` is the Claude Code process, the one the bridge serves and exits with.
3. Leave `crossSessionInbound` unset. `refuse` drops every frame the bridge injects, `hold` parks each one for the user's approval, and `accept` would also admit the native messages of every other local session.

The bridge reads `CLAUDE_CODE_MESSAGING_SOCKET` and `CLAUDE_CODE_MESSAGING_TOKEN` from its environment. Claude Code exports both before any hook runs, and the hook passes them on by inheritance, so the token never appears in a process listing.

## What it does

Every 2 seconds the bridge looks under the session's process for `estafette mcp` processes, follows each one's inbox socket, and writes one notice per frame to the session's messaging socket. Each frame is written as the line Claude Code 2.1.319 itself writes for a peer send: the frame's body byte for byte inside a `<cross-session-message from-name="<handle>" from-mode="<class>">` envelope, with the frame's message id as `msg_id`, and nothing else added to the content. The class is the session's own permission class from the hook's `permission_mode`, `bypass` for `bypassPermissions` and `prompting` for every other value, so the session accepts the line without a hold; it is fixed when the bridge starts, so switching the session's permission mode in place leaves the bridge attesting the old class until the next start. A sender name the harness's envelope cannot carry (over 64 characters, or holding `"`, `<`, `>` or a newline) is logged on stderr and its frame is dropped and counted. At most 10 notices per sender in 60 seconds, and no repeat of the previous body from a sender within 30 seconds; the frames that limit drops are counted. The bridge exits when the session ends. A second start for the same session, as a `/resume` causes, exits 0 at once and names the bridge already running.

## Doctor

Run it inside the session (the Bash tool, or `!` at the prompt) so the session's environment is present:

```
estafette-bridge-claude doctor
```

The working directory names the project whose `.claude/settings.json` and `.claude/settings.local.json` are read. The user settings come from `$CLAUDE_CONFIG_DIR`, or `~/.claude`.

Checks, one line each on stdout:

- hook installed: a `SessionStart` hook that starts `estafette-bridge-claude` is in the user, project or local settings.
- bridge process runs for this session: a bridge started for a parent process of the doctor is running.
- socket path exists: the path in `CLAUDE_CODE_MESSAGING_SOCKET` exists.
- no refuse applies: none of the settings sources that reach the session sets `crossSessionInbound` to `refuse`, to `hold`, or to any value other than `accept`, `hold` and `refuse`. The sources read are the user, project and local settings files, the managed `managed-settings.json` and `managed-settings.d/*.json` (macOS `/Library/Application Support/ClaudeCode/`, Linux `/etc/claude-code/`), and each `--settings` value on the session's command line, inline or a file. A source that exists but cannot be read, or is not valid JSON, makes the check `UNVERIFIED`, which fails it. MDM profiles and server-managed settings are not read.
- binary process found: an `estafette mcp` process runs under the session.

The last two lines are the permission class the bridge attests (`unknown` when it left none) and the count of frames dropped, by the rate limit or for a refused sender name. Exit status is 0 when every check passes, 3 when one fails or cannot be verified, 1 when the doctor cannot run (the message is on stderr), 2 on a malformed command line.
