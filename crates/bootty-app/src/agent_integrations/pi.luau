--!strict
-- Pi owns the RPC schema. Bootty supplies only bounded JSONL process transport.

local PROCESS = "pi-rpc"
local state = {
	provider = "pi",
	source = "none",
	status = "stopped",
	session_id = nil,
	session_name = nil,
	last_event = nil,
	error = nil,
}
local running = false
local next_request = 0

bootty.events.register("agents.pi.event")

local function snapshot()
	return {
		provider = state.provider,
		source = state.source,
		status = state.status,
		session_id = state.session_id,
		session_name = state.session_name,
		last_event = state.last_event,
		error = state.error,
	}
end

local function publish(kind, payload)
	pcall(function()
		bootty.events.publish("agents.pi.event", {
			kind = kind,
			state = snapshot(),
			payload = payload,
		})
	end)
end

local function apply_native_event(event)
	state.last_event = event.type or event.hook_event_name or "unknown"
	if event.sessionId then
		state.session_id = event.sessionId
	elseif event.session_id then
		state.session_id = event.session_id
	end
	if event.sessionName then
		state.session_name = event.sessionName
	end
	if event.type == "agent_start" or event.type == "turn_start" then
		state.status = "working"
	elseif event.type == "agent_settled" then
		state.status = "idle"
	elseif event.type == "tool_execution_start" then
		state.status = "tool:" .. (event.toolName or "unknown")
	elseif event.type == "extension_error" then
		state.status = "error"
		state.error = event.error or "Pi extension error"
	elseif event.type == "session_shutdown" then
		state.status = "stopped"
	end
end

local function apply_rpc_message(message)
	if message.type == "response" then
		if message.success == false then
			state.status = "error"
			state.error = message.error or "Pi RPC request failed"
		elseif message.command == "get_state" and message.data then
			state.session_id = message.data.sessionId
			state.session_name = message.data.sessionName
			state.status = if message.data.isStreaming then "working" else "idle"
			state.error = nil
		end
	else
		apply_native_event(message)
	end
	publish("rpc", message)
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
				apply_rpc_message(message)
			else
				state.status = "error"
				state.error = "Pi emitted invalid JSONL"
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

local function send(command)
	if not running then
		error("Pi RPC session is not running")
	end
	next_request += 1
	command.id = "bootty-" .. tostring(next_request)
	bootty.process.write(PROCESS, bootty.json.encode(command))
	return command.id
end

bootty.commands.register({
	id = "agents.pi.start",
	title = "Start managed Pi",
	mutation = "write",
	arguments = {
		{ name = "cwd", type = "string" },
		{ name = "program", type = "string" },
	},
}, function(context)
	local cwd = context.arguments[1]
	local program = context.arguments[2] or "pi"
	bootty.process.start({ id = PROCESS, argv = { program, "--mode", "rpc" }, cwd = cwd })
	running = true
	state.source = "managed"
	state.status = "starting"
	state.error = nil
	send({ type = "get_state" })
	return snapshot()
end)

bootty.commands.register({
	id = "agents.pi.prompt",
	title = "Prompt managed Pi",
	mutation = "write",
	arguments = { { name = "message", type = "string", required = true } },
}, function(context)
	drain()
	local id = send({ type = "prompt", message = context.arguments[1] })
	state.status = "working"
	return { request_id = id, state = snapshot() }
end)

for _, command_type in ipairs({ "steer", "follow_up" }) do
	local current = command_type
	bootty.commands.register({
		id = "agents.pi." .. current,
		title = if current == "steer" then "Steer managed Pi" else "Follow up with managed Pi",
		mutation = "write",
		arguments = { { name = "message", type = "string", required = true } },
	}, function(context)
		drain()
		return { request_id = send({ type = current, message = context.arguments[1] }) }
	end)
end

bootty.commands.register({
	id = "agents.pi.abort",
	title = "Abort managed Pi",
	mutation = "destructive",
}, function()
	drain()
	return { request_id = send({ type = "abort" }) }
end)

bootty.commands.register({ id = "agents.pi.state", title = "Inspect Pi state" }, function()
	drain()
	return snapshot()
end)

bootty.commands.register({
	id = "agents.pi.stop",
	title = "Stop managed Pi",
	mutation = "destructive",
}, function()
	bootty.process.stop(PROCESS)
	running = false
	state.status = "stopped"
	return snapshot()
end)

bootty.commands.register({
	id = "agents.pi.ingest",
	title = "Ingest a Pi native event",
	mutation = "write",
	arguments = { { name = "event", type = "string", required = true } },
}, function(context)
	local event = bootty.json.decode(context.arguments[1])
	state.source = "existing"
	state.error = nil
	apply_native_event(event)
	bootty.storage.set("latest", event)
	publish("native", event)
	return snapshot()
end)

bootty.ui.register({ id = "agents.pi", placement = "sidebar", order = 900, interval = 0.1 }, function()
	drain()
	local text = "Pi " .. state.status
	if state.session_name then
		text ..= " · " .. state.session_name
	end
	return { { kind = "footer", text = text } }
end)
