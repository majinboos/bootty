# Agent integrations

Bootty manages visible agent sessions. It does not hide an agent behind a
headless RPC subprocess.

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

Every module declares and owns its adapter through
`bootty.integration.register`. Install or remove it from that module's entry in
Settings. Bootty writes the adapter and updates the tool's configuration.

## Pi

The built-in `agents/pi.luau` module starts Pi in the selected visible terminal:

```sh
pi
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

With an explicit target, `start` launches in that visible terminal. Without a
target, it creates a new visible tab first. `prompt`, `steer`, `follow_up`, and
`abort` operate on their selected visible terminal.

Install the Pi extension from the `agents.pi` module in Settings to publish
native events from existing interactive Pi sessions.

A project can use `.pi/extensions/bootty.ts` after Pi trusts that project.

The adapter calls `agents.pi.ingest` through the live Bootty owner, with the
pane Pi runs in as the second argument.

The adapter uses one active publisher and a bounded event queue.

It coalesces `tool_execution_update` events for the same tool call.

It reports any dropped event count through `extension_error`.

## Codex

The built-in `agents/codex.luau` module starts Codex in the selected visible
terminal:

```sh
codex
```

Use these commands through the Bootty CLI or socket:

```sh
bootty command agents.codex.start /path/to/worktree
bootty command agents.codex.prompt "Inspect the failing test"
bootty command agents.codex.steer "Check the persistence path first"
bootty command agents.codex.interrupt --yes
bootty command agents.codex.state
bootty command agents.codex.stop --yes
```

Install the Codex hooks from the `agents.codex` module in Settings. The module
owns both the hook script and the native hook configuration Bootty merges.

The hook reads one native hook JSON object from stdin.

The hook calls `agents.codex.ingest` through the live Bootty owner, with the
pane it ran in as the second argument.

## Claude Code

Claude Code reports through command hooks, like Codex, and can also be started
in the selected visible terminal.

`agents.claude.state` inspects what the hooks reported:

```sh
bootty command agents.claude.state
bootty command agents.claude.state %3
bootty command agents.claude.start /path/to/worktree
```

Install the Claude Code hooks from the `agents.claude` module in Settings. The
module owns both the hook script and the native hook configuration Bootty
merges.

The hook reads one native hook JSON object from stdin and calls
`agents.claude.ingest` through the live Bootty owner.

`Notification` is what tells Bootty that Claude Code is waiting on the person.

## Limits and cleanup

Agent processes are owned by the visible mux pane that launched them. Closing
that pane stops its agent process tree. Reloading an integration does not stop
the interactive session.

Agent-specific JSON schemas, lifecycle rules, and adapter source stay in the
Luau modules.
