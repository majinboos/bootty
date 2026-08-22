# Agent integrations

Bootty uses native agent protocols.

Bootty does not infer agent state from process names, terminal output, screen
content, or transcript files. An agent reports, or Bootty shows nothing.

## Which session an event belongs to

An agent runs in a session, so every reported event names the pane it came
from and the sidebar shows one row per session.

Bootty exports `BOOTTY_PANE` into every pane it spawns itself, carrying the
same pane id the mux snapshot reports as `session.pane_id`. tmux exports that
id as `TMUX_PANE` inside its own panes.

Each adapter reads `${TMUX_PANE:-${BOOTTY_PANE:-}}` and passes it as the
second argument of its `ingest` command. An event with no pane lands on no
session row.

Every module declares its adapter through `bootty.integration.register`, so
the files under `integrations/` are the same text Bootty installs.

## Pi

The built-in `agents/pi.luau` module starts managed Pi sessions with:

```sh
pi --mode rpc
```

Use these commands through the Bootty CLI or socket:

```sh
bootty command agents.pi.start /path/to/worktree
bootty command agents.pi.prompt "Inspect the failing test"
bootty command agents.pi.steer "Check the persistence path first"
bootty command agents.pi.follow_up "Run the focused contract"
bootty command agents.pi.abort --yes
bootty command agents.pi.state
bootty command agents.pi.stop --yes
```

Copy `integrations/pi/bootty.ts` to `~/.pi/agent/extensions/bootty.ts` to
publish native events from existing interactive Pi sessions.

A project can use `.pi/extensions/bootty.ts` after Pi trusts that project.

The adapter calls `agents.pi.ingest` through the live Bootty owner, with the
pane Pi runs in as the second argument.

The adapter uses one active publisher and a bounded event queue.

It coalesces `tool_execution_update` events for the same tool call.

It reports any dropped event count through `extension_error`.

## Codex

The built-in `agents/codex.luau` module starts managed Codex sessions with:

```sh
codex app-server --listen stdio://
```

The managed thread uses `approvalPolicy = "never"`.
The managed request fails instead of waiting for an approval response that
Bootty does not own.

Use these commands through the Bootty CLI or socket:

```sh
bootty command agents.codex.start /path/to/worktree
bootty command agents.codex.prompt "Inspect the failing test"
bootty command agents.codex.steer "Check the persistence path first"
bootty command agents.codex.interrupt --yes
bootty command agents.codex.state
bootty command agents.codex.stop --yes
```

Copy `integrations/codex/bootty-hook.sh` to a stable executable path.

Add that path as a command hook for the native Codex events that Bootty must
show.

For example, `~/.codex/hooks.json` can contain:

```json
{
  "hooks": {
    "SessionStart": [{
      "matcher": "startup|resume|clear|compact",
      "hooks": [{"type": "command", "command": "/absolute/path/bootty-hook.sh", "timeout": 2}]
    }],
    "UserPromptSubmit": [{
      "hooks": [{"type": "command", "command": "/absolute/path/bootty-hook.sh", "timeout": 2}]
    }],
    "PreToolUse": [{
      "matcher": ".*",
      "hooks": [{"type": "command", "command": "/absolute/path/bootty-hook.sh", "timeout": 2}]
    }],
    "Stop": [{
      "hooks": [{"type": "command", "command": "/absolute/path/bootty-hook.sh", "timeout": 2}]
    }],
    "SessionEnd": [{
      "hooks": [{"type": "command", "command": "/absolute/path/bootty-hook.sh", "timeout": 1}]
    }]
  }
}
```

The hook reads one native hook JSON object from stdin.

The hook calls `agents.codex.ingest` through the live Bootty owner, with the
pane it ran in as the second argument.

## Claude Code

Claude Code reports through command hooks, like Codex.

Bootty starts no managed Claude Code process; `agents.claude.state` inspects
what the hooks reported:

```sh
bootty command agents.claude.state
bootty command agents.claude.state %3
```

Copy `integrations/claude/bootty-hook.sh` to a stable executable path.

Add that path as a command hook for the native Claude Code events that Bootty
must show.

For example, `~/.claude/settings.json` can contain:

```json
{
  "hooks": {
    "SessionStart": [{
      "matcher": "startup|resume|clear|compact",
      "hooks": [{"type": "command", "command": "/absolute/path/bootty-hook.sh", "timeout": 2}]
    }],
    "UserPromptSubmit": [{
      "hooks": [{"type": "command", "command": "/absolute/path/bootty-hook.sh", "timeout": 2}]
    }],
    "PreToolUse": [{
      "matcher": "*",
      "hooks": [{"type": "command", "command": "/absolute/path/bootty-hook.sh", "timeout": 2}]
    }],
    "PostToolUse": [{
      "matcher": "*",
      "hooks": [{"type": "command", "command": "/absolute/path/bootty-hook.sh", "timeout": 2}]
    }],
    "Notification": [{
      "hooks": [{"type": "command", "command": "/absolute/path/bootty-hook.sh", "timeout": 2}]
    }],
    "Stop": [{
      "hooks": [{"type": "command", "command": "/absolute/path/bootty-hook.sh", "timeout": 2}]
    }],
    "SessionEnd": [{
      "hooks": [{"type": "command", "command": "/absolute/path/bootty-hook.sh", "timeout": 1}]
    }]
  }
}
```

The hook reads one native hook JSON object from stdin and calls
`agents.claude.ingest` through the live Bootty owner.

`Notification` is what tells Bootty that Claude Code is waiting on the person.

## Limits and cleanup

Each extension generation owns at most four managed processes.

Each process has bounded input and output queues.

Extension replacement and removal stop the process trees owned by the retired
generation.

Agent-specific JSON schemas and lifecycle rules stay in the Luau modules and
adapter files.
