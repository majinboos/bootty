--!strict
-- Codex owns the app-server and hook schemas. Bootty supplies bounded JSONL transport.

local ui = bootty.ui
local PROCESS = "codex-app-server"
local ICON = "openai"
local COLOR = bootty.theme.accent
-- An agent runs in a session, so state is kept per pane: every reported event names the pane it
-- came from, and the sidebar row for that session reads the pane's state. The app-server bootty
-- starts itself belongs to no pane and keeps its own key.
local MANAGED_PANE = "managed"
local panes = {}
local running = false
local cwd = nil
local next_request = 2

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

local function write(message)
	if not running then
		error("Codex app-server is not running")
	end
	bootty.process.write(PROCESS, bootty.json.encode(message))
end

local function request(method, params)
	local id = next_request
	next_request += 1
	write({ id = id, method = method, params = params or {} })
	return id
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

local function apply_app_server(message)
	local state = state_for(MANAGED_PANE)
	if message.id == 0 and message.result then
		write({ method = "initialized", params = {} })
		write({
			id = 1,
			method = "thread/start",
			params = { cwd = cwd, ephemeral = true, approvalPolicy = "never" },
		})
		state.status = "starting_thread"
	elseif message.id == 1 and message.result and message.result.thread then
		state.thread_id = message.result.thread.id
		state.status = "idle"
	elseif message.error then
		state.status = "error"
		state.error = message.error.message or bootty.json.encode(message.error)
	elseif message.method then
		state.last_event = message.method
		local params = message.params or {}
		if params.threadId then
			state.thread_id = params.threadId
		end
		if message.method == "turn/started" then
			state.turn_id = if params.turn then params.turn.id else state.turn_id
			state.status = "working"
		elseif message.method == "turn/completed" then
			state.turn_id = if params.turn then params.turn.id else state.turn_id
			local status = if params.turn then params.turn.status else nil
			state.status = status or "idle"
		elseif message.method == "thread/closed" then
			state.status = "stopped"
		end
	end
	publish(MANAGED_PANE, "app_server", message)
end

local function drain()
	if not running then
		return
	end
	local state = state_for(MANAGED_PANE)
	local ok, events = pcall(function()
		return bootty.process.poll(PROCESS, 64)
	end)
	if not ok then
		running = false
		state.status = "error"
		state.error = tostring(events)
		return
	end
	for _, event in ipairs(events) do
		if event.stream == "stdout" then
			local decoded, message = pcall(function()
				return bootty.json.decode(event.line)
			end)
			if decoded then
				apply_app_server(message)
			else
				state.status = "error"
				state.error = "Codex app-server emitted invalid JSONL"
				publish(MANAGED_PANE, "protocol_error", { line = event.line })
			end
		elseif event.stream == "stderr" or event.stream == "error" then
			state.error = event.line
			publish(MANAGED_PANE, event.stream, { line = event.line })
		elseif event.stream == "exit" then
			running = false
			state.status = "stopped"
			publish(MANAGED_PANE, "exit", { code = event.code })
		elseif event.stream == "dropped" then
			publish(MANAGED_PANE, "dropped", { count = event.count })
		end
	end
end

bootty.commands.register({
	id = "agents.codex.start",
	title = "Start managed Codex",
	mutation = "write",
	arguments = {
		{ name = "cwd", type = "string" },
		{ name = "program", type = "string" },
	},
}, function(context)
	cwd = context.arguments[1]
	local program = context.arguments[2] or "codex"
	bootty.process.start({
		id = PROCESS,
		argv = { program, "app-server", "--listen", "stdio://" },
		cwd = cwd,
	})
	running = true
	local state = state_for(MANAGED_PANE)
	state.source = "managed"
	state.status = "initializing"
	state.error = nil
	write({
		id = 0,
		method = "initialize",
		params = {
			clientInfo = { name = "bootty", title = "Bootty Codex adapter", version = "0.0.1" },
			capabilities = { experimentalApi = true },
		},
	})
	return snapshot(MANAGED_PANE)
end)

bootty.commands.register({
	id = "agents.codex.prompt",
	title = "Prompt managed Codex",
	mutation = "write",
	arguments = { { name = "message", type = "string", required = true } },
}, function(context)
	drain()
	local state = state_for(MANAGED_PANE)
	if not state.thread_id then
		error("Codex thread is not ready")
	end
	local id = request("turn/start", {
		threadId = state.thread_id,
		input = { { type = "text", text = context.arguments[1] } },
	})
	state.status = "working"
	return { request_id = id, state = snapshot(MANAGED_PANE) }
end)

bootty.commands.register({
	id = "agents.codex.steer",
	title = "Steer managed Codex",
	mutation = "write",
	arguments = { { name = "message", type = "string", required = true } },
}, function(context)
	drain()
	local state = state_for(MANAGED_PANE)
	if not state.thread_id or not state.turn_id then
		error("Codex turn is not active")
	end
	return {
		request_id = request("turn/steer", {
			threadId = state.thread_id,
			expectedTurnId = state.turn_id,
			input = { { type = "text", text = context.arguments[1] } },
		}),
	}
end)

bootty.commands.register({
	id = "agents.codex.interrupt",
	title = "Interrupt managed Codex",
	mutation = "destructive",
}, function()
	drain()
	local state = state_for(MANAGED_PANE)
	if not state.thread_id or not state.turn_id then
		error("Codex turn is not active")
	end
	return {
		request_id = request("turn/interrupt", {
			threadId = state.thread_id,
			turnId = state.turn_id,
		}),
	}
end)

bootty.commands.register({
	id = "agents.codex.state",
	title = "Inspect Codex state",
	-- Without a pane this answers for the app-server bootty manages itself.
	arguments = { { name = "pane", type = "string" } },
}, function(context)
	drain()
	local pane = context.arguments[1]
	return snapshot(if pane and pane ~= "" then pane else MANAGED_PANE)
end)

bootty.commands.register({
	id = "agents.codex.stop",
	title = "Stop managed Codex",
	mutation = "destructive",
}, function()
	bootty.process.stop(PROCESS)
	running = false
	state_for(MANAGED_PANE).status = "stopped"
	return snapshot(MANAGED_PANE)
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
	local pane = if reported and reported ~= "" then reported else MANAGED_PANE
	local state = state_for(pane)
	state.source = "existing"
	state.error = nil
	apply_hook(state, event)
	bootty.storage.set("latest", event)
	publish(pane, "hook", event)
	local result = snapshot(pane)
	-- A finished session has nothing left to show, and keeping it would grow one entry per pane
	-- bootty ever hosted.
	if pane ~= MANAGED_PANE and state.status == "stopped" then
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
	drain()
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
