# estafette Claude Code bridge

Delivers estafette inbox messages into a running Claude Code session as notices. It reaches the estafette binary only through its inbox socket.

## Install

1. Build with `cargo build --release -p estafette-bridge-claude` and copy `target/release/estafette-bridge-claude` to a directory on `PATH`.
2. Merge `bridges/claude/hooks/session-start.json` into `~/.claude/settings.json`, or into `.claude/settings.json` of a project. The hook starts `estafette-bridge-claude run "$PPID"` detached at session start and resume. `$PPID` is the Claude Code process, the one the bridge serves and exits with.
3. Leave `crossSessionInbound` unset. `refuse` drops every frame the bridge injects; `accept` would also admit the native messages of every other local session.

The bridge reads `CLAUDE_CODE_MESSAGING_SOCKET` and `CLAUDE_CODE_MESSAGING_TOKEN` from its environment. Claude Code exports both before any hook runs, and the hook passes them on by inheritance, so the token never appears in a process listing.

## What it does

Every 2 seconds the bridge looks under the session's process for `estafette mcp` processes, follows each one's inbox socket, and writes one notice per frame to the session's messaging socket. The notice names the sender as a peer. At most 10 notices per sender in 60 seconds, and no repeat of the previous body from a sender within 30 seconds; the frames that limit drops are counted. The bridge exits when the session ends.

## Doctor

Run it inside the session (the Bash tool, or `!` at the prompt) so the session's environment is present:

```
estafette-bridge-claude doctor [--project-dir <dir>]
```

`--project-dir` names the project whose `.claude/settings.json` and `.claude/settings.local.json` are read; it defaults to the working directory. The user settings come from `$CLAUDE_CONFIG_DIR`, or `~/.claude`.

Checks, one line each on stdout:

- hook installed: a `SessionStart` hook that starts `estafette-bridge-claude` is in the user, project or local settings.
- bridge process runs for this session: a bridge started for a parent process of the doctor is running.
- socket path exists: the path in `CLAUDE_CODE_MESSAGING_SOCKET` exists.
- no refuse applies: none of those settings files sets `crossSessionInbound` to `refuse`. Managed settings and `--settings` are not read.
- binary process found: an `estafette mcp` process runs under the session.

The last line is the count of frames the rate limit dropped. Exit status is 0 when every check passes, 3 when one fails, 1 when the doctor cannot run (the message is on stderr), 2 on a malformed command line.
