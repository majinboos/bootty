--!strict
-- Claude Code owns the hook schema and its event names. Bootty only records what a hook reports.

local ui = bootty.ui
local ICON = "claude"
local COLOR = bootty.theme.warning
-- An agent runs in a session, so state is kept per pane: every hook names the pane it ran in, and
-- the sidebar row for that session reads the pane's state.
local panes = {}
local last_pane = nil

bootty.events.register("agents.claude.event")

local function new_state()
	return {
		provider = "claude",
		source = "none",
		status = "stopped",
		session_id = nil,
		last_event = nil,
		error = nil,
	}
end

local function state_for(pane)
	local state = panes[pane]
	if not state then
		state = new_state()
		panes[pane] = state
	end
	return state
end

local function snapshot(pane)
	local state = if pane then panes[pane] else nil
	return table.clone(state or new_state())
end

local function publish(pane, kind, payload)
	pcall(function()
		bootty.events.publish("agents.claude.event", {
			kind = kind,
			pane = pane,
			state = snapshot(pane),
			payload = payload,
		})
	end)
end

local function apply_hook(state, event)
	state.last_event = event.hook_event_name or "unknown"
	state.session_id = event.session_id or state.session_id
	if event.hook_event_name == "SessionStart" then
		state.status = "idle"
	elseif event.hook_event_name == "UserPromptSubmit" or event.hook_event_name == "PostToolUse" then
		state.status = "working"
	elseif event.hook_event_name == "PreToolUse" then
		state.status = "tool:" .. (event.tool_name or "unknown")
	elseif event.hook_event_name == "Notification" then
		-- Claude notifies when it needs the person: a permission prompt, or an idle input box.
		state.status = "waiting"
	elseif event.hook_event_name == "Stop" then
		state.status = "idle"
	elseif event.hook_event_name == "SessionEnd" then
		state.status = "stopped"
	end
end

local function invoke(invocation)
	local outcome = bootty.commands.invoke(invocation)
	if outcome.status ~= "success" then
		error(outcome.message or ("command failed: " .. tostring(outcome.status)))
	end
	return outcome.value
end

local function write(context, text)
	local invocation = {
		command = "terminal.write",
		arguments = { text },
	}
	if context.target_supplied then
		invocation.target = context.target
	end
	invoke(invocation)
	return { sent = true }
end

local function submit(context, text)
	local invocation = { command = "terminal.paste", arguments = { text } }
	if context.target_supplied then
		invocation.target = context.target
	end
	invoke(invocation)
	invocation = { command = "terminal.submit" }
	if context.target_supplied then
		invocation.target = context.target
	end
	invoke(invocation)
	return { sent = true }
end

bootty.commands.register({
	id = "agents.claude.start",
	title = "Start Claude Code",
	mutation = "write",
	target = "terminal",
	arguments = {
		{ name = "cwd", type = "string" },
		{ name = "program", type = "string" },
	},
}, function(context)
	if not context.target_supplied then
		local created = invoke({ command = "new_tab" }).created
		if not created then
			error("new tab did not return its terminal target")
		end
		context.target = created
		context.target_supplied = true
	end
	local cwd = context.arguments[1]
	local program = context.arguments[2] or "claude"
	local command = bootty.shell.quote(program)
	if cwd and cwd ~= "" then
		command = "cd " .. bootty.shell.quote(cwd) .. " && exec " .. command
	else
		command = "exec " .. command
	end
	return submit(context, command)
end)

for _, command_type in ipairs({ "prompt", "steer", "follow_up" }) do
	local current = command_type
	bootty.commands.register({
		id = "agents.claude." .. current,
		title = "Send text to Claude Code",
		mutation = "write",
		target = "terminal",
		arguments = { { name = "message", type = "string", required = true } },
	}, function(context)
		return submit(context, context.arguments[1])
	end)
end

bootty.commands.register({
	id = "agents.claude.abort",
	title = "Interrupt Claude Code",
	mutation = "destructive",
	target = "terminal",
}, function(context)
	return write(context, "\003")
end)

bootty.commands.register({
	id = "agents.claude.ingest",
	title = "Ingest a Claude Code hook event",
	mutation = "write",
	arguments = {
		{ name = "event", type = "string", required = true },
		-- The pane the hook ran in, so the event lands on the session showing it.
		{ name = "pane", type = "string" },
	},
}, function(context)
	local event = bootty.json.decode(context.arguments[1])
	local reported = context.arguments[2]
	local pane = if reported and reported ~= "" then reported else "unknown"
	local state = state_for(pane)
	state.source = "existing"
	state.error = nil
	last_pane = pane
	apply_hook(state, event)
	bootty.storage.set("latest", event)
	publish(pane, "hook", event)
	local result = snapshot(pane)
	-- A finished session has nothing left to show, and keeping it would grow one entry per pane
	-- bootty ever hosted.
	if state.status == "stopped" then
		panes[pane] = nil
	end
	return result
end)

bootty.commands.register({
	id = "agents.claude.state",
	title = "Inspect Claude Code state",
	-- Without a pane this answers for whichever pane reported last.
	arguments = { { name = "pane", type = "string" } },
}, function(context)
	local pane = context.arguments[1]
	return snapshot(if pane and pane ~= "" then pane else last_pane)
end)

bootty.ui.register({
	id = "agents.claude",
	placement = "session",
	order = 902,
	interval = 0.1,
}, function()
	return ui.session_components({
		sessions = bootty.sessions(),
		render = function(ctx)
			local state = if ctx.session.pane_id then panes[ctx.session.pane_id] else nil
			-- Nothing has reported Claude Code in this session's pane, so the row stays quiet.
			if not state or state.source == "none" then
				return nil
			end
			return {
				details = {
					{
						key = "agent",
						icon = ICON,
						label = "claude",
						color = COLOR,
						right = state.status,
						right_color = if state.status == "idle" then bootty.theme.muted else COLOR,
					},
				},
			}
		end,
	})
end)

-- The adapter Bootty installs for an existing Claude Code session, declared here so the hook script
-- and the settings it needs stay next to the module that reads its events.
local HOOK = [[#!/bin/sh
set -eu

payload=$(cat)
pane=${TMUX_PANE:-${BOOTTY_PANE:-}}
bootty --json command agents.claude.ingest "$payload" "$pane" >/dev/null 2>&1 || :
printf '{}\n'
]]

local script = bootty.integration.dir .. "/claude/bootty-hook.sh"
local function hook(timeout)
	return { { type = "command", command = script, timeout = timeout } }
end
bootty.integration.register({
	id = "hooks",
	title = "Claude Code hooks",
	summary = "Reports Claude Code activity to Bootty.",
	files = {
		{ path = "claude/bootty-hook.sh", contents = HOOK, executable = true },
	},
	merge = {
		{
			path = "~/.claude/settings.json",
			value = {
				hooks = {
					SessionStart = { { matcher = "startup|resume|clear|compact", hooks = hook(2) } },
					UserPromptSubmit = { { hooks = hook(2) } },
					PreToolUse = { { matcher = "*", hooks = hook(2) } },
					PostToolUse = { { matcher = "*", hooks = hook(2) } },
					Notification = { { hooks = hook(2) } },
					Stop = { { hooks = hook(2) } },
					SessionEnd = { { hooks = hook(1) } },
				},
			},
		},
	},
})
