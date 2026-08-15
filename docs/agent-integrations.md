# Agent integrations

Bootty uses native agent protocols.

Bootty does not infer agent state from process names, terminal output, screen
content, or transcript files.

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

The adapter calls `agents.pi.ingest` through the live Bootty owner.

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

The hook calls `agents.codex.ingest` through the live Bootty owner.

## Limits and cleanup

Each extension generation owns at most four managed processes.

Each process has bounded input and output queues.

Extension replacement and removal stop the process trees owned by the retired
generation.

Agent-specific JSON schemas and lifecycle rules stay in the Luau modules and
adapter files.
