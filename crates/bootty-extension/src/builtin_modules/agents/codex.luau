--!strict
-- Codex owns its interactive session. Bootty targets the visible terminal that owns it.

local ui = bootty.ui
local ICON = "openai"
local COLOR = bootty.theme.accent
local panes = {}

bootty.events.register("agents.codex.event")

local function new_state()
	return {
		provider = "codex",
		source = "none",
		status = "stopped",
		thread_id = nil,
		turn_id = nil,
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
	return table.clone(panes[pane] or new_state())
end

local function publish(pane, kind, payload)
	pcall(function()
		bootty.events.publish("agents.codex.event", {
			kind = kind,
			pane = pane,
			state = snapshot(pane),
			payload = payload,
		})
	end)
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

local function apply_hook(state, event)
	state.last_event = event.hook_event_name or "unknown"
	state.thread_id = event.session_id or state.thread_id
	state.turn_id = event.turn_id or state.turn_id
	if event.hook_event_name == "SessionStart" then
		state.status = "idle"
	elseif event.hook_event_name == "UserPromptSubmit" then
		state.status = "working"
	elseif event.hook_event_name == "PreToolUse" then
		state.status = "tool:" .. (event.tool_name or "unknown")
	elseif event.hook_event_name == "Stop" then
		state.status = "idle"
	elseif event.hook_event_name == "SessionEnd" then
		state.status = "stopped"
	end
end

bootty.commands.register({
	id = "agents.codex.start",
	title = "Start Codex",
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
	local program = context.arguments[2] or "codex"
	local command = bootty.shell.quote(program)
	local cwd = context.arguments[1]
	if cwd and cwd ~= "" then
		command = "cd " .. bootty.shell.quote(cwd) .. " && exec " .. command
	else
		command = "exec " .. command
	end
	return submit(context, command)
end)

bootty.commands.register({
	id = "agents.codex.prompt",
	title = "Prompt Codex",
	mutation = "write",
	target = "terminal",
	arguments = { { name = "message", type = "string", required = true } },
}, function(context)
	return submit(context, context.arguments[1])
end)

bootty.commands.register({
	id = "agents.codex.steer",
	title = "Steer Codex",
	mutation = "write",
	target = "terminal",
	arguments = { { name = "message", type = "string", required = true } },
}, function(context)
	return submit(context, context.arguments[1])
end)

bootty.commands.register({
	id = "agents.codex.interrupt",
	title = "Interrupt Codex",
	mutation = "destructive",
	target = "terminal",
}, function(context)
	return write(context, "\003")
end)

bootty.commands.register({
	id = "agents.codex.state",
	title = "Inspect Codex state",
	-- Without a pane this answers for the most recently reported visible session.
	arguments = { { name = "pane", type = "string" } },
}, function(context)
	local pane = context.arguments[1]
	return snapshot(if pane and pane ~= "" then pane else next(panes))
end)

bootty.commands.register({
	id = "agents.codex.stop",
	title = "Stop Codex pane",
	mutation = "destructive",
	target = "pane",
}, function(context)
	invoke({ command = "kill_pane", target = context.target })
	return { stopped = true }
end)

bootty.commands.register({
	id = "agents.codex.ingest",
	title = "Ingest a Codex hook event",
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

bootty.ui.register({
	id = "agents.codex",
	placement = "session",
	order = 901,
	interval = 0.1,
}, function()
	return ui.session_components({
		sessions = bootty.sessions(),
		render = function(ctx)
			local state = if ctx.session.pane_id then panes[ctx.session.pane_id] else nil
			-- Nothing has reported Codex in this session's pane, so the row stays quiet.
			if not state or state.source == "none" then
				return nil
			end
			return {
				details = {
					{
						key = "agent",
						icon = ICON,
						label = "codex",
						color = COLOR,
						right = state.status,
						right_color = if state.status == "idle" then bootty.theme.muted else COLOR,
					},
				},
			}
		end,
	})
end)

-- The adapter Bootty installs for an existing Codex session, declared here so the hook script and
-- the settings it needs stay next to the module that reads its events.
local HOOK = [[#!/bin/sh
set -eu

payload=$(cat)
pane=${TMUX_PANE:-${BOOTTY_PANE:-}}
bootty --json command agents.codex.ingest "$payload" "$pane" >/dev/null 2>&1 || :
printf '{}\n'
]]

local script = bootty.integration.dir .. "/codex/bootty-hook.sh"
local function hook(timeout)
	return { { type = "command", command = script, timeout = timeout } }
end
bootty.integration.register({
	id = "hooks",
	title = "Codex hooks",
	summary = "Reports Codex activity to Bootty.",
	files = {
		{ path = "codex/bootty-hook.sh", contents = HOOK, executable = true },
	},
	merge = {
		{
			path = "~/.codex/hooks.json",
			value = {
				hooks = {
					SessionStart = { { matcher = "startup|resume|clear|compact", hooks = hook(2) } },
					UserPromptSubmit = { { hooks = hook(2) } },
					PreToolUse = { { matcher = ".*", hooks = hook(2) } },
					Stop = { { hooks = hook(2) } },
					SessionEnd = { { hooks = hook(1) } },
				},
			},
		},
	},
})
