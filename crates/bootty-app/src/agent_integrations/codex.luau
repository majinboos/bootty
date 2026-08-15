--!strict
-- Codex owns the app-server and hook schemas. Bootty supplies bounded JSONL transport.

local PROCESS = "codex-app-server"
local state = {
	provider = "codex",
	source = "none",
	status = "stopped",
	thread_id = nil,
	turn_id = nil,
	last_event = nil,
	error = nil,
}
local running = false
local cwd = nil
local next_request = 2

bootty.events.register("agents.codex.event")

local function snapshot()
	return {
		provider = state.provider,
		source = state.source,
		status = state.status,
		thread_id = state.thread_id,
		turn_id = state.turn_id,
		last_event = state.last_event,
		error = state.error,
	}
end

local function publish(kind, payload)
	pcall(function()
		bootty.events.publish("agents.codex.event", {
			kind = kind,
			state = snapshot(),
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

local function apply_hook(event)
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
	publish("app_server", message)
end

local function drain()
	if not running then
		return
	end
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
				publish("protocol_error", { line = event.line })
			end
		elseif event.stream == "stderr" or event.stream == "error" then
			state.error = event.line
			publish(event.stream, { line = event.line })
		elseif event.stream == "exit" then
			running = false
			state.status = "stopped"
			publish("exit", { code = event.code })
		elseif event.stream == "dropped" then
			publish("dropped", { count = event.count })
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
	return snapshot()
end)

bootty.commands.register({
	id = "agents.codex.prompt",
	title = "Prompt managed Codex",
	mutation = "write",
	arguments = { { name = "message", type = "string", required = true } },
}, function(context)
	drain()
	if not state.thread_id then
		error("Codex thread is not ready")
	end
	local id = request("turn/start", {
		threadId = state.thread_id,
		input = { { type = "text", text = context.arguments[1] } },
	})
	state.status = "working"
	return { request_id = id, state = snapshot() }
end)

bootty.commands.register({
	id = "agents.codex.steer",
	title = "Steer managed Codex",
	mutation = "write",
	arguments = { { name = "message", type = "string", required = true } },
}, function(context)
	drain()
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

bootty.commands.register({ id = "agents.codex.state", title = "Inspect Codex state" }, function()
	drain()
	return snapshot()
end)

bootty.commands.register({
	id = "agents.codex.stop",
	title = "Stop managed Codex",
	mutation = "destructive",
}, function()
	bootty.process.stop(PROCESS)
	running = false
	state.status = "stopped"
	return snapshot()
end)

bootty.commands.register({
	id = "agents.codex.ingest",
	title = "Ingest a Codex hook event",
	mutation = "write",
	arguments = { { name = "event", type = "string", required = true } },
}, function(context)
	local event = bootty.json.decode(context.arguments[1])
	state.source = "existing"
	state.error = nil
	apply_hook(event)
	bootty.storage.set("latest", event)
	publish("hook", event)
	return snapshot()
end)

bootty.ui.register({
	id = "agents.codex",
	placement = "sidebar",
	order = 901,
	interval = 0.1,
}, function()
	drain()
	local text = "Codex " .. state.status
	if state.thread_id then
		text ..= " · " .. state.thread_id
	end
	return { { kind = "footer", text = text } }
end)
